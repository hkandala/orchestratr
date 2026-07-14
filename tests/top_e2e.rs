//! M6 `top` e2e (spec §7 acceptance): the data path — `watch.open` snapshot + event stream
//! (§11.6) — and the pure tree/filter/lineage model rendered against a **live** store driven
//! by a scripted storm of mock agents over live herdr.
//!
//! Gated behind `ORCR_E2E=1`. Each test runs a real `orcr` server over a throwaway `ORCR_HOME`
//! whose config points at a **disposable** herdr session (`orcr_test_<rand>`), torn down by a
//! drop guard. The user's `default` session is never touched.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test top_e2e -- --test-threads=1 --nocapture`

use orchestratr::driver::HerdrBinary;
use orchestratr::home::Home;
use orchestratr::path::Pattern;
use orchestratr::server::Client;
use orchestratr::top::model::{build_tree, Snapshot, TopFilter};
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
        if let Err(e) = bin.ensure_session(&session) {
            let _ = bin.session_stop(&session);
            let _ = bin.session_delete(&session);
            panic!("disposable session bootstrap failed: {e}");
        }
        std::fs::write(
            home.path().join("config.json"),
            format!(r#"{{"herdr":{{"session":"{session}"}},"concurrency":{{"max":30}}}}"#),
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

    /// Spawn a mock agent at `path`, optionally with caller lineage; returns its uuid.
    fn run(&self, path: &str, caller: Option<(&str, &str)>) -> String {
        let mut p = json!({ "path": path, "agent": "mock" });
        if let Some((id, cpath)) = caller {
            p["caller_id"] = json!(id);
            p["caller_path"] = json!(cpath);
        }
        let r = self.request("agent.run", p).expect("agent.run");
        r["agent"]["uuid"].as_str().unwrap().to_string()
    }

    /// `agent.ls` uuids for a pattern (the authoritative node set to compare `top` against).
    fn ls_uuids(&self, pattern: Option<&str>) -> BTreeSet<String> {
        let mut params = json!({});
        if let Some(p) = pattern {
            params["pattern"] = json!(p);
        }
        let r = self.request("agent.ls", params).expect("agent.ls");
        r["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["uuid"].as_str().unwrap().to_string())
            .collect()
    }

    fn snapshot(&self) -> Value {
        self.request("api.snapshot", json!({}))
            .expect("api.snapshot")
    }

    fn kill(&self, target: &str) {
        let _ = self.request("agent.kill", json!({ "targets": [target], "force": true }));
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

/// Build the tree's agent-uuid node set from a snapshot doc under an optional pattern.
fn tree_uuids(snap_doc: &Value, pattern: Option<&str>) -> BTreeSet<String> {
    let snap = Snapshot::from_json(snap_doc);
    let filter = TopFilter {
        pattern: pattern.map(|p| Pattern::compile(p).unwrap()),
        ..Default::default()
    };
    build_tree(&snap, &filter).agent_uuids()
}

/// Script a mixed storm; returns the set of live uuids after it quiesces.
fn drive_storm(ts: &TestServer) {
    // A wide/deep tree with a cross-scope lineage edge.
    let fixer = ts.run("fix_build/fixer", None);
    // A child created at an ABSOLUTE path outside its parent's scope (cross-scope lineage).
    ts.run("/verify/checker", Some((&fixer, "fix_build/fixer")));
    for i in 0..6 {
        ts.run(&format!("review/fanout/file_{i}"), None);
    }
    ts.run("review/lint", None);
    ts.run("reviewer/a", None);
    ts.run("reviewer/deep/b", None);
    ts.run("solo", None);
    // Kill one to exercise removal.
    ts.kill("review/lint");
    // Let statuses settle and the kill land.
    std::thread::sleep(Duration::from_millis(1500));
}

// --- Correctness: snapshot renders the store's final state exactly (golden node set) --------

#[test]
fn e2e_snapshot_tree_matches_store() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    drive_storm(&ts);

    // The `watch.open` pinned snapshot and the store's `agent ls` must agree exactly.
    let (initial, _sub) = ts
        .client()
        .open_stream("watch.open", json!({}))
        .expect("watch.open");
    let watch_snap = initial["snapshot"].clone();
    let tree_set = tree_uuids(&watch_snap, None);
    let ls_set = ts.ls_uuids(None);
    assert_eq!(
        tree_set, ls_set,
        "tree node set from watch.open snapshot diverged from agent ls"
    );
    assert!(!ls_set.is_empty(), "storm produced agents");

    // Cross-scope lineage: checker sits under `verify` with a `↖ fix_build/fixer` annotation,
    // placed exactly once (never re-rooted under its parent).
    let snap = Snapshot::from_json(&watch_snap);
    let tree = build_tree(&snap, &TopFilter::default());
    let checker = &tree.roots["verify"].children["checker"];
    assert_eq!(checker.lineage.as_deref(), Some("fix_build/fixer"));
    let lines = tree.structure_lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.contains("verify/checker"))
            .count(),
        1,
        "checker placed exactly once"
    );
    assert!(!tree
        .roots
        .get("fix_build")
        .and_then(|n| n.children.get("fixer"))
        .map(|f| f.children.contains_key("checker"))
        .unwrap_or(false));
}

// --- Filters produce the same node sets as the equivalent `agent ls` query -----------------

#[test]
fn e2e_filters_match_ls() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    drive_storm(&ts);
    let snap = ts.snapshot();

    for pattern in [
        "review",
        "review/*",
        "review/**",
        "reviewer/**",
        "review/fanout/*",
    ] {
        let tree_set = tree_uuids(&snap, Some(pattern));
        let ls_set = ts.ls_uuids(Some(pattern));
        assert_eq!(
            tree_set, ls_set,
            "pattern `{pattern}`: top node set != agent ls node set"
        );
    }
}

// --- The event stream converges: a post-snapshot change is delivered and re-rendered -------

#[test]
fn e2e_stream_delivers_post_snapshot_change() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    ts.run("base/one", None);

    // Open the stream, then make a change AFTER the pinned snapshot.
    let (initial, mut sub) = ts
        .client()
        .open_stream("watch.open", json!({}))
        .expect("watch.open");
    let seq0 = initial["snapshot_seq"].as_i64().unwrap();
    let before = tree_uuids(&initial["snapshot"], None);

    let new_uuid = ts.run("base/two", None);
    assert!(
        !before.contains(&new_uuid),
        "new agent not in the pinned snapshot"
    );

    // At least one event with seq > snapshot_seq arrives (no gap, no double-apply).
    sub.set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut saw_new_event = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match sub.next_event() {
            Ok(Some(frame)) => {
                if let Some(seq) = frame.get("seq").and_then(|s| s.as_i64()) {
                    if seq > seq0 {
                        saw_new_event = true;
                        break;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => continue, // read timeout — keep waiting
        }
    }
    assert!(
        saw_new_event,
        "no event delivered after the pinned snapshot"
    );

    // A coalesced refresh (fresh api.snapshot) now includes the new agent — the tree converges
    // to the store's final state.
    assert!(wait_until(Duration::from_secs(10), || {
        tree_uuids(&ts.snapshot(), None).contains(&new_uuid)
    }));
}

// --- Mid-storm server restart: watch reconnects and still matches the store ----------------

#[test]
fn e2e_restart_mid_storm_still_matches() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    drive_storm(&ts);
    let before = ts.ls_uuids(None);

    // Hard-kill the server mid-storm; the panes keep running and the store is intact (§6.4).
    let pid = ts.pid();
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    wait_until(Duration::from_secs(5), || ts.client().handshake().is_err());

    // Restart and re-open the watch — the reconnected snapshot still matches the store.
    ts.spawn_server();
    assert!(!before.is_empty());

    // The reconnected `watch.open` snapshot renders the same node set the store reports, and
    // the pre-restart live agents (whose panes survived) are all still present.
    assert!(wait_until(Duration::from_secs(15), || {
        let (initial, _sub) = match ts.client().open_stream("watch.open", json!({})) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let tree_set = tree_uuids(&initial["snapshot"], None);
        tree_set == ts.ls_uuids(None) && before.is_subset(&tree_set)
    }));
}

// --- Scale: many concurrent agents render from one consistent snapshot ----------------------

#[test]
fn e2e_scale_snapshot_consistent() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start();
    // 24 concurrent mock agents across a wide tree (cap=30 so none queue).
    for i in 0..24 {
        ts.run(&format!("s{}/phase/w{}", i % 6, i), None);
    }
    assert!(wait_until(Duration::from_secs(30), || {
        ts.ls_uuids(None).len() >= 24
    }));
    let snap = ts.snapshot();
    let start = Instant::now();
    let tree_set = tree_uuids(&snap, None);
    let build = start.elapsed();
    assert_eq!(tree_set, ts.ls_uuids(None));
    assert!(
        build < Duration::from_millis(50),
        "tree build over budget: {build:?}"
    );
}
