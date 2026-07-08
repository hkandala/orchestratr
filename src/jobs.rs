use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
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
    #[serde(default)]
    pub max_runs: Option<u64>,
    #[serde(default)]
    pub max_duration_s: Option<u64>,
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
pub struct ScheduleSpec {
    pub harness: String,
    pub prompt: Option<String>,
    pub prompt_file: Option<String>,
    pub trigger: ScheduleTrigger,
    pub catchup: CatchupPolicy,
    pub name: Option<String>,
    pub model: String,
    pub effort: String,
    pub cwd: String,
    pub timeout_s: u64,
    pub keep: bool,
    pub mode: String,
    pub worktree: bool,
    pub max_runs: Option<u64>,
    pub max_duration_s: Option<u64>,
    pub last_tick_agent: Option<String>,
    pub last_tick_response: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum ScheduleTrigger {
    Cron { utc: String, local: String },
    At { at_utc: String, original: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatchupPolicy {
    Skip,
    Once,
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

impl ScheduleSpec {
    pub fn prompt_text(&self) -> Result<String> {
        match (&self.prompt, &self.prompt_file) {
            (Some(prompt), None) => Ok(prompt.clone()),
            (None, Some(path)) => fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt file {path}")),
            _ => Err(anyhow!("schedule needs exactly one prompt source")),
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
    let max_runs = spec.max_runs.or(spec.max);
    if max_runs.is_some_and(|max| {
        u64::try_from(job.runs_count)
            .map(|runs| runs >= max)
            .unwrap_or(true)
    }) {
        ended_reason = Some("max_runs".to_string());
    }
    if let Some(max_duration_s) = spec.max_duration_s {
        let created = parse_rfc3339_utc(&job.created_at)?;
        if Utc::now().signed_duration_since(created).num_seconds() >= i64::try_from(max_duration_s)?
        {
            ended_reason = Some("max_duration".to_string());
        }
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

pub fn run_schedule_tick(
    config: &Config,
    store: &mut Store,
    herdr: HerdrClient,
    job: &mut JobRow,
) -> Result<()> {
    let mut spec: ScheduleSpec = serde_json::from_str(&job.spec_json)?;
    let profile = profile::lookup(&spec.harness)
        .ok_or_else(|| anyhow!("unknown harness: {}", spec.harness))?;
    let prompt = spec.prompt_text()?;
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
            cwd: PathBuf::from(&spec.cwd),
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
    if let Some(max) = spec.max_runs {
        if u64::try_from(job.runs_count).unwrap_or(u64::MAX) >= max {
            ended_reason = Some("max_runs".to_string());
        }
    }
    if let Some(expires_at) = job
        .expires_at
        .as_deref()
        .and_then(|s| parse_rfc3339_utc(s).ok())
    {
        if Utc::now() >= expires_at {
            ended_reason = Some("expired".to_string());
        }
    }
    if ended_reason.is_none() {
        match &spec.trigger {
            ScheduleTrigger::At { .. } => ended_reason = Some("fired".to_string()),
            ScheduleTrigger::Cron { utc, .. } => {
                job.next_run_at = next_cron_after(utc, Utc::now()).map(|dt| dt.to_rfc3339());
            }
        }
    }
    job.spec_json = serde_json::to_string(&spec)?;
    if let Some(reason) = ended_reason {
        job.status = "done".to_string();
        job.ended_reason = Some(reason);
        job.next_run_at = None;
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

pub fn current_iana_timezone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

pub fn parse_timezone(value: &str) -> Result<Tz> {
    value
        .parse::<Tz>()
        .with_context(|| format!("invalid IANA timezone: {value}"))
}

pub fn parse_at_time(value: &str, tz: Tz, now_utc: DateTime<Utc>) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(value) {
        return Ok(dt.with_timezone(&Utc));
    }
    let trimmed = value.trim().to_ascii_lowercase();
    if let Some(rest) = trimmed.strip_prefix("in ") {
        let seconds = parse_friendly_duration(rest.trim())?;
        return Ok(now_utc + ChronoDuration::seconds(i64::try_from(seconds)?));
    }
    let now_local = now_utc.with_timezone(&tz);
    for prefix in ["today ", "tomorrow "] {
        if let Some(time_text) = trimmed.strip_prefix(prefix) {
            let time = NaiveTime::parse_from_str(time_text, "%H:%M")
                .with_context(|| format!("invalid friendly time `{value}`; expected HH:MM"))?;
            let date = if prefix == "tomorrow " {
                now_local.date_naive() + ChronoDuration::days(1)
            } else {
                now_local.date_naive()
            };
            let naive = date.and_time(time);
            return match tz.from_local_datetime(&naive) {
                LocalResult::Single(dt) => Ok(dt.with_timezone(&Utc)),
                LocalResult::Ambiguous(a, _) => Ok(a.with_timezone(&Utc)),
                LocalResult::None => Err(anyhow!("local time does not exist in timezone {tz}")),
            };
        }
    }
    Err(anyhow!(
        "unsupported --at form `{value}`; use RFC3339, `today HH:MM`, `tomorrow HH:MM`, or `in 2h`"
    ))
}

pub fn normalize_cron_utc(value: &str, tz: Tz) -> Result<(String, String, DateTime<Utc>)> {
    let fields: Vec<&str> = value.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(anyhow!("cron must have exactly five fields"));
    }
    let local = fields.join(" ");
    validate_cron(&local)?;
    let now = Utc::now();
    let next_local = next_cron_after(&local, now.with_timezone(&tz))
        .ok_or_else(|| anyhow!("cron has no future ticks"))?;
    let utc = cron_from_local_tick(&local, next_local.with_timezone(&Utc));
    validate_cron(&utc)?;
    Ok((utc, local, next_local.with_timezone(&Utc)))
}

pub fn next_cron_after<TzLike>(
    five_field_cron: &str,
    after: DateTime<TzLike>,
) -> Option<DateTime<TzLike>>
where
    TzLike: TimeZone,
    TzLike::Offset: std::fmt::Display,
{
    let schedule: Schedule = format!("0 {five_field_cron}").parse().ok()?;
    schedule.after(&after).next()
}

fn validate_cron(five_field_cron: &str) -> Result<()> {
    let _: Schedule = format!("0 {five_field_cron}")
        .parse()
        .with_context(|| format!("invalid cron expression `{five_field_cron}`"))?;
    Ok(())
}

fn cron_from_local_tick(local: &str, utc_tick: DateTime<Utc>) -> String {
    let fields: Vec<&str> = local.split_whitespace().collect();
    let minute = utc_tick.format("%M").to_string();
    let hour = utc_tick.format("%H").to_string();
    format!(
        "{} {} {} {} {}",
        minute, hour, fields[2], fields[3], fields[4]
    )
}

fn parse_friendly_duration(value: &str) -> Result<u64> {
    let (number, multiplier) = match value.chars().last() {
        Some('s') => (&value[..value.len() - 1], 1),
        Some('m') => (&value[..value.len() - 1], 60),
        Some('h') => (&value[..value.len() - 1], 60 * 60),
        Some('d') => (&value[..value.len() - 1], 24 * 60 * 60),
        Some(ch) if ch.is_ascii_digit() => (value, 1),
        _ => return Err(anyhow!("invalid duration `{value}`")),
    };
    Ok(number.parse::<u64>()?.saturating_mul(multiplier))
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
            max_runs: None,
            max_duration_s: None,
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
