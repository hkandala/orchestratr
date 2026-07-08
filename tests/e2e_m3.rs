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
fn goal_passes_on_second_iteration_with_mock_judge() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m3; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("goal-pass")?;
    let state = ctx.store.path().join("judge-seq.txt");
    let prompt = ctx.store.path().join("goal.md");
    fs::write(
        &prompt,
        format!(
            "finish this [[respond:worker first]] [[respond-seq:{}:FAIL: needs revision||PASS]]",
            path_str(&state)?
        ),
    )?;

    let created = ctx.run(&[
        "goal",
        "--harness",
        "mock",
        "--judge-harness",
        "mock",
        "--prompt-file",
        path_str(&prompt)?,
        "--max-iters",
        "3",
        "--json",
    ])?;
    let id = json_string(&created, "/result/id")?;
    wait_for_job_status(&ctx, &id, "done", Duration::from_secs(45))?;
    let job = ctx.run(&["job", "show", &id, "--json"])?.json()?;
    assert_eq!(
        job.pointer("/result/runs_count").and_then(Value::as_i64),
        Some(2)
    );
    assert_eq!(
        job.pointer("/result/ended_reason").and_then(Value::as_str),
        Some("passed")
    );
    Ok(())
}

#[test]
fn workflow_parents_children_and_kills_orphans_by_default() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m3; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("workflow-orphan")?;
    let script = ctx.store.path().join("workflow.sh");
    fs::write(
        &script,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\n{} run --harness mock -p 'one [[sleep:30000]]' --json\n{} run --harness mock -p 'two [[sleep:30000]]' --json\n",
            shell_quote(env!("CARGO_BIN_EXE_orcr")),
            shell_quote(env!("CARGO_BIN_EXE_orcr")),
        ),
    )?;

    let run = ctx.run(&[
        "workflow",
        "run",
        path_str(&script)?,
        "--on-orphan",
        "kill",
        "--json",
    ])?;
    let workflow_id = json_string(&run, "/result/id")?;
    assert_eq!(
        run.json()?
            .pointer("/result/status")
            .and_then(Value::as_str),
        Some("done")
    );
    let children = ctx
        .run(&["history", "--parent", &workflow_id, "--json"])?
        .json()?;
    let items = children
        .pointer("/result/items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("missing history items: {children}"))?;
    assert_eq!(items.len(), 2, "expected two workflow children: {children}");
    assert!(items.iter().all(|item| {
        item.get("parent_id").and_then(Value::as_str) == Some(workflow_id.as_str())
            && item.get("status").and_then(Value::as_str) == Some("killed")
    }));
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
        let store = tempfile::tempdir()?;
        let session = format!("{E2E_PREFIX}{label}-{}", unique_suffix());
        fs::write(
            store.path().join("config.toml"),
            format!("[herdr]\nsession = {session:?}\n"),
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
    fn json(&self) -> Result<Value> {
        serde_json::from_slice(&self.output.stdout)
            .with_context(|| format!("stdout was not json:\n{}", self.stdout_string()))
    }

    fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).to_string()
    }
}

fn wait_for_job_status(ctx: &E2eContext, id: &str, status: &str, timeout: Duration) -> Result<()> {
    wait_until(timeout, || {
        let job = ctx.run(&["job", "show", id, "--json"]).ok()?.json().ok()?;
        (job.pointer("/result/status").and_then(Value::as_str) == Some(status)).then_some(())
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
        eprintln!("skipping e2e_m3 hygiene; set ORCR_E2E=1 to run against real herdr");
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
