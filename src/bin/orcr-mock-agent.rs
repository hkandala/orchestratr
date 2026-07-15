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
//! - `ORCR_MOCK_NO_TRANSCRIPT`  — if set, don't write/report a transcript (tests that assert
//!   `transcript_unavailable`).
//! - `ORCR_MOCK_LATE_TRANSCRIPT_MS` — if > 0, report `idle` at end of turn and only write the
//!   transcript this many ms later (simulates a real provider that reports idle before flushing
//!   its native transcript — the known-issues #2 gc-immediate race).
//! - `ORCR_MOCK_DELAY_WORKING_MS` — if > 0, stay `idle` for this long after receiving a line
//!   before reporting `working` (exercises orcr's known-issues #2 submit-confirm re-send loop).
//! - `ORCR_MOCK_DROP_FIRST_SENDS` — silently DISCARD the first N input lines (no echo, stays
//!   `idle`), simulating a provider TUI that isn't yet accepting input on a slow boot (the
//!   `send_text` itself is dropped). orcr's submit-confirm must re-deliver the FULL prompt — a
//!   bare-Enter re-send never gets the prompt through (known-issues #2 / E02).
//!
//! Per-turn `@`-directives in the prompt: `@turn_ms=` `@tool_gaps=` `@gap_ms=` `@block`
//! `@say=<word>` (the exact response text) `@write=<relpath>` (also write the response to
//! `$ORCR_AGENT_DATA_DIR/<relpath>` — the file convention, §8). It writes a claude-format
//! `transcript.jsonl` into its data dir so `logs`/`ask` resolve (§11.4 `mock` adapter).
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
    /// A claude-format transcript the mock writes into its data dir so `logs`/`ask` have a
    /// readable file (orcr's `mock` locator reads it from the data dir). `None` when no data dir.
    transcript_path: Option<String>,
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
                transcript_path: None,
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
        // promptly (orcr's spawn pipeline waits for it, §11.1). herdr surfaces this as an
        // `id`-kind pointer which orcr captures reliably; orcr's `mock`-provider transcript
        // locator then reads the transcript from the agent's data dir (below).
        let session_id = Some(
            std::env::var("ORCR_MOCK_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "mock_session".to_string()),
        );
        // The transcript file in the agent's data dir (claude-format JSONL the mock appends to),
        // so recipe/SDK e2e can exercise `logs`/`ask` — self-contained, never touching a real
        // provider's home. Created up front so the first `logs` read finds a file.
        // `ORCR_MOCK_NO_TRANSCRIPT` suppresses it (for tests that assert transcript_unavailable).
        let transcript_path = std::env::var("ORCR_MOCK_NO_TRANSCRIPT")
            .is_err()
            .then(|| {
                std::env::var("ORCR_AGENT_DATA_DIR")
                    .ok()
                    .filter(|s| !s.is_empty())
            })
            .flatten()
            .map(|dir| {
                let p = std::path::Path::new(&dir).join("transcript.jsonl");
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p);
                p.to_string_lossy().to_string()
            });
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
            transcript_path,
        }
    }

    fn report(&self, state: PaneAgentState) {
        if let Some(d) = &self.driver {
            // Report a short `id`-kind session pointer (herdr captures it reliably); orcr's
            // `mock`-provider transcript locator reads the transcript from the agent's data dir.
            let _ = d.pane_report_agent(
                &self.pane_id,
                "orcr:mock",
                &self.agent,
                state,
                self.session_id.as_deref(),
            );
        }
    }

    /// Append a user prompt + assistant response to the claude-format transcript (§11.4), so a
    /// caller's `lastResponse()`/`ask()` reads real text back.
    fn append_transcript(&self, prompt: &str, response: &str) {
        let Some(path) = &self.transcript_path else {
            return;
        };
        use std::io::Write as _;
        let now = chrono::Utc::now().to_rfc3339();
        let user = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": prompt },
        });
        let asst = serde_json::json!({
            "type": "assistant",
            "timestamp": now,
            "message": { "role": "assistant", "content": response },
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{user}");
            let _ = writeln!(f, "{asst}");
        }
    }
}

/// Per-turn behavior, from env defaults overridden by `@key=val` tokens in the prompt.
struct Directives {
    turn_ms: u64,
    tool_gaps: u64,
    gap_ms: u64,
    block: bool,
    /// `@say=<word>` — the exact response text to emit (single token; default echoes the prompt).
    say: Option<String>,
    /// `@write=<relpath>` — also write the response to `$ORCR_AGENT_DATA_DIR/<relpath>` (the
    /// file convention, §8), so fan-out/generate recipes can read a guaranteed-format answer.
    write: Option<String>,
}

impl Directives {
    fn parse(prompt: &str, turn_ms: u64, tool_gaps: u64, gap_ms: u64, block: bool) -> Directives {
        let mut d = Directives {
            turn_ms,
            tool_gaps,
            gap_ms,
            block,
            say: None,
            write: None,
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
            } else if let Some(v) = tok.strip_prefix("say=") {
                d.say = Some(v.to_string());
            } else if let Some(v) = tok.strip_prefix("write=") {
                d.write = Some(v.to_string());
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
    // Simulate a real provider that reports idle before flushing its native transcript: when set,
    // the transcript is appended this many ms *after* the end-of-turn idle report.
    let late_transcript_ms: u64 = std::env::var("ORCR_MOCK_LATE_TRANSCRIPT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // After receiving a line, wait this many ms before reporting `working` — the pane stays
    // `idle` in the meantime, so orcr's submit-confirmation loop observes not-yet-submitted and
    // re-sends Enter (the extra empty line is buffered and skipped). Exercises the known-issues #2
    // submit-confirm re-send path with the mock.
    let delay_working_ms: u64 = std::env::var("ORCR_MOCK_DELAY_WORKING_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    // Simulate a not-yet-ready TUI that drops early input: discard the first N received lines
    // entirely (no echo, no state change), so orcr's submit-confirm must re-deliver the whole
    // prompt (a bare-Enter re-send never lands it). Exercises the E02 hardening.
    let mut drop_remaining: u64 = std::env::var("ORCR_MOCK_DROP_FIRST_SENDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

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

    // Disable terminal echo on stdin so herdr `pane.read` reflects only what the mock explicitly
    // prints (its banner + per-turn acceptance echo below), not the raw characters herdr types via
    // `pane.send_text` (a cooked pty echoes those). This models a real TUI's own input handling and
    // lets orcr's submit-confirm reliably distinguish a DROPPED send (nothing on screen) from an
    // ACCEPTED one (the mock's `> …` echo) — the basis of the E02 re-delivery test. Canonical mode
    // stays on, so line reads are unaffected. A no-op when stdin isn't a tty.
    unsafe {
        let fd = libc::STDIN_FILENO;
        if libc::isatty(fd) == 1 {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut term) == 0 {
                term.c_lflag &= !libc::ECHO;
                let _ = libc::tcsetattr(fd, libc::TCSANOW, &term);
            }
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
        // A not-yet-ready TUI drops this input entirely: no echo, no state change (stays idle).
        // orcr's submit-confirm reads the pane, sees the prompt is absent, and re-delivers.
        if drop_remaining > 0 {
            drop_remaining -= 1;
            continue;
        }
        // Echo the accepted prompt to stdout immediately so herdr `pane.read` reflects that the
        // input landed (a real TUI shows typed/submitted text) — this lets orcr's submit-confirm
        // distinguish "accepted, working delayed" from "dropped" and avoid double-delivery.
        println!("> {prompt}");
        let _ = std::io::stdout().flush();

        // Per-turn directives embedded in the prompt (`@turn_ms=..`, `@tool_gaps=..`,
        // `@gap_ms=..`, `@block`) override the env defaults — this is how e2e drives a
        // specific turn shape per agent without needing to inject pane env (§5.6 matrix).
        let d = Directives::parse(prompt, turn_ms, tool_gaps_env, gap_ms_env, block_env);

        // Optionally stay idle for a beat after receiving input, so orcr's submit-confirm loop
        // sees not-yet-submitted and re-sends Enter before this turn reports `working`.
        if delay_working_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_working_ms));
        }
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
        // The response text: an explicit `@say=` value, else the echoed prompt. The sentinel
        // `DONE` is appended so file-convention callers have a stable marker.
        let response = match &d.say {
            Some(s) => format!("{s}\nDONE"),
            None => format!("RESPONSE: {prompt}\nDONE"),
        };
        // Default: write the transcript BEFORE reporting idle (so the completion monitor never
        // sees idle without a settled transcript). The `late_transcript_ms` path defers it to
        // *after* the idle report, reproducing a real provider's report-idle-then-flush race.
        if late_transcript_ms == 0 {
            reporter.append_transcript(prompt, &response);
        }
        // The file convention (§8): write the response to a data-dir file on request.
        if let Some(rel) = &d.write {
            if let Ok(dir) = std::env::var("ORCR_AGENT_DATA_DIR") {
                if !dir.is_empty() {
                    let _ = std::fs::write(std::path::Path::new(&dir).join(rel), &response);
                }
            }
        }
        println!("{response}");
        let _ = std::io::stdout().flush();
        // Turn complete: idle (or blocked, until the next input clears it).
        if d.block {
            reporter.report(PaneAgentState::Blocked);
        } else {
            reporter.report(PaneAgentState::Idle);
        }
        if late_transcript_ms > 0 {
            std::thread::sleep(Duration::from_millis(late_transcript_ms));
            reporter.append_transcript(prompt, &response);
        }

        turns += 1;
        if once || (exit_after > 0 && turns >= exit_after) {
            break;
        }
    }
}
