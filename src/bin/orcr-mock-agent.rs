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
//! - `ORCR_MOCK_HERDR_SOCKET`   — herdr session socket (optional; falls back to
//!   the herdr-injected `HERDR_SOCKET_PATH`).
//! - `ORCR_MOCK_PANE_ID`        — this pane's id (optional; falls back to the
//!   herdr-injected `HERDR_PANE_ID`).
//! - `ORCR_MOCK_AGENT`          — provider name to report as (default "mock").
//! - `ORCR_MOCK_SESSION_ID`     — agent_session id to report (optional).
//! - `ORCR_MOCK_TOOL_GAPS`      — simulate a tool-heavy turn: toggle working→idle→working
//!   this many times mid-turn (idle gaps shorter than the settle window; default 0).
//! - `ORCR_MOCK_GAP_MS`         — ms of each mid-turn idle gap (default 600).
//! - `ORCR_MOCK_BLOCK`          — if set, report `blocked` (not idle) at end of turn until the
//!   next input arrives (the `blocked` matrix case).
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
        // Opt out of self-reporting (tests that drive state explicitly avoid a
        // competing reporter).
        if std::env::var("ORCR_MOCK_NO_REPORT").is_ok() {
            return Reporter {
                driver: None,
                pane_id: String::new(),
                agent: String::new(),
                session_id: None,
            };
        }
        // Prefer an explicit override, else use herdr's own injected pane env
        // (HERDR_SOCKET_PATH / HERDR_PANE_ID) so no wiring from orcr is needed.
        let socket = std::env::var("ORCR_MOCK_HERDR_SOCKET")
            .or_else(|_| std::env::var("HERDR_SOCKET_PATH"))
            .ok()
            .filter(|s| !s.is_empty());
        let pane_id = std::env::var("ORCR_MOCK_PANE_ID")
            .or_else(|_| std::env::var("HERDR_PANE_ID"))
            .unwrap_or_default();
        let agent = std::env::var("ORCR_MOCK_AGENT").unwrap_or_else(|_| "mock".to_string());
        // A non-empty session id by default so herdr reports an `agent_session` pointer
        // promptly (orcr's spawn pipeline waits for it, §11.1).
        let session_id = Some(
            std::env::var("ORCR_MOCK_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "mock_session".to_string()),
        );
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

/// Per-turn behavior, from env defaults overridden by `@key=val` tokens in the prompt.
struct Directives {
    turn_ms: u64,
    tool_gaps: u64,
    gap_ms: u64,
    block: bool,
}

impl Directives {
    fn parse(prompt: &str, turn_ms: u64, tool_gaps: u64, gap_ms: u64, block: bool) -> Directives {
        let mut d = Directives {
            turn_ms,
            tool_gaps,
            gap_ms,
            block,
        };
        for tok in prompt.split_whitespace() {
            let tok = tok.trim_start_matches('@');
            if let Some(v) = tok.strip_prefix("turn_ms=") {
                if let Ok(n) = v.parse() {
                    d.turn_ms = n;
                }
            } else if let Some(v) = tok.strip_prefix("tool_gaps=") {
                if let Ok(n) = v.parse() {
                    d.tool_gaps = n;
                }
            } else if let Some(v) = tok.strip_prefix("gap_ms=") {
                if let Ok(n) = v.parse() {
                    d.gap_ms = n;
                }
            } else if tok == "block" {
                d.block = true;
            }
        }
        d
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
    let tool_gaps_env: u64 = std::env::var("ORCR_MOCK_TOOL_GAPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let gap_ms_env: u64 = std::env::var("ORCR_MOCK_GAP_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let block_env = std::env::var("ORCR_MOCK_BLOCK").is_ok();

    // Prove the env contract reached the pane (§5.3): dump every ORCR_* var to a file in the
    // agent's data dir, so e2e can assert it without needing to read pane env over herdr
    // (which the socket does not expose).
    if let Ok(dir) = std::env::var("ORCR_AGENT_DATA_DIR") {
        if !dir.is_empty() {
            let mut map = serde_json::Map::new();
            for (k, v) in std::env::vars() {
                if k.starts_with("ORCR_") {
                    map.insert(k, serde_json::Value::String(v));
                }
            }
            let _ = std::fs::write(
                std::path::Path::new(&dir).join("mock_env.json"),
                serde_json::to_vec_pretty(&serde_json::Value::Object(map)).unwrap_or_default(),
            );
        }
    }

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

        // Per-turn directives embedded in the prompt (`@turn_ms=..`, `@tool_gaps=..`,
        // `@gap_ms=..`, `@block`) override the env defaults — this is how e2e drives a
        // specific turn shape per agent without needing to inject pane env (§5.6 matrix).
        let d = Directives::parse(prompt, turn_ms, tool_gaps_env, gap_ms_env, block_env);

        // Turn begins: working.
        reporter.report(PaneAgentState::Working);
        // Optional tool-heavy turn: brief idle gaps that must not settle a completion.
        for _ in 0..d.tool_gaps {
            std::thread::sleep(Duration::from_millis(d.turn_ms.max(150)));
            reporter.report(PaneAgentState::Idle);
            std::thread::sleep(Duration::from_millis(d.gap_ms));
            reporter.report(PaneAgentState::Working);
        }
        if d.turn_ms > 0 {
            std::thread::sleep(Duration::from_millis(d.turn_ms));
        }
        println!("RESPONSE: {prompt}");
        println!("DONE");
        let _ = std::io::stdout().flush();
        // Turn complete: idle (or blocked, until the next input clears it).
        if d.block {
            reporter.report(PaneAgentState::Blocked);
        } else {
            reporter.report(PaneAgentState::Idle);
        }

        turns += 1;
        if once || (exit_after > 0 && turns >= exit_after) {
            break;
        }
    }
}
