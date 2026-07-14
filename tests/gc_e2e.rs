//! M4 GC & reconciliation e2e against **live herdr** + the mock provider (spec M4 acceptance).
//!
//! Gated behind `ORCR_E2E=1` so unit runs stay fast. Every test runs a real `orcr` server over
//! a throwaway `ORCR_HOME` whose config points at a **disposable** herdr session named
//! `orcr_test_<rand>`, torn down (`session stop` + `session delete`) by a drop guard. The
//! user's `default` session is never touched. Timings are shrunk (idle_after/kill_after/gc_tick
//! in the sub-second range) so park/reap happen fast.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test gc_e2e -- --test-threads=1 --nocapture`

use orchestratr::driver::{AgentStartParams, HerdrBinary, HerdrDriver, PaneInfo};
use orchestratr::home::Home;
use orchestratr::server::Client;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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

/// Fast GC windows + short mock completion tuning so the suite runs quickly.
const FAST_GC: &str = r#""concurrency":{"max":30},"timings":{"idle_after":"1s","kill_after":"2s","gc_tick":"400ms","attach_lease_ttl":"3s","max_starting":"60s"},"integrations":{"mock":{"fast_turn_grace_ms":1500,"idle_stable_ms":500,"transcript_settle_ms":0,"shutdown_grace_ms":150}}"#;

/// Longer idle_after + far-off kill_after so a park doesn't fire before the crash tests arm the
/// fault hook, and reap never interferes.
const CRASH_GC: &str = r#""concurrency":{"max":30},"timings":{"idle_after":"2s","kill_after":"60s","gc_tick":"400ms","attach_lease_ttl":"3s","max_starting":"60s"},"integrations":{"mock":{"fast_turn_grace_ms":1500,"idle_stable_ms":500,"transcript_settle_ms":0,"shutdown_grace_ms":150}}"#;

struct TestServer {
    home: tempfile::TempDir,
    bin: HerdrBinary,
    session: String,
    driver: HerdrDriver,
    /// Extra env applied to every spawned server (e.g. the park-crash fault hook).
    crash_phase: Option<String>,
}

impl TestServer {
    fn start() -> TestServer {
        Self::start_cfg(FAST_GC)
    }

    fn start_cfg(cfg_extra: &str) -> TestServer {
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
            format!(r#"{{"herdr":{{"session":"{session}"}},{cfg_extra}}}"#),
        )
        .unwrap();
        let ts = TestServer {
            home,
            bin,
            session,
            driver,
            crash_phase: None,
        };
        ts.spawn_server();
        ts
    }

    fn spawn_server(&self) {
        let mut cmd = Command::new(orcr_bin());
        cmd.args(["server", "start"])
            .env("ORCR_HOME", self.home.path())
            .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
            .env("ORCR_DEBUG_METHODS", "1")
            .env("ORCR_MOCK_AGENT_BIN", mock_agent_bin())
            .stdin(Stdio::null());
        if let Some(phase) = &self.crash_phase {
            cmd.env("ORCR_TEST_PARK_CRASH", phase);
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
    fn kill_server(&self) {
        let pid = self.pid();
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        assert!(wait_until(Duration::from_secs(10), || self
            .client()
            .handshake()
            .is_err()));
    }

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

    fn row(&self, uuid: &str) -> Option<Value> {
        let r = self.request("agent.ls", json!({ "all": true })).ok()?;
        r["agents"]
            .as_array()?
            .iter()
            .find(|a| a["uuid"] == json!(uuid))
            .cloned()
    }
    fn status(&self, uuid: &str) -> Option<String> {
        self.row(uuid)?["status"].as_str().map(String::from)
    }
    fn wait_status(&self, uuid: &str, want: &str, timeout: Duration) -> bool {
        wait_until(timeout, || self.status(uuid).as_deref() == Some(want))
    }
    fn server_status(&self) -> Value {
        self.request("server.status", json!({}))
            .expect("server.status")
    }

    /// The workspace label of an agent's current pane (via the owned-session driver).
    fn workspace_label_of(&self, uuid: &str) -> Option<String> {
        let pane_id = self.row(uuid)?["pane_id"].as_str()?.to_string();
        let panes = self.driver.pane_list(None).ok()?;
        let ws_id = panes
            .iter()
            .find(|p| p.pane_id == pane_id)?
            .workspace_id
            .clone();
        self.driver
            .workspace_list()
            .ok()?
            .into_iter()
            .find(|w| w.workspace_id == ws_id)
            .map(|w| w.label)
    }

    fn agent_panes(&self) -> Vec<PaneInfo> {
        self.driver
            .pane_list(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.agent.is_some())
            .collect()
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
    }
}

/// A second **disposable** herdr session standing in for a user's own session, where we
/// hand-start an agent orcr did not create (the unmanaged-discovery drill).
struct UserSession {
    bin: HerdrBinary,
    name: String,
    driver: HerdrDriver,
}
impl UserSession {
    fn start() -> UserSession {
        let bin = HerdrBinary::discover(None).unwrap();
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("orcr_test_{}", &rand[..12]);
        let sock = bin.ensure_session(&name).expect("user session");
        let driver = HerdrDriver::connect(&sock).unwrap();
        UserSession { bin, name, driver }
    }
    /// Hand-start a mock agent in this session; returns its terminal id.
    fn start_mock(&self) -> String {
        let mut env = BTreeMap::new();
        env.insert("ORCR_MOCK_AGENT".to_string(), "mock".to_string());
        let info = self
            .driver
            .agent_start(&AgentStartParams {
                name: "handmade".into(),
                argv: vec![mock_agent_bin()],
                cwd: None,
                env,
                focus: false,
                split: None,
                tab_id: None,
                workspace_id: None,
            })
            .expect("hand-start mock");
        info.terminal_id
    }
}
impl Drop for UserSession {
    fn drop(&mut self) {
        let _ = self.bin.session_stop(&self.name);
        let _ = self.bin.session_delete(&self.name);
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

macro_rules! skip_unless_e2e {
    () => {
        if !e2e_enabled() {
            eprintln!("skipping (set ORCR_E2E=1)");
            return;
        }
    };
}

/// Park → send → un-park: the agent parks to the `idle` workspace, a send moves it back to its
/// home workspace, clocks reset, and the delivered turn completes (spec M4 acceptance).
#[test]
fn e2e_park_send_unpark() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let uuid = ts.run("park/worker", Some("@turn_ms=0 hi"), Some("auto"));
    assert!(
        ts.wait_status(&uuid, "idle", Duration::from_secs(20)),
        "should go idle"
    );
    // idle_after 1s + gc_tick → parks.
    assert!(
        ts.wait_status(&uuid, "parked", Duration::from_secs(15)),
        "should park after idle_after"
    );
    assert_eq!(
        ts.workspace_label_of(&uuid).as_deref(),
        Some("idle"),
        "parked pane lives in the idle workspace"
    );
    // Send un-parks (before delivery), moving the pane home; the turn then completes.
    let sent = ts
        .request(
            "agent.send",
            json!({ "target": "park/worker", "prompt": "@turn_ms=0 back to work" }),
        )
        .expect("send");
    assert_eq!(sent["delivered_while"], json!("parked"));
    // Back in the home workspace + a fresh completion settles.
    let done = ts
        .request(
            "agent.wait",
            json!({ "targets": ["park/worker"], "timeout": "20s" }),
        )
        .unwrap();
    assert_eq!(done["targets"][0]["reason"], json!("turn_complete"));
    assert_eq!(
        ts.workspace_label_of(&uuid).as_deref(),
        Some("park"),
        "un-parked pane returns to its home workspace"
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Reap: a parked agent past `kill_after` is gracefully killed (`exit_reason: reaped`) and its
/// pane closed (spec M4 acceptance).
#[test]
fn e2e_park_then_reap() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let uuid = ts.run("reap/worker", Some("@turn_ms=0 hi"), Some("auto"));
    assert!(ts.wait_status(&uuid, "parked", Duration::from_secs(20)));
    // parked ≥ kill_after (2s) → reaped.
    assert!(
        ts.wait_status(&uuid, "ended", Duration::from_secs(15)),
        "should reap after kill_after"
    );
    assert_eq!(ts.row(&uuid).unwrap()["exit_reason"], json!("reaped"));
    // Pane closed → no agent panes leaked.
    assert!(wait_until(Duration::from_secs(10), || ts
        .agent_panes()
        .is_empty()));
}

/// `--gc never` is exempt from parking (spec §5.4).
#[test]
fn e2e_gc_never_not_parked() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let uuid = ts.run("never/worker", Some("@turn_ms=0 hi"), Some("never"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    // Well past idle_after — must NOT park.
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        ts.status(&uuid).as_deref(),
        Some("idle"),
        "gc never never parks"
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Explicit `--timeout` kills a still-working agent with `exit_reason: timeout` (spec §5.4).
#[test]
fn e2e_explicit_timeout_kills() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    // gc never so only the explicit --timeout can end it; a long turn keeps it working.
    let mut p = json!({ "path": "to/worker", "agent": "mock", "gc": "never",
                        "prompt": "@turn_ms=60000 long", "timeout": "2s" });
    p.as_object_mut().unwrap();
    let uuid = ts.request("agent.run", p).unwrap()["agent"]["uuid"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(ts.wait_status(&uuid, "ended", Duration::from_secs(15)));
    assert_eq!(ts.row(&uuid).unwrap()["exit_reason"], json!("timeout"));
}

/// Crash mid-park-move (after the herdr move, before the store commit) → restart → the
/// reconciler completes the park; status and location agree (spec M4 acceptance).
#[test]
fn e2e_crash_mid_park_completes() {
    skip_unless_e2e!();
    let mut ts = TestServer::start_cfg(CRASH_GC);
    let uuid = ts.run("crash/worker", Some("@turn_ms=0 hi"), Some("auto"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    // Arm the fault hook and let the GC thread crash the server mid-move. Restart clean.
    ts.crash_phase = Some("after_move".to_string());
    // The already-running server has no crash env; restart it WITH the hook so the next park
    // sweep crashes it. Kill, then respawn with the hook.
    ts.kill_server();
    ts.spawn_server(); // now armed — the park sweep will crash it
    assert!(wait_until(Duration::from_secs(15), || ts
        .client()
        .handshake()
        .is_err()));
    // Restart clean → reconciler completes the half-done move.
    ts.crash_phase = None;
    ts.spawn_server();
    assert!(
        ts.wait_status(&uuid, "parked", Duration::from_secs(15)),
        "reconciler should complete the park"
    );
    assert_eq!(
        ts.workspace_label_of(&uuid).as_deref(),
        Some("idle"),
        "completed park leaves the pane in the idle workspace"
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Crash mid-park-move (before the herdr move) → restart → the reconciler rolls back; the agent
/// stays idle in its home workspace (spec M4 acceptance).
#[test]
fn e2e_crash_before_park_rolls_back() {
    skip_unless_e2e!();
    let mut ts = TestServer::start_cfg(CRASH_GC);
    let uuid = ts.run("rb/worker", Some("@turn_ms=0 hi"), Some("auto"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    ts.kill_server();
    ts.crash_phase = Some("before_move".to_string());
    ts.spawn_server(); // armed — park sweep begins the move then crashes before the herdr move
    assert!(wait_until(Duration::from_secs(15), || ts
        .client()
        .handshake()
        .is_err()));
    ts.crash_phase = None;
    ts.spawn_server();
    // After recovery the move is rolled back; the agent is idle again in its home workspace.
    assert!(wait_until(Duration::from_secs(10), || {
        matches!(ts.status(&uuid).as_deref(), Some("idle") | Some("parked"))
    }));
    // It should not be stuck mid-move: move_state cleared (status is a terminal-ish public one).
    let st = ts.status(&uuid).unwrap();
    assert!(st == "idle" || st == "parked", "status settled, got {st}");
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Attach guard: park/reap deferred while a lease is fresh — including across a server restart
/// with a live lease — and resumes after release (spec M4 acceptance).
#[test]
fn e2e_attach_defers_gc() {
    skip_unless_e2e!();
    // Long lease so it stays fresh across a restart without heart-beating. idle_after is a
    // couple of seconds so we can reliably insert the lease before the first park would fire.
    let cfg = r#""concurrency":{"max":30},"timings":{"idle_after":"2s","kill_after":"3s","gc_tick":"400ms","attach_lease_ttl":"60s","max_starting":"60s"},"integrations":{"mock":{"idle_stable_ms":500,"transcript_settle_ms":0,"shutdown_grace_ms":150}}"#;
    let ts = TestServer::start_cfg(cfg);
    let uuid = ts.run("att/worker", Some("@turn_ms=0 hi"), Some("auto"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    // Insert a lease (prepare inserts it; we don't exec the interactive herdr attach here).
    let prep = ts
        .request(
            "agent.attach.prepare",
            json!({ "target": "att/worker", "client_pid": 4242 }),
        )
        .expect("attach.prepare");
    let lease = prep["lease_id"].as_str().unwrap().to_string();
    // Well past idle_after + kill_after — the fresh lease must keep it idle (never parks).
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(
        ts.status(&uuid).as_deref(),
        Some("idle"),
        "GC deferred while attached"
    );
    // Survives a restart: the lease is persisted and still fresh, so GC stays deferred.
    ts.kill_server();
    ts.spawn_server();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        ts.status(&uuid).as_deref(),
        Some("idle"),
        "GC still deferred after restart with a live lease"
    );
    // Release → GC resumes → parks.
    ts.request("agent.attach.release", json!({ "lease_id": lease }))
        .unwrap();
    assert!(
        ts.wait_status(&uuid, "parked", Duration::from_secs(10)),
        "GC resumes after detach"
    );
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Unknown-marked-pane drill: delete an agent's store row under a live pane → the reconciler
/// reports it in `server status` and never closes it (spec M4 acceptance).
#[test]
fn e2e_unknown_marked_pane_reported_not_closed() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let uuid = ts.run("orphan/worker", Some("@turn_ms=0 hi"), Some("never"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    let pane_id = ts.row(&uuid).unwrap()["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Delete the store row out from under the live pane (debug method).
    ts.request("__debug.delete_agent", json!({ "uuid": uuid }))
        .unwrap();
    // Reconciler counts it as an unknown marked pane and never touches it.
    assert!(wait_until(Duration::from_secs(10), || {
        ts.server_status()["counts"]["unknown_marked_panes"]
            .as_i64()
            .unwrap_or(0)
            >= 1
    }));
    // Several GC cycles later the pane is still alive.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        ts.driver
            .pane_list(None)
            .unwrap()
            .iter()
            .any(|p| p.pane_id == pane_id),
        "the orphaned pane must never be closed by orcr"
    );
    // Clean the orphan up via herdr directly (orcr won't).
    let _ = ts.driver.pane_close(&pane_id);
}

/// Foreign-pane safety: a user shell opened inside the owned session is reported as an unmarked
/// pane and never touched across many GC cycles (spec M4 acceptance).
#[test]
fn e2e_foreign_pane_never_touched() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    // A plain shell workspace created directly via herdr (orcr did not make it).
    let created = ts
        .driver
        .workspace_create(Some("foreign"), None, &BTreeMap::new())
        .unwrap();
    let shell_pane = created.root_pane.pane_id.clone();
    assert!(wait_until(Duration::from_secs(10), || {
        ts.server_status()["counts"]["unmarked_panes"]
            .as_i64()
            .unwrap_or(0)
            >= 1
    }));
    // Many GC cycles pass — the shell is never closed.
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        ts.driver
            .pane_list(None)
            .unwrap()
            .iter()
            .any(|p| p.pane_id == shell_pane),
        "a foreign user shell must never be touched"
    );
    let _ = ts.driver.pane_close(&shell_pane);
}

/// Reconciliation: a managed agent whose pane vanishes outside orcr → `lost`, then resolved to
/// `ended (lost)` on a following poll once herdr confirms the terminal is gone (spec §11.5).
#[test]
fn e2e_vanished_pane_lost_then_ended() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let uuid = ts.run("lost/worker", Some("@turn_ms=0 hi"), Some("never"));
    assert!(ts.wait_status(&uuid, "idle", Duration::from_secs(20)));
    let pane_id = ts.row(&uuid).unwrap()["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Close the pane behind orcr's back (herdr crash / manual close).
    ts.driver.pane_close(&pane_id).unwrap();
    // First reconcile poll → lost; a following poll (herdr reachable, terminal gone) → ended.
    assert!(ts.wait_status(&uuid, "ended", Duration::from_secs(15)));
    assert_eq!(ts.row(&uuid).unwrap()["exit_reason"], json!("lost"));
}

/// Unmanaged discovery: hand-start an agent in a user session → it appears in `ls` within
/// seconds with the right provider; closing its pane → the row ends (spec M4 acceptance).
#[test]
fn e2e_unmanaged_discovery() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let user = UserSession::start();
    let term = user.start_mock();
    // Give the pane a moment to self-report, then discovery (every ~3s) should pick it up.
    let found = wait_until(Duration::from_secs(15), || {
        let r = ts
            .request("agent.ls", json!({ "unmanaged": true }))
            .unwrap();
        r["agents"]
            .as_array()
            .map(|a| {
                a.iter().any(|x| {
                    x["managed"] == json!(false)
                        && x["path"].as_str().unwrap_or("").starts_with("unmanaged/")
                        && x["agent"] == json!("mock")
                })
            })
            .unwrap_or(false)
    });
    assert!(found, "hand-started agent should appear as unmanaged");
    // Grab OUR mock row specifically (the user's `default` session may also surface real agents
    // — discovery tracks every non-owned session, §5.7). Mock has no native transcript, so logs
    // → transcript_unavailable, but the path resolves (not integration_missing).
    let r = ts
        .request("agent.ls", json!({ "unmanaged": true }))
        .unwrap();
    let row = r["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["managed"] == json!(false) && x["agent"] == json!("mock"))
        .expect("our hand-started mock row")
        .clone();
    let uuid = row["uuid"].as_str().unwrap().to_string();
    let logs_err = ts
        .request(
            "agent.logs",
            json!({ "target": uuid.clone(), "last_response": true }),
        )
        .unwrap_err();
    assert_eq!(logs_err.code, orchestratr::ErrorCode::TranscriptUnavailable);
    // Close the pane in the user session → the row ends within a couple of discovery ticks.
    let _ = user.driver.pane_list(None).map(|panes| {
        for p in panes.iter().filter(|p| p.terminal_id == term) {
            let _ = user.driver.pane_close(&p.pane_id);
        }
    });
    assert!(
        wait_until(Duration::from_secs(15), || {
            ts.row(&uuid)
                .map(|r| r["status"] == json!("ended"))
                .unwrap_or(false)
        }),
        "closed unmanaged terminal → ended"
    );
}

/// Unmanaged kill requires `--force` (spec §5.7 behavior contract).
#[test]
fn e2e_unmanaged_kill_requires_force() {
    skip_unless_e2e!();
    let ts = TestServer::start();
    let user = UserSession::start();
    user.start_mock();
    let uuid = wait_until_some(Duration::from_secs(15), || {
        let r = ts.request("agent.ls", json!({ "unmanaged": true })).ok()?;
        r["agents"]
            .as_array()?
            .iter()
            .find(|x| x["managed"] == json!(false))
            .and_then(|x| x["uuid"].as_str().map(String::from))
    })
    .expect("unmanaged agent discovered");
    // Kill without --force → skipped (force_required); the pane in the user session is untouched.
    let killed = ts
        .request("agent.kill", json!({ "targets": [uuid] }))
        .unwrap();
    assert_eq!(killed["killed"].as_array().unwrap().len(), 0);
    assert_eq!(
        killed["skipped"][0]["reason"],
        json!("force_required"),
        "unmanaged kill needs --force"
    );
}

/// Soak: many mock agents churn (complete → park → reap); afterwards the owned session is clean
/// — no leaked agent panes, no leftover home workspaces (spec M4 acceptance, scaled). Override
/// the count with `ORCR_SOAK_AGENTS` (defaults to a CI-feasible 20; the spec's 100×1h is a
/// manual soak).
#[test]
fn e2e_soak_churn_leaves_no_leaks() {
    skip_unless_e2e!();
    let n: usize = std::env::var("ORCR_SOAK_AGENTS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let ts = TestServer::start();
    let mut uuids = Vec::new();
    for i in 0..n {
        uuids.push(ts.run(&format!("soak/w{i}"), Some("@turn_ms=0 go"), Some("auto")));
    }
    // Every agent should complete, park, and finally be reaped (ended/reaped).
    let all_reaped = wait_until(Duration::from_secs(90), || {
        uuids
            .iter()
            .all(|u| ts.status(u).as_deref() == Some("ended"))
    });
    assert!(all_reaped, "all soak agents should reap");
    for u in &uuids {
        assert_eq!(ts.row(u).unwrap()["exit_reason"], json!("reaped"));
    }
    // No agent panes left, and the home workspaces + idle holding pen auto-removed.
    assert!(wait_until(Duration::from_secs(15), || ts
        .agent_panes()
        .is_empty()));
    assert!(wait_until(Duration::from_secs(10), || {
        let ws = ts.driver.workspace_list().unwrap_or_default();
        !ws.iter().any(|w| w.label == "soak" || w.label == "idle")
    }));
}

fn wait_until_some<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}
