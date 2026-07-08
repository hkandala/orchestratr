use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

const E2E_PREFIX: &str = "orcr-e2e-";

static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn fan_out_two_mocks_wait_all_then_out_both() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("fanout-all")?;

    let a = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "left",
        "-p",
        "alpha",
        "--json",
    ])?;
    let b = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "right",
        "-p",
        "beta",
        "--json",
    ])?;
    let a_id = json_string(&a, "/result/agent/id")?;
    let b_id = json_string(&b, "/result/agent/id")?;

    ctx.run(&["wait", &a_id, &b_id, "--timeout", "20s", "--json"])?
        .assert_code(0)?;
    let out_a = ctx.run(&["out", &a_id, "--json"])?.json()?;
    let out_b = ctx.run(&["out", &b_id, "--json"])?.json()?;

    assert_json_contains(&out_a, "alpha");
    assert_json_contains(&out_b, "beta");
    Ok(())
}

#[test]
fn wait_any_returns_first_completed_agent() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("wait-any")?;

    let fast = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "fast",
        "-p",
        "fast [[sleep:300]]",
        "--json",
    ])?;
    let slow = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "slow",
        "-p",
        "slow [[sleep:4000]]",
        "--json",
    ])?;
    let fast_id = json_string(&fast, "/result/agent/id")?;
    let slow_id = json_string(&slow, "/result/agent/id")?;

    let waited = ctx
        .run(&[
            "wait",
            &fast_id,
            &slow_id,
            "--any",
            "--timeout",
            "10s",
            "--json",
        ])?
        .assert_code(0)?
        .json()?;
    assert_eq!(
        waited
            .pointer("/result/completed/0")
            .and_then(Value::as_str),
        Some(fast_id.as_str())
    );
    assert!(waited["result"]["pending"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id.as_str() == Some(slow_id.as_str())));

    ctx.run(&["kill", &slow_id, "--json"])?.assert_code(0)?;
    Ok(())
}

#[test]
fn steer_mid_turn_keeps_one_response_with_both_prompts() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("steer")?;

    let run = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "steered",
        "-p",
        "first prompt [[sleep:4000]]",
        "--keep",
        "--timeout",
        "45s",
        "--json",
    ])?;
    let id = json_string(&run, "/result/agent/id")?;
    std::thread::sleep(std::time::Duration::from_millis(700));

    ctx.run(&["send", &id, "second steer prompt", "--steer", "--json"])?
        .assert_code(0)?;
    ctx.run(&["wait", &id, "--timeout", "45s", "--json"])?
        .assert_code(0)?;

    let run_dir = ctx.store.path().join("runs").join(&id);
    let responses = response_files(&run_dir)?;
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].file_name().and_then(|n| n.to_str()),
        Some("001-response.md")
    );
    let text = fs::read_to_string(&responses[0])?;
    assert!(text.contains("first prompt"));
    assert!(text.contains("second steer prompt"));
    Ok(())
}

#[test]
fn kept_agent_accepts_second_turn_and_writes_002_files() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("turn")?;

    let run = ctx
        .run(&[
            "run",
            "--harness",
            "mock",
            "--name",
            "kept",
            "-p",
            "turn one",
            "--keep",
            "--wait",
            "--json",
        ])?
        .assert_code(0)?
        .json()?;
    let id = json_string_value(&run, "/result/agent/id")?;

    ctx.run(&["send", &id, "turn two", "--turn", "--wait", "--json"])?
        .assert_code(0)?;

    let run_dir = ctx.store.path().join("runs").join(&id);
    assert!(run_dir.join("002-prompt.md").exists());
    assert!(run_dir.join("002-response.md").exists());
    let show = ctx.run(&["show", &id, "--json"])?.assert_code(0)?.json()?;
    assert_eq!(show["result"]["turns"].as_array().unwrap().len(), 2);
    Ok(())
}

#[test]
fn recursive_path_output_and_lineage_from_env_contract() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("lineage")?;

    let parent = ctx
        .run(&[
            "run",
            "--harness",
            "mock",
            "--name",
            "parent",
            "-p",
            "parent body",
            "--wait",
            "--json",
        ])?
        .assert_code(0)?
        .json()?;
    let parent_id = json_string_value(&parent, "/result/agent/id")?;

    let child = ctx.run_with_env(
        &[
            "run",
            "--harness",
            "mock",
            "--name",
            "child",
            "-p",
            "child body",
            "--wait",
            "--json",
        ],
        &[
            ("ORCR_ID", parent_id.as_str()),
            ("ORCR_PARENT", parent_id.as_str()),
            ("ORCR_DEPTH", "0"),
        ],
    )?;
    let child = child.assert_code(0)?.json()?;
    let child_id = json_string_value(&child, "/result/agent/id")?;

    let show = ctx
        .run(&["show", &parent_id, "--json"])?
        .assert_code(0)?
        .json()?;
    assert_eq!(
        show.pointer("/result/children/0").and_then(Value::as_str),
        Some(child_id.as_str())
    );
    let tree = ctx
        .run(&["tree", &parent_id, "--json"])?
        .assert_code(0)?
        .json()?;
    assert_json_contains(&tree, &child_id);

    let paths = ctx
        .run(&["out", &parent_id, "--recursive", "--format", "path"])?
        .assert_code(0)?
        .stdout_string();
    assert!(paths.contains(&parent_id));
    assert!(paths.contains(&child_id));
    assert!(paths.contains("001-response.md"));
    Ok(())
}

#[test]
fn kill_tree_marks_parent_and_children_killed() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("kill-tree")?;

    let parent = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "root",
        "-p",
        "root [[sleep:8000]]",
        "--keep",
        "--json",
    ])?;
    let parent_id = json_string(&parent, "/result/agent/id")?;
    let child_one = ctx.run_with_env(
        &[
            "run",
            "--harness",
            "mock",
            "--name",
            "child-one",
            "-p",
            "child one [[sleep:8000]]",
            "--keep",
            "--json",
        ],
        &[("ORCR_ID", parent_id.as_str()), ("ORCR_DEPTH", "0")],
    )?;
    let child_two = ctx.run_with_env(
        &[
            "run",
            "--harness",
            "mock",
            "--name",
            "child-two",
            "-p",
            "child two [[sleep:8000]]",
            "--keep",
            "--json",
        ],
        &[("ORCR_ID", parent_id.as_str()), ("ORCR_DEPTH", "0")],
    )?;
    let child_one_id = json_string(&child_one, "/result/agent/id")?;
    let child_two_id = json_string(&child_two, "/result/agent/id")?;

    let killed = ctx
        .run(&["kill", &parent_id, "--tree", "--json"])?
        .assert_code(0)?
        .json()?;
    assert_json_contains(&killed, &parent_id);
    assert_json_contains(&killed, &child_one_id);
    assert_json_contains(&killed, &child_two_id);

    for id in [&parent_id, &child_one_id, &child_two_id] {
        let show = ctx.run(&["show", id, "--json"])?.assert_code(0)?.json()?;
        assert_eq!(
            show.pointer("/result/agent/status").and_then(Value::as_str),
            Some("killed")
        );
    }
    Ok(())
}

#[test]
fn run_wait_timeout_exits_3() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("timeout")?;

    let output = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "timeout",
        "-p",
        "[[sleep:8000]]",
        "--wait",
        "--timeout",
        "2s",
        "--json",
    ])?;
    output.assert_code(3)?;
    let json = output.json()?;
    assert_eq!(
        json.pointer("/result/agent/status").and_then(Value::as_str),
        Some("timeout")
    );
    Ok(())
}

#[test]
fn blocked_directive_exits_4_on_run_wait() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("blocked")?;

    let output = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "blocked",
        "-p",
        "[[block]]",
        "--wait",
        "--timeout",
        "5s",
        "--json",
    ])?;
    output.assert_code(4)?;
    let json = output.json()?;
    assert_eq!(
        json.pointer("/result/agent/status").and_then(Value::as_str),
        Some("blocked")
    );
    Ok(())
}

#[test]
fn reserved_agent_name_is_rejected() -> Result<()> {
    let _serial = lock_serial();
    if !e2e_enabled() {
        eprintln!("skipping e2e_m1; set ORCR_E2E=1 to run against real herdr");
        return Ok(());
    }
    let ctx = E2eContext::new("reserved")?;

    let output = ctx.run(&[
        "run",
        "--harness",
        "mock",
        "--name",
        "a7",
        "-p",
        "nope",
        "--json",
    ])?;
    output.assert_code(1)?;
    let json = output.json()?;
    assert_json_contains(&json, "reserved");
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
        self.run_with_env(args, &[])
    }

    fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> Result<CmdOutput> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_orcr"));
        command.env("ORCR_STORE", self.store.path());
        for (key, value) in envs {
            command.env(key, value);
        }
        command.args(args);
        let output = command.output()?;
        Ok(CmdOutput { output })
    }
}

impl Drop for E2eContext {
    fn drop(&mut self) {
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
    json_string_value(&output.json()?, pointer)
}

fn json_string_value(value: &Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| anyhow!("missing string at {pointer}: {value}"))
}

fn assert_json_contains(value: &Value, needle: &str) {
    assert!(
        value.to_string().contains(needle),
        "expected JSON to contain {needle:?}: {value}"
    );
}

fn response_files(run_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(run_dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with("-response.md"))
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn assert_no_running_e2e_sessions() -> Result<()> {
    if std::env::var("ORCR_E2E").ok().as_deref() != Some("1") {
        eprintln!("skipping e2e_m1 hygiene; set ORCR_E2E=1 to run against real herdr");
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
