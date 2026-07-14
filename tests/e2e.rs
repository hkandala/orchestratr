//! End-to-end driver + harness tests against a **live herdr** and the mock provider.
//!
//! Gated behind `ORCR_E2E=1` so unit runs stay fast. Every test runs in an isolated
//! `ORCR_HOME` tempdir and a **disposable** herdr session named `orcr_test_<rand>`,
//! torn down (`session stop` + `session delete`) by a drop guard. The user's `default`
//! herdr session is never touched.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test e2e -- --nocapture`

use orchestratr::driver::{normalize_done, AgentStatus, HerdrBinary, HerdrDriver, PaneAgentState};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").as_deref() == Ok("1")
}

/// The mock-agent binary built by cargo for this integration test.
fn mock_agent_bin() -> String {
    env!("CARGO_BIN_EXE_orcr-mock-agent").to_string()
}

/// A disposable herdr session + bound driver, cleaned up on drop.
struct Harness {
    bin: HerdrBinary,
    session: String,
    driver: HerdrDriver,
    _home: tempfile::TempDir,
}

impl Harness {
    fn start() -> Harness {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = HerdrBinary::discover(None).expect("herdr binary on PATH");
        // Fully random suffix — UUIDv7's leading hex is a slow-changing timestamp and
        // collides across near-simultaneous tests, so use v4 here.
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let session = format!("orcr_test_{}", &rand[..12]);
        // Bootstrap + connect can fail; ensure teardown even before the struct (and its
        // Drop guard) exists, so a partially-started session never leaks.
        let started = (|| {
            let socket = bin.ensure_session(&session)?;
            HerdrDriver::connect(&socket)
        })();
        let driver = match started {
            Ok(d) => d,
            Err(e) => {
                let _ = bin.session_stop(&session);
                let _ = bin.session_delete(&session);
                panic!("bootstrap disposable session failed: {e}");
            }
        };
        Harness {
            bin,
            session,
            driver,
            _home: home,
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Best-effort teardown; never touches the user's default session.
        let _ = self.bin.session_stop(&self.session);
        let _ = self.bin.session_delete(&self.session);
        // Known-issues #1: the disposable session must never leak (unconditional — the real
        // per-test guarantee). The literal `orcr` owned-session check is skipped when an external
        // `orcr` session pre-existed the run (a developer using orcr concurrently on the same box;
        // its default session is literally `orcr`) — not a leak this suite produced, and we must
        // not delete an in-use session to satisfy the check. Skipped mid-panic to avoid a double
        // panic masking the real failure.
        if !std::thread::panicking() {
            assert!(
                matches!(self.bin.find_session(&self.session), Ok(None)),
                "disposable session `{}` leaked after teardown",
                self.session
            );
            if !orcr_session_preexisted(&self.bin) {
                assert!(
                    matches!(self.bin.find_session("orcr"), Ok(None)),
                    "literal `orcr` session leaked after teardown"
                );
            }
        }
    }
}

/// Whether an `orcr` herdr session already existed when this test binary first probed (captured
/// once). Used to skip the shared-session leak check when a developer is running orcr
/// concurrently, without weakening the per-test disposable-session guarantee.
fn orcr_session_preexisted(bin: &HerdrBinary) -> bool {
    static SEEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SEEN.get_or_init(|| matches!(bin.find_session("orcr"), Ok(Some(_))))
}

/// A detached `orcr server` pid, SIGKILLed on drop (belt-and-suspenders teardown so a panic
/// before an explicit `server.stop` can't orphan a server against a soon-deleted session).
struct ServerProc(u32);

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = std::process::Command::new("kill")
            .args(["-9", &self.0.to_string()])
            .status();
    }
}

/// Poll `f` until it returns true or the deadline elapses.
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

#[test]
fn e2e_handshake_and_enumeration() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let h = Harness::start();

    // Handshake succeeded at connect; protocol is the one we require.
    assert_eq!(h.driver.protocol(), 16);
    let pong = h.driver.ping().unwrap();
    assert_eq!(pong.protocol, 16);

    // Our disposable session is enumerable via the binary.
    let sessions = h.bin.session_list().unwrap();
    assert!(
        sessions.iter().any(|s| s.name == h.session),
        "disposable session should be listed"
    );
    // The default session must still be present and untouched.
    assert!(
        sessions.iter().any(|s| s.name == "default"),
        "default session should be present"
    );

    // Session-scoped reads round-trip.
    let ws = h.driver.workspace_list().unwrap();
    assert!(!ws.is_empty(), "a fresh session has at least one workspace");
    let _snap = h.driver.session_snapshot().unwrap();
}

#[test]
fn e2e_bootstrap_is_idempotent() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let h = Harness::start();
    // Re-ensuring the same session returns the same running socket, no new server.
    let again: PathBuf = h.bin.ensure_session(&h.session).unwrap();
    assert_eq!(again, h.driver.socket_path());
}

#[test]
fn e2e_agent_lifecycle_and_state_reporting() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let h = Harness::start();
    let d = &h.driver;

    // Create a dedicated workspace and start the mock agent in it.
    let created = d
        .workspace_create(Some("e2e"), None, &BTreeMap::new())
        .unwrap();
    let ws = created.workspace.workspace_id.clone();

    let mut env = BTreeMap::new();
    env.insert("ORCR_ID".to_string(), "e2e-agent".to_string());
    // Silence the mock's own state reports so this test's explicit reports are the only
    // source driving pane state (deterministic).
    env.insert("ORCR_MOCK_NO_REPORT".to_string(), "1".to_string());
    let params = orchestratr::driver::AgentStartParams {
        name: "worker".into(),
        argv: vec![mock_agent_bin()],
        cwd: None,
        env,
        focus: false,
        split: None,
        tab_id: None,
        workspace_id: Some(ws.clone()),
    };
    let agent = d.agent_start(&params).unwrap();
    let pane = agent.pane_id.clone();
    assert!(!agent.terminal_id.is_empty());

    // The pane shows up in pane.list / pane.get and agent.list.
    assert!(wait_until(Duration::from_secs(5), || {
        d.pane_list(Some(&ws))
            .unwrap()
            .iter()
            .any(|p| p.pane_id == pane)
    }));
    let got = d.pane_get(&pane).unwrap();
    assert_eq!(got.pane_id, pane);
    assert!(d.agent_list().unwrap().iter().any(|a| a.pane_id == pane));

    // Input delivery (two-call rule) round-trips to the live pane.
    d.pane_send_text(&pane, "hello").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    d.pane_send_keys(&pane, &["Enter"]).unwrap();

    // Close the agent pane; the workspace still has its root shell pane.
    d.pane_close(&pane).unwrap();
    assert!(wait_until(Duration::from_secs(5), || {
        !d.pane_list(Some(&ws))
            .unwrap()
            .iter()
            .any(|p| p.pane_id == pane)
    }));

    // State reporting through herdr's integration mechanism round-trips. Use a quiet
    // pane so herdr's own screen-detection doesn't compete with the reports.
    let quiet = orchestratr::driver::AgentStartParams {
        name: "quiet".into(),
        argv: vec!["sh".into(), "-c".into(), "sleep 120".into()],
        cwd: None,
        env: BTreeMap::new(),
        focus: false,
        split: None,
        tab_id: None,
        workspace_id: Some(ws.clone()),
    };
    let qpane = d.agent_start(&quiet).unwrap().pane_id;
    for state in [
        PaneAgentState::Working,
        PaneAgentState::Idle,
        PaneAgentState::Blocked,
    ] {
        d.pane_report_agent(&qpane, "orcr:test", "mock", state, Some("sess-1"))
            .unwrap();
        // herdr surfaces a working→idle transition as `done` (its turn-complete signal),
        // which orcr normalizes to `idle` (spec §5.6). Compare on the normalized value.
        let want = match state {
            PaneAgentState::Working => AgentStatus::Working,
            PaneAgentState::Idle => AgentStatus::Idle,
            PaneAgentState::Blocked => AgentStatus::Blocked,
            PaneAgentState::Unknown => AgentStatus::Unknown,
        };
        assert!(
            wait_until(Duration::from_secs(5), || {
                d.pane_get(&qpane)
                    .map(|p| normalize_done(p.agent_status) == want)
                    .unwrap_or(false)
            }),
            "reported {:?} state should be visible (normalized)",
            state
        );
    }
    d.pane_close(&qpane).unwrap();
}

#[test]
fn e2e_empty_workspace_auto_removal() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let h = Harness::start();
    let d = &h.driver;

    // Create a workspace (herdr gives it a root pane) and confirm it exists.
    let created = d
        .workspace_create(Some("scratch"), None, &BTreeMap::new())
        .unwrap();
    let ws = created.workspace.workspace_id.clone();
    let root = created.root_pane.pane_id.clone();
    assert!(d
        .workspace_list()
        .unwrap()
        .iter()
        .any(|w| w.workspace_id == ws));

    // Close its only pane → herdr removes the now-empty workspace (spec §5.2).
    d.pane_close(&root).unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || {
            !d.workspace_list()
                .unwrap()
                .iter()
                .any(|w| w.workspace_id == ws)
        }),
        "workspace should be auto-removed once its last pane is closed"
    );
}

/// M1 e2e: the orcr server's `server.status` reports live herdr reachability + integrations
/// against a **disposable** session. Runs the real `orcr` binary over a throwaway ORCR_HOME.
#[test]
fn e2e_server_status_reports_herdr() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    use orchestratr::home::Home;
    use orchestratr::server::Client;
    use std::process::{Command, Stdio};

    // A disposable herdr session (torn down by the Harness drop guard).
    let h = Harness::start();

    // A throwaway orcr home whose config points at the disposable session.
    let orcr_home = tempfile::tempdir().expect("orcr home");
    std::fs::write(
        orcr_home.path().join("config.json"),
        format!(r#"{{"herdr":{{"session":"{}"}}}}"#, h.session),
    )
    .unwrap();

    let orcr = env!("CARGO_BIN_EXE_orcr");
    let start = Command::new(orcr)
        .args(["server", "start"])
        .env("ORCR_HOME", orcr_home.path())
        // Belt-and-suspenders (known-issues #1): pin the disposable session on the child too, so
        // a config-less orphan can never bootstrap the literal `orcr` session — matching every
        // other e2e harness.
        .env("ORCR_HERDR_SESSION", &h.session)
        .env("ORCR_DISABLE_DISCOVERY", "1")
        .stdin(Stdio::null())
        .output()
        .expect("orcr server start");
    assert!(start.status.success(), "orcr server start should succeed");

    let client = Client::new(Home::at(orcr_home.path()).socket_path());
    client
        .wait_for_ready(Duration::from_secs(10))
        .expect("orcr server ready");

    let status = client
        .request("server.status", serde_json::json!({}))
        .expect("server.status");

    // Track the detached server so a panic before `server.stop` can't leave an orphan running
    // against a soon-deleted session. Armed before the asserts; dropped (killed) before the
    // Harness teardown that stops+deletes the session.
    let _server = ServerProc(status["pid"].as_u64().expect("server pid") as u32);

    // herdr binary reachable; the owned (disposable) session is running and pingable.
    let herdr = &status["herdr"];
    assert_eq!(
        herdr["reachable"], true,
        "herdr binary should be discovered"
    );
    assert_eq!(
        herdr["session"], h.session,
        "owned session should be reported"
    );
    assert_eq!(
        herdr["session_running"], true,
        "disposable session is running"
    );
    assert_eq!(herdr["protocol"], 16, "pinged protocol should be 16");

    // Integrations: claude has an orcr built-in and (in this env) a herdr integration.
    let claude = &status["integrations"]["claude"];
    assert_eq!(
        claude["orcr"], true,
        "claude has an orcr built-in integration"
    );
    assert!(claude["herdr"].is_boolean(), "claude herdr layer reported");

    // Counts are all zero for M1 (no agents yet).
    assert_eq!(status["counts"]["live"], 0);

    let _ = client.request("server.stop", serde_json::json!({}));
}
