use std::fs;
use std::path::Path;
use std::process::Output;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const E2E_PREFIX: &str = "orcr-e2e-";
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn loop_three_ticks_until_stops_it() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("loop-until")?;
    let prompt = ctx.store.path().join("prompt.md");
    fs::write(&prompt, "tick")?;

    let output = ctx.run(&[
        "loop",
        "--harness",
        "mock",
        "--prompt-file",
        path_str(&prompt)?,
        "--every",
        "3s",
        "--max-runs",
        "5",
        "--until",
        "ALL PASS",
        "--json",
    ])?;
    let id = json_string(&output, "/result/id")?;
    wait_for_job_runs(&ctx, &id, 2, Duration::from_secs(30))?;
    fs::write(&prompt, "tick ALL PASS")?;
    wait_for_job_done(&ctx, &id, Duration::from_secs(30))?;
    let output = ctx.run(&["job", "show", &id, "--json"])?.json()?;
    assert_eq!(
        output
            .pointer("/result/ended_reason")
            .and_then(Value::as_str),
        Some("until_matched")
    );
    assert_eq!(
        output.pointer("/result/runs_count").and_then(Value::as_i64),
        Some(3)
    );
    Ok(())
}

#[test]
fn tick_on_fires_when_probe_changes() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("tick-on")?;
    let probe = ctx.store.path().join("probe.txt");
    fs::write(&probe, "one")?;
    let script = format!("cat {}", shell_quote(path_str(&probe)?));

    let run = ctx.run(&[
        "loop",
        "--harness",
        "mock",
        "-p",
        "probe tick",
        "--every",
        "30s",
        "--tick-on",
        &script,
        "--max-runs",
        "2",
        "--json",
    ])?;
    let id = json_string(&run, "/result/id")?;
    wait_for_job_runs(&ctx, &id, 1, Duration::from_secs(25))?;
    fs::write(&probe, "two")?;
    wait_for_job_done(&ctx, &id, Duration::from_secs(40))?;
    let job = ctx.run(&["job", "show", &id, "--json"])?.json()?;
    assert_eq!(
        job.pointer("/result/runs_count").and_then(Value::as_i64),
        Some(2)
    );
    Ok(())
}

#[test]
fn schedule_at_fires_once_and_ends_fired() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("schedule-at")?;
    let run = ctx.run(&[
        "schedule",
        "add",
        "--at",
        "in 5s",
        "--harness",
        "mock",
        "-p",
        "scheduled once",
        "--json",
    ])?;
    let id = json_string(&run, "/result/id")?;
    wait_for_job_done(&ctx, &id, Duration::from_secs(30))?;
    let job = ctx.run(&["schedule", "show", &id, "--json"])?.json()?;
    assert_eq!(
        job.pointer("/result/ended_reason").and_then(Value::as_str),
        Some("fired")
    );
    assert_eq!(
        job.pointer("/result/runs_count").and_then(Value::as_i64),
        Some(1)
    );
    ctx.run(&["schedule", "resume", &id, "--json"])?
        .assert_code(7)?;
    Ok(())
}

#[test]
fn daemon_restart_reconciles_missing_pane_to_lost() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("reconcile-lost")?;
    let run = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "-p",
        "long [[sleep:30000]]",
        "--json",
    ])?;
    let id = json_string(&run, "/result/agent/id")?;
    let pane = json_string(&run, "/result/agent/pane_id")?;
    ctx.herdr(&["pane", "close", &pane])?;
    ctx.stop_daemon();
    ctx.run(&["serve"])?;
    wait_for_agent_status(&ctx, &id, "lost", Duration::from_secs(15))?;
    Ok(())
}

#[test]
fn gc_dry_run_then_real_cleans_orphaned_pane() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("gc-orphan")?;
    let client_bin = herdr_bin();
    let _server = std::process::Command::new(&client_bin)
        .args(["--session", &ctx.session, "server"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("failed to start herdr server")?;
    std::thread::sleep(Duration::from_millis(800));
    let started = std::process::Command::new(&client_bin)
        .args([
            "--session",
            &ctx.session,
            "agent",
            "start",
            "a999",
            "--cwd",
            path_str(ctx.store.path())?,
            "--",
            env!("CARGO_BIN_EXE_orcr-mock-agent"),
        ])
        .output()
        .context("failed to create orphaned pane")?;
    if !started.status.success() {
        bail!(
            "failed to create orphaned pane\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&started.stdout),
            String::from_utf8_lossy(&started.stderr)
        );
    }
    let dry = ctx
        .run(&["gc", "--dry-run", "--json"])?
        .assert_code(0)?
        .json()?;
    assert!(
        dry.pointer("/result/killed_unknown_panes")
            .and_then(Value::as_array)
            .is_some_and(|panes| !panes.is_empty()),
        "expected dry-run to report an orphan pane: {dry}"
    );
    let real = ctx.run(&["gc", "--json"])?.assert_code(0)?.json()?;
    assert!(
        real.pointer("/result/killed_unknown_panes")
            .and_then(Value::as_array)
            .is_some_and(|panes| !panes.is_empty()),
        "expected gc to close an orphan pane: {real}"
    );
    Ok(())
}

#[test]
fn queued_admission_promotes_fifo_with_max_concurrent_one() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new_with_config("queue", "[limits]\nmax_concurrent = 1\n")?;
    let first = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "first",
        "-p",
        "first [[sleep:2500]]",
        "--json",
    ])?;
    let second = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "second",
        "-p",
        "second",
        "--json",
    ])?;
    let first_id = json_string(&first, "/result/agent/id")?;
    let second_id = json_string(&second, "/result/agent/id")?;
    assert_eq!(
        second
            .json()?
            .pointer("/result/agent/status")
            .and_then(Value::as_str),
        Some("queued")
    );
    ctx.run(&["wait", &first_id, "--timeout", "20s", "--json"])?
        .assert_code(0)?;
    wait_for_agent_status(&ctx, &second_id, "done", Duration::from_secs(25))?;
    Ok(())
}

#[test]
fn no_running_e2e_sessions_are_left_by_prior_tests() -> Result<()> {
    let _serial = lock_serial();
    assert_no_running_e2e_sessions()
}

struct E2eContext {
    store: TempDir,
    session: String,
    herdr_bin: String,
}

impl E2eContext {
    fn new(label: &str) -> Result<Self> {
        Self::new_with_config(label, "")
    }

    fn new_with_config(label: &str, extra_config: &str) -> Result<Self> {
        let store = tempfile::tempdir()?;
        let session = format!("{E2E_PREFIX}{label}-{}", unique_suffix());
        fs::write(
            store.path().join("config.toml"),
            format!("[herdr]\nsession = {session:?}\n{extra_config}"),
        )?;
        Ok(Self {
            store,
            session,
            herdr_bin: herdr_bin(),
        })
    }

    fn run(&self, args: &[&str]) -> Result<CmdOutput> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orcr"));
        command.env("ORCR_STORE", self.store.path());
        command.args(args);
        let output = command.output()?;
        Ok(CmdOutput { output })
    }

    fn herdr(&self, args: &[&str]) -> Result<CmdOutput> {
        let mut command = std::process::Command::new(&self.herdr_bin);
        command.arg("--session").arg(&self.session).args(args);
        let output = command.output()?;
        Ok(CmdOutput { output })
    }

    fn stop_daemon(&self) {
        if let Ok(pid) = fs::read_to_string(self.store.path().join("serve.pid")) {
            let _ = std::process::Command::new("kill").arg(pid.trim()).output();
            std::thread::sleep(Duration::from_millis(600));
        }
    }
}

impl Drop for E2eContext {
    fn drop(&mut self) {
        self.stop_daemon();
        if !self.session.starts_with(E2E_PREFIX) {
            return;
        }
        let _ = std::process::Command::new(&self.herdr_bin)
            .args(["session", "stop", &self.session, "--json"])
            .output();
        let _ = std::process::Command::new(&self.herdr_bin)
            .args(["session", "delete", &self.session, "--json"])
            .output();
    }
}

struct CmdOutput {
    output: Output,
}

impl CmdOutput {
    fn assert_code(&self, code: i32) -> Result<&Self> {
        let actual = self.output.status.code();
        if actual != Some(code) {
            bail!(
                "expected exit code {code}, got {actual:?}\nstdout:\n{}\nstderr:\n{}",
                self.stdout_string(),
                String::from_utf8_lossy(&self.output.stderr)
            );
        }
        Ok(self)
    }

    fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.output.stdout)
            .with_context(|| format!("stdout was not json:\n{}", self.stdout_string()))
    }

    fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).to_string()
    }
}

fn wait_for_job_runs(ctx: &E2eContext, id: &str, runs: i64, timeout: Duration) -> Result<()> {
    wait_until(timeout, || {
        let job = ctx.run(&["job", "show", id, "--json"]).ok()?.json().ok()?;
        (job.pointer("/result/runs_count").and_then(Value::as_i64) >= Some(runs)).then_some(())
    })
}

fn wait_for_job_done(ctx: &E2eContext, id: &str, timeout: Duration) -> Result<()> {
    wait_until(timeout, || {
        let job = ctx.run(&["job", "show", id, "--json"]).ok()?.json().ok()?;
        (job.pointer("/result/status").and_then(Value::as_str) == Some("done")).then_some(())
    })
}

fn wait_for_agent_status(
    ctx: &E2eContext,
    id: &str,
    status: &str,
    timeout: Duration,
) -> Result<()> {
    wait_until(timeout, || {
        let show = ctx.run(&["show", id, "--json"]).ok()?.json().ok()?;
        (show.pointer("/result/agent/status").and_then(Value::as_str) == Some(status)).then_some(())
    })
}

fn wait_until<F>(timeout: Duration, mut f: F) -> Result<()>
where
    F: FnMut() -> Option<()>,
{
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if f().is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    bail!("timed out after {timeout:?}")
}

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").ok().as_deref() == Some("1")
}

fn lock_serial() -> MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn herdr_bin() -> String {
    std::env::var("HERDR_BIN").unwrap_or_else(|_| "herdr".to_string())
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn json_string(output: &CmdOutput, pointer: &str) -> Result<String> {
    output
        .json()?
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("missing string at {pointer}"))
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn assert_no_running_e2e_sessions() -> Result<()> {
    if !e2e_enabled() {
        eprintln!("skipping e2e_m2 hygiene; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let output = std::process::Command::new(herdr_bin())
        .args(["session", "list", "--json"])
        .output()?;
    if !output.status.success() {
        bail!(
            "herdr session list failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let json: Value = serde_json::from_slice(&output.stdout)?;
    let sessions = json
        .pointer("/result/sessions")
        .or_else(|| json.pointer("/sessions"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("unexpected herdr session list json: {json}"))?;
    let running: Vec<String> = sessions
        .iter()
        .filter(|session| {
            session
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name.starts_with(E2E_PREFIX))
                && session
                    .get("running")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        })
        .filter_map(|session| {
            session
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    assert!(running.is_empty(), "left running e2e sessions: {running:?}");
    Ok(())
}
