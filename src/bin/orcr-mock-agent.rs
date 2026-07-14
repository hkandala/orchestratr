//! `orcr-mock-agent` — a scriptable stand-in for a real agent TUI, the workhorse for
//! orcr's e2e suites (spec M0 "Test harness"). It behaves like an interactive agent:
//! prints a banner, reads prompts from stdin (as herdr's `pane.send_text` + Enter would
//! deliver them), "works" for a scriptable duration, echoes a response, and finishes the
//! turn on cue. When it can locate its herdr socket + pane, it reports state through
//! herdr's integration mechanism (`pane.report_agent`) so completion detection in later
//! milestones has a real signal to test against.
//!
//! Behavior is controlled entirely by env vars so it is trivial to drive from `agent.start`:
//!
//! - `ORCR_MOCK_BANNER`         — startup line (default "orcr-mock-agent ready").
//! - `ORCR_MOCK_TURN_MS`        — ms to "work" per turn (default 0).
//! - `ORCR_MOCK_ONCE`           — if set, process exactly one turn then exit.
//! - `ORCR_MOCK_EXIT_AFTER`     — exit after this many turns (0 = never; default 0).
//! - `ORCR_MOCK_HERDR_SOCKET`   — herdr session socket to report state on (optional).
//! - `ORCR_MOCK_PANE_ID`        — this pane's id, for state reporting (optional).
//! - `ORCR_MOCK_AGENT`          — provider name to report as (default "mock").
//! - `ORCR_MOCK_SESSION_ID`     — agent_session id to report (optional).
//!
//! The line `/quit` (or EOF) ends the process. Every response ends with the sentinel
//! `DONE` so file-convention-style callers have a stable marker.

use orchestratr::driver::{HerdrDriver, PaneAgentState};
use std::io::{BufRead, Write};
use std::time::Duration;

struct Reporter {
    driver: Option<HerdrDriver>,
    pane_id: String,
    agent: String,
    session_id: Option<String>,
}

impl Reporter {
    fn from_env() -> Reporter {
        let socket = std::env::var("ORCR_MOCK_HERDR_SOCKET")
            .ok()
            .filter(|s| !s.is_empty());
        let pane_id = std::env::var("ORCR_MOCK_PANE_ID").unwrap_or_default();
        let agent = std::env::var("ORCR_MOCK_AGENT").unwrap_or_else(|_| "mock".to_string());
        let session_id = std::env::var("ORCR_MOCK_SESSION_ID")
            .ok()
            .filter(|s| !s.is_empty());
        // Only build a driver if we have both a socket and a pane id to report on.
        let driver = match (socket, pane_id.is_empty()) {
            (Some(sock), false) => HerdrDriver::connect(sock).ok(),
            _ => None,
        };
        Reporter {
            driver,
            pane_id,
            agent,
            session_id,
        }
    }

    fn report(&self, state: PaneAgentState) {
        if let Some(d) = &self.driver {
            let _ = d.pane_report_agent(
                &self.pane_id,
                "orcr:mock",
                &self.agent,
                state,
                self.session_id.as_deref(),
            );
        }
    }
}

fn main() {
    let banner =
        std::env::var("ORCR_MOCK_BANNER").unwrap_or_else(|_| "orcr-mock-agent ready".into());
    let turn_ms: u64 = std::env::var("ORCR_MOCK_TURN_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let once = std::env::var("ORCR_MOCK_ONCE").is_ok();
    let exit_after: u64 = std::env::var("ORCR_MOCK_EXIT_AFTER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let reporter = Reporter::from_env();

    println!("{banner}");
    let _ = std::io::stdout().flush();
    // Announce readiness as idle (waiting for input).
    reporter.report(PaneAgentState::Idle);

    let stdin = std::io::stdin();
    let mut turns: u64 = 0;
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let prompt = line.trim();
        if prompt == "/quit" {
            break;
        }
        if prompt.is_empty() {
            continue;
        }

        // Turn begins: working.
        reporter.report(PaneAgentState::Working);
        if turn_ms > 0 {
            std::thread::sleep(Duration::from_millis(turn_ms));
        }
        println!("RESPONSE: {prompt}");
        println!("DONE");
        let _ = std::io::stdout().flush();
        // Turn complete: idle.
        reporter.report(PaneAgentState::Idle);

        turns += 1;
        if once || (exit_after > 0 && turns >= exit_after) {
            break;
        }
    }
}
