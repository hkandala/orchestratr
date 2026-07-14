//! M3 completion & logs e2e against **live herdr** + the mock provider (spec M3 acceptance).
//!
//! Gated behind `ORCR_E2E=1` so unit runs stay fast. Every test runs a real `orcr` server
//! over a throwaway `ORCR_HOME` whose config points at a **disposable** herdr session named
//! `orcr_test_<rand>`, torn down by a drop guard. The user's `default` session is never
//! touched. The mock provider drives turn shape via `@`-directives embedded in the prompt
//! (`@turn_ms`, `@tool_gaps`, `@gap_ms`, `@block`).
//!
//! Run with:  `ORCR_E2E=1 cargo test --test completion_e2e -- --test-threads=1 --nocapture`

use orchestratr::driver::{HerdrBinary, HerdrDriver};
use orchestratr::home::Home;
use orchestratr::server::Client;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").as_deref() == Ok("1")
}
fn mock_agent_bin() -> String {
    env!("CARGO_BIN_EXE_orcr-mock-agent").to_string()
}
fn orcr_bin() -> String {
    env!("CARGO_BIN_EXE_orcr").to_string()
}

/// Short mock completion tuning so the suite runs fast (idle-stable 700ms; no transcript).
const MOCK_TUNING: &str = r#""integrations":{"mock":{"fast_turn_grace_ms":2500,"idle_stable_ms":700,"transcript_settle_ms":0,"shutdown_grace_ms":200}}"#;

struct TestServer {
    home: tempfile::TempDir,
    bin: HerdrBinary,
    session: String,
    driver: HerdrDriver,
    extra_env: Vec<(String, String)>,
}

impl TestServer {
    fn start() -> TestServer {
        TestServer::start_with_env(&[])
    }

    fn start_with_env(extra_env: &[(&str, &str)]) -> TestServer {
        let home = tempfile::tempdir().expect("home");
        let bin = HerdrBinary::discover(None).expect("herdr on PATH");
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let session = format!("orcr_test_{}", &rand[..12]);
        let driver = match (|| {
            let sock = bin.ensure_session(&session)?;
            HerdrDriver::connect(&sock)
        })() {
            Ok(d) => d,
            Err(e) => {
                let _ = bin.session_stop(&session);
                let _ = bin.session_delete(&session);
                panic!("disposable session bootstrap failed: {e}");
            }
        };
        std::fs::write(
            home.path().join("config.json"),
            format!(
                r#"{{"herdr":{{"session":"{session}"}},"concurrency":{{"max":5}},{MOCK_TUNING}}}"#
            ),
        )
        .unwrap();
        let ts = TestServer {
            home,
            bin,
            session,
            driver,
            extra_env: extra_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        ts.spawn_server();
        ts
    }

    fn spawn_server(&self) {
        let mut cmd = Command::new(orcr_bin());
        cmd.args(["server", "start"])
            .env("ORCR_HOME", self.home.path())
            .env("ORCR_HERDR_SESSION", &self.session)
            .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
            .env("ORCR_DISABLE_DISCOVERY", "1")
            .env("ORCR_MOCK_AGENT_BIN", mock_agent_bin())
            .stdin(Stdio::null());
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("orcr server start");
        assert!(
            out.status.success(),
            "server start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.client()
            .wait_for_ready(Duration::from_secs(10))
            .expect("server ready");
    }

    fn client(&self) -> Client {
        Client::new(Home::at(self.home.path()).socket_path())
    }
    fn request(&self, method: &str, params: Value) -> orchestratr::Result<Value> {
        self.client().request(method, params)
    }
    fn pid(&self) -> u32 {
        self.client().handshake().unwrap()["pid"].as_u64().unwrap() as u32
    }

    /// Run a mock agent at `path` with an optional prompt and gc mode; returns its uuid.
    fn run(&self, path: &str, prompt: Option<&str>, gc: Option<&str>) -> String {
        let mut p = json!({ "path": path, "agent": "mock" });
        if let Some(pr) = prompt {
            p["prompt"] = json!(pr);
        }
        if let Some(g) = gc {
            p["gc"] = json!(g);
        }
        let r = self.request("agent.run", p).expect("agent.run");
        r["agent"]["uuid"].as_str().unwrap().to_string()
    }

    fn wait(&self, targets: &[&str], timeout: &str) -> Value {
        self.request(
            "agent.wait",
            json!({ "targets": targets, "timeout": timeout }),
        )
        .expect("agent.wait")
    }

    fn status(&self, uuid: &str) -> Option<String> {
        let r = self.request("agent.ls", json!({ "all": true })).ok()?;
        r["agents"]
            .as_array()?
            .iter()
            .find(|a| a["uuid"] == json!(uuid))
            .and_then(|a| a["status"].as_str().map(String::from))
    }
    fn wait_status(&self, uuid: &str, want: &str, timeout: Duration) -> bool {
        wait_until(timeout, || self.status(uuid).as_deref() == Some(want))
    }

    /// The herdr pane id of the agent at `leaf` (its name), for direct external injection.
    fn pane_of(&self, leaf: &str) -> Option<String> {
        self.driver
            .pane_list(None)
            .ok()?
            .into_iter()
            .find(|p| p.label.as_deref() == Some(leaf))
            .map(|p| p.pane_id)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.request("server.stop", json!({}));
        for _ in 0..20 {
            match self.client().handshake() {
                Ok(v) => {
                    if let Some(pid) = v["pid"].as_u64() {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
        let _ = self.bin.session_stop(&self.session);
        let _ = self.bin.session_delete(&self.session);
        assert_no_session_leak(&self.bin, &self.session);
    }
}

/// Known-issues #1: after teardown neither this test's disposable session nor the shared
/// `orcr` session (default `herdr.session`, only ever created by a leaked bootstrap) may
/// survive. Skipped mid-panic so a real failure isn't masked by a double panic (abort).
fn assert_no_session_leak(bin: &HerdrBinary, session: &str) {
    if std::thread::panicking() {
        return;
    }
    // The disposable session is the real per-test guarantee: every server + loop-run child pins
    // ORCR_HERDR_SESSION + ORCR_HOME, so a leaked child can only ever create *this* session.
    assert!(
        matches!(bin.find_session(session), Ok(None)),
        "disposable session `{session}` leaked after teardown"
    );
    // Belt-and-suspenders (known-issues #1): a test must never bootstrap the shared `orcr`
    // session. Skipped when an external `orcr` session pre-existed the run (a developer using
    // orcr concurrently on the same box — its default session is literally `orcr`); that is not
    // a leak this suite produced, and we must not delete an in-use session to satisfy the check.
    if !orcr_session_preexisted(bin) {
        assert!(
            matches!(bin.find_session("orcr"), Ok(None)),
            "shared `orcr` herdr session leaked (a child bootstrapped the default session)"
        );
    }
}

/// Whether an `orcr` herdr session already existed when this test binary first probed (captured
/// once). Used to skip the shared-session leak check when a developer is running orcr
/// concurrently, without weakening the per-test disposable-session guarantee.
fn orcr_session_preexisted(bin: &HerdrBinary) -> bool {
    static SEEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| matches!(bin.find_session("orcr"), Ok(Some(_))))
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// One target's row out of a wait result, by path.
fn target<'a>(waited: &'a Value, path: &str) -> &'a Value {
    waited["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["path"] == json!(path))
        .unwrap_or_else(|| panic!("no wait target for {path}"))
}

/// Fast turn (< grace): a quick prompt completes and `wait` settles turn_complete.
#[test]
fn e2e_fast_turn_completes() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run("fast/worker", Some("@turn_ms=0 hi"), None);
    let waited = ts.wait(&["fast/worker"], "20s");
    let t = target(&waited, "fast/worker");
    assert_eq!(
        t["reason"],
        json!("turn_complete"),
        "fast turn should complete"
    );
    assert_eq!(t["ok"], json!(true));
    assert_eq!(waited["all_ok"], json!(true));
    assert!(waited["decision_seq"].as_i64().unwrap() > 0);
    // next hint points at the last response.
    assert_eq!(t["next"]["kind"], json!("logs_last_response"));
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Slow, tool-heavy turn: brief idle gaps (< settle window) must NOT settle a completion;
/// the turn only completes after the real, stable idle.
#[test]
fn e2e_slow_tool_heavy_turn() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run(
        "tool/heavy",
        Some("@turn_ms=300 @tool_gaps=2 @gap_ms=500 do work"),
        None,
    );
    // A short wait mid-turn must not be satisfied by a brief tool-gap idle.
    let mid = ts.wait(&["tool/heavy"], "600ms");
    assert_eq!(mid["timed_out"], json!(true), "mid-turn wait must time out");
    assert_ne!(
        target(&mid, "tool/heavy")["reason"],
        json!("turn_complete"),
        "an idle gap shorter than the settle window must not complete the turn"
    );
    // The real completion settles.
    let done = ts.wait(&["tool/heavy"], "20s");
    assert_eq!(
        target(&done, "tool/heavy")["reason"],
        json!("turn_complete")
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Blocked mid-turn → wait reports blocked (not ok); a subsequent send clears it and the
/// next turn completes.
#[test]
fn e2e_blocked_then_send_clears() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run("blk/worker", Some("@block please decide"), None);
    assert!(
        ts.wait_status(&uuid, "blocked", Duration::from_secs(20)),
        "agent should become blocked"
    );
    let waited = ts.wait(&["blk/worker"], "5s");
    let t = target(&waited, "blk/worker");
    assert!(t["reason"].as_str().unwrap().starts_with("blocked"));
    assert_eq!(t["ok"], json!(false));
    assert_eq!(t["next"]["kind"], json!("attach"));

    // send clears the block and starts a fresh turn that completes.
    ts.request(
        "agent.send",
        json!({ "target": "blk/worker", "prompt": "@turn_ms=0 continue" }),
    )
    .unwrap();
    let done = ts.wait(&["blk/worker"], "20s");
    assert_eq!(
        target(&done, "blk/worker")["reason"],
        json!("turn_complete")
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// External input (typed into the pane via herdr directly) → synthetic turn recorded → a
/// subsequent orcr `wait` settles on the external turn.
#[test]
fn e2e_external_input_synthetic_turn() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // No prompt → the agent primes and settles to `idle` with no open turn (§5.6).
    let uuid = ts.run("ext/worker", None, None);
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));

    // Type directly into the pane via herdr (bypassing orcr) — a working transition orcr
    // didn't deliver → synthetic external turn.
    // A non-trivial turn so the monitor observes the working→idle transition (the signal for
    // a synthetic external turn).
    let pane = ts.pane_of("ext/worker").expect("agent pane");
    ts.driver
        .pane_send_text(&pane, "@turn_ms=1500 external work")
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    ts.driver.pane_send_keys(&pane, &["Enter"]).unwrap();

    // The external turn completes and a fresh wait settles on it.
    let done = ts.wait(&["ext/worker"], "20s");
    assert_eq!(
        target(&done, "ext/worker")["reason"],
        json!("turn_complete")
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Two consecutive sends: the second `wait` is never satisfied by the first idle.
#[test]
fn e2e_two_sends_no_stale_idle() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run("seq/worker", Some("@turn_ms=0 first"), None);
    // First turn completes.
    assert_eq!(
        target(&ts.wait(&["seq/worker"], "20s"), "seq/worker")["reason"],
        json!("turn_complete")
    );
    // A slow second turn.
    ts.request(
        "agent.send",
        json!({ "target": "seq/worker", "prompt": "@turn_ms=2000 second" }),
    )
    .unwrap();
    // A wait right after the send must NOT be satisfied by the (stale) first idle.
    let quick = ts.wait(&["seq/worker"], "600ms");
    assert_eq!(
        quick["timed_out"],
        json!(true),
        "the second wait must not be satisfied by the first idle"
    );
    // The second turn does complete when it finishes.
    assert_eq!(
        target(&ts.wait(&["seq/worker"], "20s"), "seq/worker")["reason"],
        json!("turn_complete")
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// gc immediate: the first turn completes, the response is captured, the pane is closed, and
/// the agent ends `completed`.
#[test]
fn e2e_gc_immediate_ends_completed() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run("imm/worker", Some("@turn_ms=0 do it"), Some("immediate"));
    let waited = ts.wait(&["imm/worker"], "20s");
    let t = target(&waited, "imm/worker");
    assert_eq!(t["reason"], json!("completed"));
    assert_eq!(t["status"], json!("ended"));
    assert_eq!(t["exit_reason"], json!("completed"));
    assert_eq!(t["ok"], json!(true));
    // The pane was closed → the `imm` workspace auto-removes.
    assert!(wait_until(Duration::from_secs(10), || {
        !ts.driver
            .workspace_list()
            .unwrap()
            .iter()
            .any(|w| w.label == "imm")
    }));
    let _ = uuid;
}

/// Restart the server mid-turn → wait re-arms conservatively and still completes.
#[test]
fn e2e_restart_mid_turn_rearms() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let uuid = ts.run("surv/worker", Some("@turn_ms=4000 long task"), None);
    assert!(ts.wait_status(&uuid, "working", Duration::from_secs(20)));
    // Crash mid-turn (agent pane keeps running — it is herdr-side).
    let pid = ts.pid();
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    assert!(wait_until(Duration::from_secs(10), || ts
        .client()
        .handshake()
        .is_err()));
    ts.spawn_server();
    // The turn completes and a wait settles after the restart.
    let done = ts.wait(&["surv/worker"], "25s");
    assert_eq!(
        target(&done, "surv/worker")["reason"],
        json!("turn_complete")
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// logs on a mock agent (no native transcript) fails loudly with transcript_unavailable.
#[test]
fn e2e_logs_transcript_unavailable_for_mock() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    // Suppress the mock's transcript so this exercises the genuinely-no-transcript path.
    let ts = TestServer::start_with_env(&[("ORCR_MOCK_NO_TRANSCRIPT", "1")]);
    let uuid = ts.run("log/worker", Some("@turn_ms=0 hi"), None);
    assert_eq!(
        target(&ts.wait(&["log/worker"], "20s"), "log/worker")["reason"],
        json!("turn_complete")
    );
    let e = ts
        .request(
            "agent.logs",
            json!({ "target": "log/worker", "last_response": true }),
        )
        .unwrap_err();
    assert_eq!(e.code, orchestratr::ErrorCode::TranscriptUnavailable);
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}
