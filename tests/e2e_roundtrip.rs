use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use orchestratr::herdr::{discover_herdr, CompletionOutcome, HerdrClient, ResponseSource};
use orchestratr::rundir::{append_preamble, create_run_dir, response_path};
use tempfile::TempDir;
use uuid::Uuid;

const E2E_PREFIX: &str = "orcr-e2e-";

#[test]
fn mock_agent_roundtrip_and_scrape_fallback() -> Result<()> {
    if std::env::var("ORCR_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping e2e_roundtrip; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }

    let herdr_bin = discover_herdr("").context("real herdr binary is required for e2e")?;
    let session = format!("{E2E_PREFIX}{}", Uuid::new_v4());
    let _guard = SessionGuard::new(herdr_bin.clone(), session.clone());
    let client = HerdrClient::new(herdr_bin, session)
        .with_timings(Duration::from_millis(250), Duration::from_millis(250));
    client.ensure_session(Duration::from_secs(10))?;

    let store = tempfile::tempdir()?;
    let mock_bin = PathBuf::from(env!("CARGO_BIN_EXE_orcr-mock-agent"));

    let first = run_mock_turn(&client, &store, &mock_bin, "mock-file", "expected response")?;
    assert_eq!(first.source, ResponseSource::File);
    assert!(first.text.contains("# mock response"));
    assert!(first.text.contains("expected response"));

    let second = run_mock_turn(&client, &store, &mock_bin, "mock-scrape", "[[ignore-out]]")?;
    assert_eq!(second.source, ResponseSource::Scrape);
    assert!(second.text.contains("[[ignore-out]]"));
    assert!(second.text.contains("MOCK_DONE"));

    Ok(())
}

struct Captured {
    text: String,
    source: ResponseSource,
}

fn run_mock_turn(
    client: &HerdrClient,
    store: &TempDir,
    mock_bin: &Path,
    label: &str,
    prompt: &str,
) -> Result<Captured> {
    let run_dir = create_run_dir(store.path())?;
    let response = response_path(&run_dir, 1);
    let agent_id = Uuid::new_v4().to_string();
    let envs = vec![
        ("ORCR_ID".to_string(), agent_id.clone()),
        ("ORCR_DEPTH".to_string(), "0".to_string()),
        ("ORCR_STORE".to_string(), store.path().display().to_string()),
        ("ORCR_OUT".to_string(), response.display().to_string()),
    ];
    let argv = vec![mock_bin.display().to_string()];

    let agent = client.agent_start(label, &run_dir, &envs, &argv)?;
    client.wait_output(&agent.pane_id, "MOCK_READY", false, 10_000)?;

    let delivered = append_preamble(prompt, &response);
    client.send_input(&agent.pane_id, &delivered)?;
    let outcome = client.watch_output_markers(
        &agent.pane_id,
        "MOCK_DONE",
        "MOCK_BLOCKED",
        Duration::from_secs(15),
    )?;
    assert_eq!(outcome, CompletionOutcome::Done);

    let capture = client.capture_response_with_scrape(&agent.pane_id, &response)?;
    assert!(
        response.exists(),
        "response file should exist after file or scrape capture"
    );
    let text_on_disk = fs::read_to_string(&response)?;
    assert_eq!(text_on_disk, capture.text);

    let _ = client.pane_close(&agent.pane_id);
    Ok(Captured {
        text: capture.text,
        source: capture.source,
    })
}

struct SessionGuard {
    client: HerdrClient,
    session: String,
}

impl SessionGuard {
    fn new(bin: PathBuf, session: String) -> Self {
        Self {
            client: HerdrClient::new(bin, session.clone()),
            session,
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if !self.session.starts_with(E2E_PREFIX) {
            return;
        }
        let _ = self.client.session_stop(&self.session);
        let _ = self.client.session_delete(&self.session);
    }
}
