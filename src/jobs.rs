use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Config;
use crate::engine::{Engine, RunMode, RunRequest};
use crate::herdr::HerdrClient;
use crate::profile;
use crate::store::{EventRow, JobRow, Store};

pub const AUTO_FALLBACK_SECS: u64 = 600;
pub const AUTO_MIN_SECS: u64 = 30;
pub const AUTO_MAX_SECS: u64 = 24 * 60 * 60;
pub const TICK_ON_POLL_SECS: u64 = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopSpec {
    pub harness: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,
    pub every: EverySpec,
    pub tick_on: Option<String>,
    pub max: Option<u64>,
    pub until: Option<String>,
    pub name: Option<String>,
    pub model: String,
    pub effort: String,
    pub cwd: String,
    pub timeout_s: u64,
    pub keep: bool,
    pub mode: String,
    pub worktree: bool,
    pub last_next_reason: Option<String>,
    pub last_tick_agent: Option<String>,
    pub last_tick_response: Option<String>,
    pub tick_probe: Option<TickProbeState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "seconds")]
pub enum EverySpec {
    Fixed(u64),
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TickProbeState {
    pub last_exit: Option<i32>,
    pub last_stdout: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextCheck {
    pub seconds: u64,
    pub reason: String,
    pub clamped: bool,
}

impl TickProbeState {
    pub fn new() -> Self {
        Self {
            last_exit: None,
            last_stdout: None,
        }
    }

    pub fn observe(&mut self, exit: i32, stdout: String) -> bool {
        let fires = match (self.last_exit, self.last_stdout.as_deref()) {
            (None, None) => exit == 0,
            (Some(previous_exit), Some(previous_stdout)) => {
                (previous_exit != 0 && exit == 0) || previous_stdout != stdout
            }
            _ => exit == 0,
        };
        self.last_exit = Some(exit);
        self.last_stdout = Some(stdout);
        fires
    }
}

impl Default for TickProbeState {
    fn default() -> Self {
        Self::new()
    }
}

impl LoopSpec {
    pub fn prompt_text(&self) -> Result<String> {
        match (&self.prompt, &self.prompt_file) {
            (Some(prompt), None) => Ok(prompt.clone()),
            (None, Some(path)) => fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {path}")),
            _ => Err(anyhow!("loop needs exactly one prompt source")),
        }
    }

    pub fn next_interval_secs(&self) -> u64 {
        match self.every {
            EverySpec::Fixed(seconds) => seconds,
            EverySpec::Auto => AUTO_FALLBACK_SECS,
        }
    }
}

pub fn parse_next_check(text: &str) -> Option<NextCheck> {
    let re = Regex::new(r"(?im)^\s*NEXT_CHECK:\s*([0-9]+)\s*([smhd]?)\s*(?:[-\u{2014}]\s*)?(.*)$")
        .expect("NEXT_CHECK regex compiles");
    let caps = re.captures_iter(text).last()?;
    let amount = caps.get(1)?.as_str().parse::<u64>().ok()?;
    let unit = caps.get(2).map(|m| m.as_str()).unwrap_or("");
    let raw_seconds = amount.checked_mul(match unit {
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => 1,
    })?;
    let seconds = raw_seconds.clamp(AUTO_MIN_SECS, AUTO_MAX_SECS);
    let reason = caps
        .get(3)
        .map(|m| m.as_str().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "no reason supplied".to_string());
    Some(NextCheck {
        seconds,
        reason,
        clamped: seconds != raw_seconds,
    })
}

pub fn auto_preamble() -> &'static str {
    "End your response with exactly one final line: NEXT_CHECK: <duration> - <reason>. Duration examples: 30s, 5m, 2h."
}

pub fn append_job_event(
    store: &Store,
    kind: &str,
    id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    store.append_event(&EventRow::new(
        Utc::now().to_rfc3339(),
        kind,
        Some(id.to_string()),
        payload.to_string(),
    ))?;
    Ok(())
}

pub fn run_loop_tick(
    config: &Config,
    store: &mut Store,
    herdr: HerdrClient,
    job: &mut JobRow,
) -> Result<()> {
    let mut spec: LoopSpec = serde_json::from_str(&job.spec_json)?;
    let mut prompt = spec.prompt_text()?;
    if spec.every == EverySpec::Auto {
        prompt = format!("{}\n\n{}", auto_preamble(), prompt);
    }
    let profile = profile::lookup(&spec.harness)
        .ok_or_else(|| anyhow!("unknown harness: {}", spec.harness))?;
    let cwd = PathBuf::from(&spec.cwd);
    append_job_event(
        store,
        "job.tick.start",
        &job.id,
        json!({"runs_count": job.runs_count + 1}),
    )?;
    let mut engine = Engine::new(config, store, herdr);
    let result = engine.run(
        profile.as_ref(),
        RunRequest {
            name: spec.name.clone(),
            parent_id: Some(job.id.clone()),
            mode: if spec.mode == "exec" {
                RunMode::Exec
            } else {
                RunMode::Tui
            },
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            cwd,
            timeout_s: spec.timeout_s,
            keep: spec.keep,
            prompt,
            wait: true,
        },
    )?;
    let store = engine.store_mut();
    job.runs_count += 1;
    spec.last_tick_agent = Some(result.agent.id.clone());
    spec.last_tick_response = Some(result.turn.response_path.clone());

    let mut ended_reason = None;
    let mut next_secs = spec.next_interval_secs();
    if let Some(response) = result.response.as_ref() {
        if spec.every == EverySpec::Auto {
            if let Some(next) = parse_next_check(&response.text) {
                next_secs = next.seconds;
                spec.last_next_reason = Some(next.reason);
            } else {
                spec.last_next_reason = Some("NEXT_CHECK missing; fallback 10m".to_string());
            }
        }
        if let Some(until) = spec.until.as_deref() {
            if Regex::new(until)?.is_match(&response.text) {
                ended_reason = Some("until_matched".to_string());
            }
        }
    }
    if spec.max.is_some_and(|max| {
        u64::try_from(job.runs_count)
            .map(|runs| runs >= max)
            .unwrap_or(true)
    }) {
        ended_reason = Some("max_ticks".to_string());
    }
    job.spec_json = serde_json::to_string(&spec)?;
    if let Some(reason) = ended_reason {
        job.status = "done".to_string();
        job.ended_reason = Some(reason);
        job.next_run_at = None;
    } else {
        let next = Utc::now() + ChronoDuration::seconds(i64::try_from(next_secs)?);
        job.next_run_at = Some(next.to_rfc3339());
    }
    store.update_job(job)?;
    append_job_event(
        store,
        "job.tick.complete",
        &job.id,
        json!({"runs_count": job.runs_count, "next_run_at": job.next_run_at}),
    )?;
    Ok(())
}

pub fn tick_on_fires(spec: &mut LoopSpec) -> Result<bool> {
    let Some(command) = spec.tick_on.as_deref() else {
        return Ok(true);
    };
    let output = Command::new("sh").arg("-c").arg(command).output()?;
    let exit = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut state = spec.tick_probe.take().unwrap_or_default();
    let fires = state.observe(exit, stdout);
    spec.tick_probe = Some(state);
    Ok(fires)
}

pub fn parse_rfc3339_utc(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_next_check_and_clamps() {
        let fast = parse_next_check("done\nNEXT_CHECK: 5s - soon").unwrap();
        assert_eq!(fast.seconds, 30);
        assert!(fast.clamped);
        assert_eq!(fast.reason, "soon");

        let normal = parse_next_check("NEXT_CHECK: 12m - wait for CI").unwrap();
        assert_eq!(normal.seconds, 720);
        assert!(!normal.clamped);

        let slow = parse_next_check("NEXT_CHECK: 99d - later").unwrap();
        assert_eq!(slow.seconds, 86_400);
    }

    #[test]
    fn tick_probe_detects_exit_flip_and_stdout_change() {
        let mut state = TickProbeState::new();
        assert!(!state.observe(1, "a".to_string()));
        assert!(state.observe(0, "a".to_string()));
        assert!(!state.observe(0, "a".to_string()));
        assert!(state.observe(0, "b".to_string()));
    }

    #[test]
    fn loop_spec_duration_defaults() {
        let mut spec = LoopSpec {
            harness: "mock".to_string(),
            prompt: Some("hi".to_string()),
            prompt_file: None,
            every: EverySpec::Fixed(42),
            tick_on: None,
            max: None,
            until: None,
            name: None,
            model: String::new(),
            effort: String::new(),
            cwd: ".".to_string(),
            timeout_s: 600,
            keep: false,
            mode: "tui".to_string(),
            worktree: false,
            last_next_reason: None,
            last_tick_agent: None,
            last_tick_response: None,
            tick_probe: None,
        };
        assert_eq!(spec.next_interval_secs(), 42);
        spec.every = EverySpec::Auto;
        assert_eq!(spec.next_interval_secs(), AUTO_FALLBACK_SECS);
    }
}
