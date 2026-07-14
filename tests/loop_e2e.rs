//! M5 loop e2e (spec M5 acceptance). Scheduler mechanics run as plain OS processes; the
//! agent-glob-kill paths spawn the mock provider against **live herdr**.
//!
//! Gated behind `ORCR_E2E=1`. Each test runs a real `orcr` server over a throwaway `ORCR_HOME`
//! whose config points at a **disposable** herdr session (`orcr_test_<rand>`), torn down by a
//! drop guard. The user's `default` session is never touched.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test loop_e2e -- --test-threads=1 --nocapture`

use orchestratr::driver::HerdrBinary;
use orchestratr::home::Home;
use orchestratr::server::Client;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").as_deref() == Ok("1")
}
fn orcr_bin() -> String {
    env!("CARGO_BIN_EXE_orcr").to_string()
}
fn mock_agent_bin() -> String {
    env!("CARGO_BIN_EXE_orcr-mock-agent").to_string()
}

struct TestServer {
    home: tempfile::TempDir,
    bin: HerdrBinary,
    session: String,
}

impl TestServer {
    fn start() -> TestServer {
        let home = tempfile::tempdir().expect("home");
        let bin = HerdrBinary::discover(None).expect("herdr on PATH");
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let session = format!("orcr_test_{}", &rand[..12]);
        // Bootstrap the disposable session so agent spawns have a target.
        if let Err(e) = bin.ensure_session(&session) {
            let _ = bin.session_stop(&session);
            let _ = bin.session_delete(&session);
            panic!("disposable session bootstrap failed: {e}");
        }
        std::fs::write(
            home.path().join("config.json"),
            format!(
                r#"{{"herdr":{{"session":"{session}"}},"concurrency":{{"max":5}},
                    "timings":{{"loop_tick":"1s","run_term_grace":"1s"}}}}"#
            ),
        )
        .unwrap();
        let ts = TestServer { home, bin, session };
        ts.spawn_server();
        ts
    }

    fn spawn_server(&self) {
        let out = Command::new(orcr_bin())
            .args(["server", "start"])
            .env("ORCR_HOME", self.home.path())
            .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
            .env("ORCR_DISABLE_DISCOVERY", "1")
            .env("ORCR_MOCK_AGENT_BIN", mock_agent_bin())
            .stdin(Stdio::null())
            .output()
            .expect("orcr server start");
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

    /// Create a loop over the given argv with the given cadence params merged in.
    fn create_loop(&self, name: &str, mut extra: Value, argv: &[&str]) -> Value {
        let obj = extra.as_object_mut().unwrap();
        obj.insert("name".into(), json!(name));
        obj.insert(
            "command".into(),
            json!(argv.iter().map(|s| s.to_string()).collect::<Vec<_>>()),
        );
        obj.insert("cwd".into(), json!(self.home.path().display().to_string()));
        self.request("loop.create", extra).expect("loop.create")
    }

    fn run_ls(&self, name: &str, all: bool) -> Vec<Value> {
        self.request("loop.run.ls", json!({ "name": name, "all": all }))
            .map(|r| r["runs"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    fn run_status(&self, name: &str, run_id: &str) -> Option<String> {
        self.run_ls(name, true)
            .into_iter()
            .find(|r| r["run_id"].as_str() == Some(run_id))
            .and_then(|r| r["status"].as_str().map(String::from))
    }

    fn agents(&self, all: bool) -> Vec<Value> {
        self.request("agent.ls", json!({ "all": all }))
            .map(|r| r["agents"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Kill every loop run's process group FIRST, while the server + throwaway home still
        // exist — otherwise a lingering `orcr agent run` in a run command could execute against
        // a torn-down home, fall back to the default config, and bootstrap the real `orcr`
        // session (a safety defect). We read pgids over the live socket, then signal them.
        if let Ok(loops) = self.request("loop.ls", json!({ "all": true })) {
            for l in loops["loops"].as_array().cloned().unwrap_or_default() {
                if let Some(name) = l["name"].as_str() {
                    for run in self.run_ls(name, true) {
                        if let Some(pgid) = run["pgid"].as_i64() {
                            unsafe {
                                libc::kill(-(pgid as i32), libc::SIGKILL);
                            }
                        }
                    }
                }
            }
        }
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
    }
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

// --- tests ---

/// once-at fires once, the command runs, output is captured to run.log, and the loop ends.
#[test]
fn e2e_once_at_fires_and_captures_output() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let created = ts.create_loop(
        "hello",
        json!({ "once_at": "1s" }),
        &["sh", "-c", "echo captured-line-42"],
    );
    assert_eq!(created["loop"]["name"], json!("hello"));

    // The run fires and completes ok.
    assert!(
        wait_until(Duration::from_secs(15), || {
            ts.run_ls("hello", true)
                .iter()
                .any(|r| r["status"] == json!("ok"))
        }),
        "run should complete ok"
    );

    // Output was captured to run.log (source=command) and readable via loop logs.
    let logs = ts.request("loop.logs", json!({ "name": "hello" })).unwrap();
    let lines = logs["lines"].as_array().cloned().unwrap_or_default();
    assert!(
        lines.iter().any(|l| l["source"] == json!("command")
            && l["text"]
                .as_str()
                .unwrap_or("")
                .contains("captured-line-42")),
        "run.log should contain the command output: {lines:?}"
    );

    // A `once` loop ends after firing → the name is free (loop ls default hides ended).
    let loops = ts.request("loop.ls", json!({})).unwrap();
    assert!(loops["loops"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true));
}

/// Active-loop namespace protection (spec §5.1): while `nightly` is active, root/unrelated
/// contexts cannot create `nightly/foo` nor `/nightly/foo`; after the loop ends it is free.
#[test]
fn e2e_namespace_protection() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    ts.create_loop(
        "nightly",
        json!({ "cron": "0 0 1 1 *" }),
        &["sh", "-c", "sleep 1"],
    );

    // A root context (no caller) cannot create an agent under the active loop's name.
    for target in ["nightly/foo", "/nightly/foo"] {
        let e = ts
            .request(
                "agent.run",
                json!({ "path": target, "agent": "mock", "prompt": "x" }),
            )
            .unwrap_err();
        assert_eq!(
            e.details["reason"], "reserved_name",
            "creating {target} from root must be rejected"
        );
    }

    // After the loop ends, the name is reusable by a root agent.
    ts.request("loop.rm", json!({ "names": ["nightly"] }))
        .unwrap();
    let ok = ts.request(
        "agent.run",
        json!({ "path": "nightly/reused", "agent": "mock", "prompt": "x" }),
    );
    assert!(
        ok.is_ok(),
        "name should be reusable after the loop ends: {ok:?}"
    );
}

/// Capacity + promotion: cap 1, two slow manual runs → one running + one pending; stopping the
/// running one promotes the pending one.
#[test]
fn e2e_capacity_pending_and_promotion() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    ts.create_loop(
        "slow",
        json!({ "cron": "0 0 1 1 *", "max_concurrency": 1 }),
        &["sh", "-c", "sleep 20"],
    );

    let r1 = ts
        .request("loop.run.start", json!({ "name": "slow" }))
        .unwrap();
    let run1 = r1["run"]["run_id"].as_str().unwrap().to_string();
    assert!(
        wait_until(Duration::from_secs(10), || {
            ts.run_status("slow", &run1).as_deref() == Some("running")
        }),
        "first run should be running"
    );

    let r2 = ts
        .request("loop.run.start", json!({ "name": "slow" }))
        .unwrap();
    let run2 = r2["run"]["run_id"].as_str().unwrap().to_string();
    assert_eq!(
        r2["run"]["status"],
        json!("pending"),
        "second run at capacity is pending"
    );

    // Stop the first run → the pending second run promotes to running.
    ts.request(
        "loop.run.stop",
        json!({ "name": "slow", "run": run1.clone() }),
    )
    .unwrap();
    assert!(
        wait_until(Duration::from_secs(10), || {
            ts.run_status("slow", &run1).as_deref() == Some("stopped")
        }),
        "first run should be stopped"
    );
    assert!(
        wait_until(Duration::from_secs(10), || {
            ts.run_status("slow", &run2).as_deref() == Some("running")
        }),
        "second run should promote to running"
    );
}

/// `loop run start` on a paused loop fires once; scheduled fires stay held.
#[test]
fn e2e_manual_start_on_paused_loop() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // A cron that fires every minute — but we pause immediately so nothing scheduled runs.
    ts.create_loop(
        "paused_loop",
        json!({ "cron": "* * * * *" }),
        &["sh", "-c", "echo manual-run-ok"],
    );
    ts.request("loop.pause", json!({ "names": ["paused_loop"] }))
        .unwrap();

    // A manual start still fires.
    let r = ts
        .request("loop.run.start", json!({ "name": "paused_loop" }))
        .unwrap();
    let run = r["run"]["run_id"].as_str().unwrap().to_string();
    assert!(
        wait_until(Duration::from_secs(10), || {
            ts.run_status("paused_loop", &run).as_deref() == Some("ok")
        }),
        "manual run should complete on a paused loop"
    );
    // Only the one manual run exists — no scheduled fire slipped through while paused.
    let manual_count = ts
        .run_ls("paused_loop", true)
        .iter()
        .filter(|r| r["kind"] == json!("scheduled"))
        .count();
    assert_eq!(manual_count, 0, "no scheduled runs while paused");
}

/// `loop run stop <run>` kills one of two concurrent runs; the other survives; the stopped
/// run's agents are glob-killed.
#[test]
fn e2e_stop_one_run_kills_its_agents_only() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // Each run spawns a mock agent under its own run path, then stays alive.
    let cmd = format!(
        "{} agent run --path worker -a mock -p hi; sleep 30",
        orcr_bin()
    );
    ts.create_loop(
        "fleet",
        json!({ "cron": "0 0 1 1 *", "max_concurrency": 2 }),
        &["sh", "-c", &cmd],
    );

    let r1 = ts
        .request("loop.run.start", json!({ "name": "fleet" }))
        .unwrap();
    let run1 = r1["run"]["run_id"].as_str().unwrap().to_string();
    let r2 = ts
        .request("loop.run.start", json!({ "name": "fleet" }))
        .unwrap();
    let run2 = r2["run"]["run_id"].as_str().unwrap().to_string();

    // Both runs spawn their agent.
    let agent_active = |run: &str| -> Option<Value> {
        ts.agents(false).into_iter().find(|a| {
            a["path"]
                .as_str()
                .unwrap_or("")
                .starts_with(&format!("fleet/{run}/"))
        })
    };
    assert!(
        wait_until(Duration::from_secs(20), || agent_active(&run1).is_some()
            && agent_active(&run2).is_some()),
        "both runs should spawn an agent"
    );

    // Stop run1 only.
    ts.request(
        "loop.run.stop",
        json!({ "name": "fleet", "run": run1.clone() }),
    )
    .unwrap();

    // run1's agent is glob-killed (gone from the active set); run2's survives.
    assert!(
        wait_until(Duration::from_secs(20), || agent_active(&run1).is_none()),
        "stopped run's agent should be glob-killed"
    );
    assert_eq!(ts.run_status("fleet", &run1).as_deref(), Some("stopped"));
    assert!(
        agent_active(&run2).is_some(),
        "other run's agent should survive"
    );
    assert_eq!(ts.run_status("fleet", &run2).as_deref(), Some("running"));
}

/// `loop logs --run` isolates one run's lines when two runs interleave.
#[test]
fn e2e_logs_run_isolation() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    ts.create_loop(
        "two",
        json!({ "cron": "0 0 1 1 *", "max_concurrency": 2 }),
        &["sh", "-c", "echo line-for-$ORCR_PATH"],
    );
    let r1 = ts
        .request("loop.run.start", json!({ "name": "two" }))
        .unwrap();
    let run1 = r1["run"]["run_id"].as_str().unwrap().to_string();
    let r2 = ts
        .request("loop.run.start", json!({ "name": "two" }))
        .unwrap();
    let run2 = r2["run"]["run_id"].as_str().unwrap().to_string();

    assert!(
        wait_until(Duration::from_secs(15), || {
            ts.run_status("two", &run1).as_deref() == Some("ok")
                && ts.run_status("two", &run2).as_deref() == Some("ok")
        }),
        "both runs should complete"
    );

    let logs1 = ts
        .request(
            "loop.logs",
            json!({ "name": "two", "run": run1.clone(), "source": "command" }),
        )
        .unwrap();
    let lines1 = logs1["lines"].as_array().cloned().unwrap_or_default();
    assert!(!lines1.is_empty(), "run1 should have command output");
    assert!(
        lines1
            .iter()
            .all(|l| l["run"].as_str() == Some(&format!("two/{run1}"))),
        "--run must isolate to run1's lines only: {lines1:?}"
    );
    assert!(
        lines1
            .iter()
            .any(|l| l["text"].as_str().unwrap_or("").contains(&run1)),
        "run1's output should reference its own path"
    );
    // And run2's filter must not leak run1's lines.
    let logs2 = ts
        .request(
            "loop.logs",
            json!({ "name": "two", "run": run2.clone(), "source": "command" }),
        )
        .unwrap();
    let lines2 = logs2["lines"].as_array().cloned().unwrap_or_default();
    assert!(lines2
        .iter()
        .all(|l| l["run"].as_str() == Some(&format!("two/{run2}"))));
}

/// Reboot simulation: kill the server (and the run's process group) with a running run + a
/// pending fire → restart → the dead run is closed out, its agents killed, and the pending fire
/// is decided exactly once.
#[test]
fn e2e_restart_recovery() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let cmd = format!(
        "{} agent run --path worker -a mock -p hi; sleep 60",
        orcr_bin()
    );
    ts.create_loop(
        "reboot",
        json!({ "cron": "0 0 1 1 *", "max_concurrency": 1 }),
        &["sh", "-c", &cmd],
    );
    let r1 = ts
        .request("loop.run.start", json!({ "name": "reboot" }))
        .unwrap();
    let run1 = r1["run"]["run_id"].as_str().unwrap().to_string();
    let r2 = ts
        .request("loop.run.start", json!({ "name": "reboot" }))
        .unwrap();
    let run2 = r2["run"]["run_id"].as_str().unwrap().to_string();

    // run1 running with an agent; run2 pending.
    assert!(
        wait_until(Duration::from_secs(20), || {
            ts.run_status("reboot", &run1).as_deref() == Some("running")
                && ts.agents(false).iter().any(|a| {
                    a["path"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with(&format!("reboot/{run1}/"))
                })
        }),
        "run1 should be running with an agent"
    );
    assert_eq!(ts.run_status("reboot", &run2).as_deref(), Some("pending"));

    // Simulate a reboot: kill run1's process group (so its leader is truly dead, mirroring a
    // machine reboot) and the server hard.
    let run1_pgid = ts
        .run_ls("reboot", true)
        .into_iter()
        .find(|r| r["run_id"].as_str() == Some(&run1))
        .and_then(|r| r["pgid"].as_i64())
        .expect("run1 pgid recorded");
    let server_pid = ts.pid();
    unsafe {
        libc::kill(server_pid as i32, libc::SIGKILL);
        // Negative pid targets the whole process group (the run process ran under setsid).
        libc::kill(-(run1_pgid as i32), libc::SIGKILL);
    }
    std::thread::sleep(Duration::from_millis(500));

    // Restart the server → recovery runs.
    ts.spawn_server();

    // The dead run is closed out (failed), and its agents are glob-killed.
    assert!(
        wait_until(Duration::from_secs(20), || {
            matches!(ts.run_status("reboot", &run1).as_deref(), Some("failed"))
        }),
        "dead run should be closed out as failed"
    );
    assert!(
        wait_until(Duration::from_secs(20), || {
            !ts.agents(false).iter().any(|a| {
                a["path"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with(&format!("reboot/{run1}/"))
            })
        }),
        "dead run's agents should be glob-killed on recovery"
    );
    // The pending fire is decided exactly once: run2 promotes to running, no duplicate runs.
    assert!(
        wait_until(Duration::from_secs(20), || {
            ts.run_status("reboot", &run2).as_deref() == Some("running")
        }),
        "pending run should promote exactly once after recovery"
    );
    assert_eq!(
        ts.run_ls("reboot", true).len(),
        2,
        "no replayed/duplicated runs"
    );
}

/// Missed cron fire (server down across a scheduled slot): a cron fire that comes due while the
/// server is dead is skipped-and-logged on restart (never replayed), and `next_fire_at` is
/// advanced forward (spec §6.2, §11.3).
#[test]
fn e2e_missed_cron_fire_skipped() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // A once-a-minute cron: its next fire is the top of the next minute (<= 60s away).
    let created = ts.create_loop(
        "missed",
        json!({ "cron": "* * * * *", "max_concurrency": 1 }),
        &["sh", "-c", "echo tick"],
    );
    let orig_next = created["loop"]["next_fire_at"]
        .as_i64()
        .expect("next_fire_at set at create");

    // Kill the server hard *before* the fire is due, so it can never tick that slot.
    let server_pid = ts.pid();
    unsafe {
        libc::kill(server_pid as i32, libc::SIGKILL);
    }
    // Wait past the scheduled slot in real wall-clock time so the fire is genuinely missed.
    let now_ms = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    };
    while now_ms() <= orig_next + 1000 {
        std::thread::sleep(Duration::from_millis(200));
    }

    // Restart → recovery decides the missed slot exactly once: skipped, not replayed.
    ts.spawn_server();

    // A `loop.skipped` (reason=missed_while_down) event is emitted, visible via `--source orcr`.
    assert!(
        wait_until(Duration::from_secs(20), || {
            let logs = ts
                .request("loop.logs", json!({ "name": "missed", "source": "orcr" }))
                .unwrap_or_default();
            logs["lines"]
                .as_array()
                .map(|ls| {
                    ls.iter().any(|l| {
                        let t = l["text"].as_str().unwrap_or("");
                        t.contains("loop.skipped") && t.contains("missed_while_down")
                    })
                })
                .unwrap_or(false)
        }),
        "a loop.skipped(missed_while_down) event should be logged on restart"
    );

    // No run row was created for the missed slot — the fire was skipped, never replayed.
    assert!(
        ts.run_ls("missed", true).is_empty(),
        "missed cron fire must not be replayed as a run"
    );

    // next_fire_at was advanced forward past the missed slot.
    let new_next = ts
        .request("loop.ls", json!({ "all": true }))
        .unwrap()
        .get("loops")
        .and_then(|v| v.as_array())
        .and_then(|ls| {
            ls.iter()
                .find(|l| l["name"].as_str() == Some("missed"))
                .cloned()
        })
        .and_then(|l| l["next_fire_at"].as_i64())
        .expect("missed loop still present with a next_fire_at");
    assert!(
        new_next > orig_next,
        "next_fire_at should advance past the missed slot (was {orig_next}, now {new_next})"
    );
}

/// Concurrent promotion never double-spawns a run nor exceeds max_concurrency (spec §11.3,
/// reviewer finding #1). cap 2 with a queue of fast runs whose commands exit near-simultaneously
/// makes several exit-monitor threads call promote_pending at once. Each run's command appends
/// its run path to a shared file; if any slot were handed out twice, a run would execute twice
/// (a duplicate line) or more runs than allocated would appear.
#[test]
fn e2e_concurrent_promotion_no_double_spawn() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // Each run: settle briefly so the two active runs exit close together (concurrent
    // promotion), then atomically append its own run path to a shared tally file.
    ts.create_loop(
        "conc",
        json!({ "cron": "0 0 1 1 *", "max_concurrency": 2 }),
        &[
            "sh",
            "-c",
            "sleep 0.3; echo \"$ORCR_PATH\" >> \"$ORCR_LOOP_DATA_DIR/ran.txt\"",
        ],
    );

    // Queue 8 manual runs: 2 start, 6 sit pending and promote as slots free.
    let total = 8;
    let mut run_ids = Vec::new();
    for _ in 0..total {
        let r = ts
            .request("loop.run.start", json!({ "name": "conc" }))
            .unwrap();
        run_ids.push(r["run"]["run_id"].as_str().unwrap().to_string());
    }

    // Every run reaches a terminal `ok`.
    assert!(
        wait_until(Duration::from_secs(30), || {
            let rows = ts.run_ls("conc", true);
            rows.len() == total && rows.iter().all(|r| r["status"] == json!("ok"))
        }),
        "all {total} runs should finish ok: {:?}",
        ts.run_ls("conc", true)
    );

    // Exactly `total` distinct run rows exist — no slot handed out twice as an extra run.
    let rows = ts.run_ls("conc", true);
    assert_eq!(rows.len(), total, "no extra run rows allocated");

    // The tally file holds exactly one line per run, each run path exactly once — proof no
    // run's command executed twice via a double promotion.
    let tally = ts.home.path().join("data").join("conc").join("ran.txt");
    let text = std::fs::read_to_string(&tally).expect("tally file written");
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    assert_eq!(
        lines.len(),
        total,
        "each run must execute exactly once (got {lines:?})"
    );
    lines.sort();
    let distinct = {
        let mut d = lines.clone();
        d.dedup();
        d.len()
    };
    assert_eq!(distinct, total, "no run path may appear twice: {lines:?}");
}
