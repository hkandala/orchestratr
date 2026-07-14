//! M2 agent-core e2e against **live herdr** + the mock provider (spec M2 acceptance).
//!
//! Gated behind `ORCR_E2E=1` so unit runs stay fast. Every test runs a real `orcr` server
//! over a throwaway `ORCR_HOME` whose config points at a **disposable** herdr session named
//! `orcr_test_<rand>`, torn down (`session stop` + `session delete`) by a drop guard. The
//! user's `default` session is never touched. The server runs with
//! `ORCR_ALLOW_MOCK_PROVIDER=1` so the mock stands in for a real provider.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test agent_e2e -- --test-threads=1 --nocapture`

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

/// A live orcr server bound to a throwaway home + a disposable herdr session.
struct TestServer {
    home: tempfile::TempDir,
    bin: HerdrBinary,
    session: String,
    driver: HerdrDriver,
}

impl TestServer {
    fn start() -> TestServer {
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
            format!(r#"{{"herdr":{{"session":"{session}"}},"concurrency":{{"max":5}}}}"#),
        )
        .unwrap();
        let ts = TestServer {
            home,
            bin,
            session,
            driver,
        };
        ts.spawn_server();
        ts
    }

    fn spawn_server(&self) {
        let out = Command::new(orcr_bin())
            .args(["server", "start"])
            .env("ORCR_HOME", self.home.path())
            .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
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

    /// The server pid (for kill -9 crash simulation).
    fn pid(&self) -> u32 {
        self.client().handshake().unwrap()["pid"].as_u64().unwrap() as u32
    }

    /// Run a mock agent at `path`; returns its uuid + data_dir.
    fn run(&self, path: &str, prompt: Option<&str>) -> (String, String) {
        let mut p = json!({ "path": path, "agent": "mock" });
        if let Some(pr) = prompt {
            p["prompt"] = json!(pr);
        }
        let r = self.request("agent.run", p).expect("agent.run");
        let a = &r["agent"];
        (
            a["uuid"].as_str().unwrap().to_string(),
            a["data_dir"].as_str().unwrap().to_string(),
        )
    }

    /// The status of an agent by uuid, from `agent ls --all`.
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

    fn agents(&self) -> Vec<Value> {
        self.request("api.snapshot", json!({}))
            .map(|r| r["agents"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // Stop the server, then reap any lingering foreground process, then the session.
        let _ = self.request("server.stop", json!({}));
        // Best-effort reap: kill anything still bound to this home's socket.
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

/// run → pane under the right workspace/tab · env contract present · send delivers · kill
/// graceful → pane closed → workspace emptied.
#[test]
fn e2e_run_send_kill_lifecycle() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let (uuid, data_dir) = ts.run("review/worker", Some("hello there"));

    // Reaches working (pre-M3 stays there).
    assert!(
        ts.wait_status(&uuid, "working", Duration::from_secs(20)),
        "agent should reach working"
    );

    // The pane exists in the owned session, in workspace `review`, labeled `worker`.
    let ws = ts
        .driver
        .workspace_list()
        .unwrap()
        .into_iter()
        .find(|w| w.label == "review")
        .expect("workspace `review` should exist");
    assert!(ts
        .driver
        .pane_list(Some(&ws.workspace_id))
        .unwrap()
        .iter()
        .any(|p| p.label.as_deref() == Some("worker")));

    // The env contract reached the pane (mock dumped it to its data dir, §5.3).
    let env: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{data_dir}/mock_env.json")).expect("mock_env.json"),
    )
    .unwrap();
    assert_eq!(env["ORCR_ID"], json!(uuid));
    assert_eq!(env["ORCR_PATH"], json!("review/worker"));
    assert_eq!(env["ORCR_AGENT_DATA_DIR"], json!(data_dir));

    // send delivers and reports delivered_while + input_seq.
    let sent = ts
        .request(
            "agent.send",
            json!({ "target": "review/worker", "prompt": "again" }),
        )
        .unwrap();
    assert_eq!(sent["delivered_while"], json!("working"));
    assert_eq!(sent["input_seq"], json!(2)); // turn 1 = first prompt, turn 2 = send

    // kill (graceful) → pane closed → workspace emptied & auto-removed.
    let killed = ts
        .request("agent.kill", json!({ "targets": ["review/worker"] }))
        .unwrap();
    assert_eq!(killed["all_killed"], json!(true));
    assert!(ts.wait_status(&uuid, "ended", Duration::from_secs(10)));
    assert!(
        wait_until(Duration::from_secs(10), || {
            !ts.driver
                .workspace_list()
                .unwrap()
                .iter()
                .any(|w| w.label == "review")
        }),
        "workspace `review` should auto-remove once its last pane closes"
    );
}

/// The path-model conformance table (spec §5.1), asserted over the socket **and** the CLI:
/// `--name` in scope, `--path` relative, leading `/` absolute, agent scope = path minus
/// name, ended-path reuse + uuid history, depth-limit + reserved errors.
#[test]
fn e2e_path_model_conformance() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();

    // Resolve a run request and return the effective path (killing the queued agent so the
    // table can be exercised without dozens of live panes).
    let resolve = |params: Value| -> orchestratr::Result<String> {
        let r = ts.request("agent.run", params)?;
        let path = r["agent"]["path"].as_str().unwrap().to_string();
        let uuid = r["agent"]["uuid"].as_str().unwrap().to_string();
        let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
        Ok(path)
    };

    // --name lands directly in the caller's scope (caller `proj/lead` → scope `proj`).
    assert_eq!(
        resolve(json!({ "name": "w1", "agent": "mock", "caller_path": "proj/lead" })).unwrap(),
        "proj/w1"
    );
    // --path is relative to the caller's scope.
    assert_eq!(
        resolve(json!({ "path": "sub/w2", "agent": "mock", "caller_path": "proj/lead" })).unwrap(),
        "proj/sub/w2"
    );
    // Leading `/` is absolute — scope is ignored.
    assert_eq!(
        resolve(json!({ "path": "/root/w3", "agent": "mock", "caller_path": "proj/lead" }))
            .unwrap(),
        "root/w3"
    );
    // Agent scope = path minus name: a child of `deep/mid/leaf` lands beside it.
    assert_eq!(
        resolve(json!({ "name": "child", "agent": "mock", "caller_path": "deep/mid/leaf" }))
            .unwrap(),
        "deep/mid/child"
    );

    // Depth limit (> 8 segments) → invalid_request:path_too_deep, nothing spawned.
    let deep = (0..9)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join("/");
    let e = ts
        .request("agent.run", json!({ "path": deep, "agent": "mock" }))
        .unwrap_err();
    assert_eq!(e.code, orchestratr::ErrorCode::InvalidRequest);
    assert_eq!(e.details["reason"], json!("path_too_deep"));

    // Reserved level-1 name → invalid_request:reserved_name.
    let e = ts
        .request("agent.run", json!({ "path": "idle/x", "agent": "mock" }))
        .unwrap_err();
    assert_eq!(e.details["reason"], json!("reserved_name"));

    // Ended-path reuse + uuid history: run, kill (→ ended), reuse the path, both in history.
    let r1 = ts
        .request("agent.run", json!({ "name": "reuse", "agent": "mock" }))
        .unwrap();
    let u1 = r1["agent"]["uuid"].as_str().unwrap().to_string();
    ts.request("agent.kill", json!({ "targets": [u1.clone()] }))
        .unwrap();
    assert!(ts.wait_status(&u1, "ended", Duration::from_secs(10)));
    let r2 = ts
        .request("agent.run", json!({ "name": "reuse", "agent": "mock" }))
        .unwrap();
    let u2 = r2["agent"]["uuid"].as_str().unwrap().to_string();
    assert_ne!(u1, u2, "reused path gets a fresh uuid");
    let all = ts.request("agent.ls", json!({ "all": true })).unwrap();
    let reuse_rows: Vec<&Value> = all["agents"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["path"] == json!("reuse"))
        .collect();
    assert_eq!(
        reuse_rows.len(),
        2,
        "history keeps both generations by uuid"
    );
    let _ = ts.request("agent.kill", json!({ "targets": [u2] }));

    // CLI parity: the same resolution through the `orcr` binary with ORCR_PATH set.
    let out = Command::new(orcr_bin())
        .args(["agent", "run", "--name", "climodel", "-a", "mock", "--json"])
        .env("ORCR_HOME", ts.home.path())
        .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
        .env("ORCR_MOCK_AGENT_BIN", mock_agent_bin())
        .env("ORCR_ID", "cli-caller")
        .env("ORCR_PATH", "proj/lead")
        .stdin(Stdio::null())
        .output()
        .expect("cli run");
    let env: Value = serde_json::from_slice(&out.stdout).expect("cli json");
    assert_eq!(env["ok"], json!(true));
    assert_eq!(env["result"]["agent"]["path"], json!("proj/climodel"));
    assert_eq!(env["result"]["agent"]["parent_path"], json!("proj/lead"));
    let cli_uuid = env["result"]["agent"]["uuid"].as_str().unwrap().to_string();
    let _ = ts.request("agent.kill", json!({ "targets": [cli_uuid] }));
}

/// 50 concurrent runs at cap 5: FIFO promotion order, never over cap, queue drains.
#[test]
fn e2e_concurrency_caps_and_fifo() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    const N: usize = 50;
    const CAP: usize = 5;

    // Subscribe from the beginning to observe promotion order + lifecycle.
    let (_init, mut sub) = ts
        .client()
        .open_stream("events.subscribe", json!({ "since_seq": 0 }))
        .unwrap();
    sub.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();

    // Enqueue N agents sequentially → queue_seq (and thus FIFO promotion) matches this order.
    let mut order = Vec::new();
    for i in 0..N {
        let (uuid, _) = ts.run(&format!("burn/a{i}"), None);
        order.push(uuid);
    }

    let mut promoted_order = Vec::new();
    let mut occupying = std::collections::BTreeSet::new();
    let mut max_occupying = 0usize;
    let mut ended = std::collections::BTreeSet::new();

    let deadline = Instant::now() + Duration::from_secs(120);
    while ended.len() < N && Instant::now() < deadline {
        let ev = match sub.next_event() {
            Ok(Some(e)) => e,
            Ok(None) => break,
            Err(_) => continue, // read timeout — loop
        };
        let ev = &ev["event"];
        let kind = ev["kind"].as_str().unwrap_or("");
        let uuid = ev["ref_uuid"].as_str().unwrap_or("").to_string();
        if !order.contains(&uuid) {
            continue;
        }
        match kind {
            "queue.promoted" => {
                promoted_order.push(uuid.clone());
                occupying.insert(uuid.clone());
                max_occupying = max_occupying.max(occupying.len());
                // Free the slot promptly so the queue drains.
                let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
            }
            "agent.ended" => {
                occupying.remove(&uuid);
                ended.insert(uuid);
            }
            _ => {}
        }
    }

    assert_eq!(ended.len(), N, "every agent should end (queue drains)");
    assert!(
        max_occupying <= CAP,
        "concurrency never exceeds the cap: saw {max_occupying} > {CAP}"
    );
    assert_eq!(
        promoted_order, order,
        "promotion order must be strict FIFO by enqueue order"
    );
}

/// Concurrent same-path spawns: exactly one wins, the rest get state_conflict:path_in_use.
#[test]
fn e2e_concurrent_same_path_one_winner() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let client = ts.client();
            std::thread::spawn(move || {
                client.request("agent.run", json!({ "path": "solo/one", "agent": "mock" }))
            })
        })
        .collect();
    let mut ok = 0;
    let mut conflicts = 0;
    for h in handles {
        match h.join().unwrap() {
            Ok(_) => ok += 1,
            Err(e) => {
                assert_eq!(e.code, orchestratr::ErrorCode::StateConflict);
                assert_eq!(e.details["reason"], json!("path_in_use"));
                conflicts += 1;
            }
        }
    }
    assert_eq!(ok, 1, "exactly one spawn wins the path");
    assert_eq!(conflicts, 7, "the rest collide with path_in_use");
}

/// kill during `starting`: canceled cleanly between pipeline steps.
#[test]
fn e2e_kill_during_starting() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let (uuid, _) = ts.run("cancel/me", Some("a prompt so starting lasts a beat"));
    // Catch it in the `starting` window (spawn takes >1s: session poll + enter delay).
    assert!(
        ts.wait_status(&uuid, "starting", Duration::from_secs(10)),
        "should observe the starting state"
    );
    ts.request("agent.kill", json!({ "targets": [uuid.clone()] }))
        .unwrap();
    assert!(ts.wait_status(&uuid, "ended", Duration::from_secs(10)));
    // Ended with exit_reason canceled (never ran to completion).
    let row = ts.request("agent.ls", json!({ "all": true })).unwrap()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["uuid"] == json!(uuid))
        .cloned()
        .unwrap();
    assert_eq!(row["exit_reason"], json!("canceled"));
    // No orphan pane for `cancel` workspace survives.
    assert!(wait_until(Duration::from_secs(10), || {
        !ts.driver
            .pane_list(None)
            .unwrap()
            .iter()
            .any(|p| p.label.as_deref() == Some("me"))
    }));
}

/// A provider that reports idle right after start is held at `working` (no false completion
/// before M3).
#[test]
fn e2e_idle_report_held_at_working() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // No prompt → the mock announces idle immediately after startup (it self-reports).
    let (uuid, _) = ts.run("hold/worker", None);
    assert!(ts.wait_status(&uuid, "working", Duration::from_secs(20)));
    // Even after the mock has reported idle to herdr, orcr holds working (§5.6 pre-M3).
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(ts.status(&uuid).as_deref(), Some("working"));
    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Integration-missing: a provider without both layers fails fast, nothing spawned.
#[test]
fn e2e_integration_missing_fails_fast() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // `pi` has a herdr integration in this env but no orcr built-in → integration_missing.
    let e = ts
        .request("agent.run", json!({ "name": "x", "agent": "pi" }))
        .unwrap_err();
    assert_eq!(e.code, orchestratr::ErrorCode::IntegrationMissing);
    assert_eq!(e.exit_code(), 2);
    // Nothing was enqueued.
    assert!(ts.agents().is_empty());
}

/// Crash mid-run (kill -9) → restart → reconciler confirms the live pane and repairs the
/// row; no duplicate pane.
#[test]
fn e2e_crash_recovery_repairs_running() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    let (uuid, _) = ts.run("survive/worker", None);
    assert!(ts.wait_status(&uuid, "working", Duration::from_secs(20)));
    let pane_present = |ts: &TestServer| {
        ts.driver
            .pane_list(None)
            .unwrap()
            .iter()
            .any(|p| p.label.as_deref() == Some("worker"))
    };
    assert!(pane_present(&ts));

    // Crash: SIGKILL the server (agent pane keeps running — it's herdr-side).
    let pid = ts.pid();
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    assert!(wait_until(Duration::from_secs(10), || ts
        .client()
        .handshake()
        .is_err()));

    // Restart → reconcile confirms the pane and keeps the agent working; no dup pane.
    ts.spawn_server();
    assert_eq!(ts.status(&uuid).as_deref(), Some("working"));
    let workers = ts
        .driver
        .pane_list(None)
        .unwrap()
        .into_iter()
        .filter(|p| p.label.as_deref() == Some("worker"))
        .count();
    assert_eq!(workers, 1, "exactly one pane survives (no duplicate)");

    let _ = ts.request("agent.kill", json!({ "targets": [uuid] }));
}

/// Crash mid-spawn (pane created but not recorded) → restart → reconciler closes the orphan
/// pane and fails the row; no duplicate survives (§11.1).
#[test]
fn e2e_crash_recovery_closes_orphan() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    use orchestratr::store::{NewAgent, Store};

    let ts = TestServer::start();

    // Simulate a spawn that crashed after agent.start but before recording the pane:
    //  - a starting row with no pane_id,
    //  - an orphan herdr pane whose label = the tab label of that path.
    // Stop the server so we can write the store it owns.
    let _ = ts.request("server.stop", json!({}));
    assert!(wait_until(Duration::from_secs(10), || ts
        .client()
        .handshake()
        .is_err()));

    let store_path = Home::at(ts.home.path()).store_path();
    {
        let mut store = Store::open(&store_path).unwrap();
        let mut a = NewAgent::queued("crash-uuid-1", "recover/orphan", "mock");
        a.herdr_session = Some(ts.session.clone());
        a.launch_token = Some("tok-1".into());
        store.enqueue_agent(&a).unwrap();
        // Promote to `starting` with no pane recorded.
        store
            .promote_queued(5, &std::collections::BTreeMap::new(), 1)
            .unwrap();
    }

    // Create the orphan pane in the owned session (workspace `recover`, pane label `orphan`).
    let ws = ts
        .driver
        .workspace_create(Some("recover"), None, &std::collections::BTreeMap::new())
        .unwrap();
    let orphan = orchestratr::driver::AgentStartParams {
        name: "orphan".into(),
        argv: vec!["sh".into(), "-c".into(), "sleep 120".into()],
        cwd: None,
        env: std::collections::BTreeMap::new(),
        focus: false,
        split: None,
        tab_id: None,
        workspace_id: Some(ws.workspace.workspace_id.clone()),
    };
    let orphan_pane = ts.driver.agent_start(&orphan).unwrap().pane_id;
    let _ = ts.driver.pane_close(&ws.root_pane.pane_id);
    assert!(ts
        .driver
        .pane_list(None)
        .unwrap()
        .iter()
        .any(|p| p.pane_id == orphan_pane));

    // Restart → reconcile closes the orphan and fails the row.
    ts.spawn_server();
    assert_eq!(ts.status("crash-uuid-1").as_deref(), Some("ended"));
    assert!(
        wait_until(Duration::from_secs(10), || {
            !ts.driver
                .pane_list(None)
                .unwrap()
                .iter()
                .any(|p| p.pane_id == orphan_pane)
        }),
        "orphan pane should be closed by the reconciler"
    );
}
