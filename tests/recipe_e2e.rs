//! M7 SDK + recipe + scaffold e2e against **live herdr** + the mock provider (spec M7
//! acceptance). Gated behind `ORCR_E2E=1` so unit runs stay fast. Every test runs a real
//! `orcr` server over a throwaway `ORCR_HOME` whose config points at a **disposable** herdr
//! session `orcr_test_<rand>`, torn down by a drop guard. The user's `default` session is
//! never touched. The mock provider stands in for a real provider and writes a claude-format
//! transcript so the SDK's `logs`/`ask` resolve.
//!
//! Run with:  `ORCR_E2E=1 cargo test --test recipe_e2e -- --test-threads=1 --nocapture`
//!
//! Proves: every §9 recipe runs end-to-end against the mock; two copies of fan-out and
//! tournament run concurrently under distinct scopes; the durable-handoff loop self-terminates;
//! `orcr scaffold` + `npx tsx workflow.ts` runs green, re-run → state_conflict, pinned version
//! == CLI version; and SDK-composed paths equal the CLI's for the same nested scope.

use orchestratr::driver::{HerdrBinary, HerdrDriver};
use orchestratr::home::Home;
use orchestratr::server::Client;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::Duration;

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").as_deref() == Ok("1")
}
fn mock_agent_bin() -> String {
    env!("CARGO_BIN_EXE_orcr-mock-agent").to_string()
}
fn orcr_bin() -> String {
    env!("CARGO_BIN_EXE_orcr").to_string()
}
fn sdk_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("sdk")
        .join("ts")
}
fn tsx_bin() -> PathBuf {
    sdk_dir().join("node_modules").join(".bin").join("tsx")
}

/// Short mock completion tuning so waits/asks resolve fast; transcript reads enabled.
const MOCK_TUNING: &str = r#""integrations":{"mock":{"fast_turn_grace_ms":2000,"idle_stable_ms":500,"transcript_settle_ms":200,"transcript_freshness_timeout_ms":8000,"shutdown_grace_ms":200}}"#;

static SDK_BUILD: Once = Once::new();

/// Build the SDK (install deps if needed, compile to dist) exactly once per test process.
fn ensure_sdk_built() {
    SDK_BUILD.call_once(|| {
        let dir = sdk_dir();
        if !dir.join("node_modules").is_dir() {
            let ok = Command::new("npm")
                .args(["install", "--no-audit", "--no-fund"])
                .current_dir(&dir)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(ok, "npm install in the SDK failed");
        }
        let ok = Command::new("npm")
            .args(["run", "build"])
            .current_dir(&dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "npm run build in the SDK failed");
    });
}

/// A live orcr server bound to a throwaway home + a disposable herdr session.
struct TestServer {
    home: tempfile::TempDir,
    bin: HerdrBinary,
    session: String,
    #[allow(dead_code)]
    driver: HerdrDriver,
    extra_env: Vec<(String, String)>,
}

impl TestServer {
    fn start(extra_env: &[(&str, &str)]) -> TestServer {
        ensure_sdk_built();
        let home = tempfile::tempdir().expect("home");
        let bin = HerdrBinary::discover(None).expect("herdr on PATH");
        let rand = uuid::Uuid::new_v4().simple().to_string();
        let session = format!("orcr_test_{}", &rand[..12]);
        let driver = match (|| {
            let sock = bin.ensure_session(&session)?;
            HerdrDriver::connect(&sock)
        })() {
            Ok(d) => d,
            Err(e) => {
                let _ = bin.session_stop(&session);
                let _ = bin.session_delete(&session);
                panic!("disposable session bootstrap failed: {e}");
            }
        };
        std::fs::write(
            home.path().join("config.json"),
            format!(
                r#"{{"herdr":{{"session":"{session}"}},"defaults":{{"agent":"mock"}},"concurrency":{{"max":10}},{MOCK_TUNING}}}"#
            ),
        )
        .unwrap();
        let ts = TestServer {
            home,
            bin,
            session,
            driver,
            extra_env: extra_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        ts.spawn_server();
        ts
    }

    fn spawn_server(&self) {
        let mut cmd = Command::new(orcr_bin());
        cmd.args(["server", "start"])
            .env("ORCR_HOME", self.home.path())
            .env("ORCR_ALLOW_MOCK_PROVIDER", "1")
            .env("ORCR_DISABLE_DISCOVERY", "1")
            .env("ORCR_MOCK_AGENT_BIN", mock_agent_bin())
            .stdin(Stdio::null());
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("orcr server start");
        assert!(
            out.status.success(),
            "server start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.client()
            .wait_for_ready(Duration::from_secs(10))
            .expect("server ready");
    }

    fn home_path(&self) -> &Path {
        self.home.path()
    }
    fn client(&self) -> Client {
        Client::new(Home::at(self.home.path()).socket_path())
    }
    fn request(&self, method: &str, params: Value) -> orchestratr::Result<Value> {
        self.client().request(method, params)
    }

    /// Run a recipe file via tsx with the mock provider selected. Returns (success, stdout+stderr).
    fn run_recipe(&self, rel: &str, extra_env: &[(&str, &str)]) -> (bool, String) {
        let mut cmd = Command::new(tsx_bin());
        cmd.arg(sdk_dir().join(rel))
            .current_dir(sdk_dir())
            .env("ORCR_HOME", self.home_path())
            .env("ORCR_BIN", orcr_bin())
            .env("ORCR_RECIPE_AGENT", "mock")
            .env("ORCR_RECIPE_VERIFIER", "mock");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let out = cmd.output().expect("run recipe via tsx");
        let mut log = String::from_utf8_lossy(&out.stdout).to_string();
        log.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), log)
    }

    fn active_agents(&self) -> Vec<Value> {
        self.request("agent.ls", json!({}))
            .map(|r| r["agents"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.request("server.stop", json!({}));
        for _ in 0..20 {
            match self.client().handshake() {
                Ok(v) => {
                    if let Some(pid) = v["pid"].as_u64() {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGKILL);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
        // Tear down the disposable session, verifying it is actually gone (herdr can
        // transiently reject a stop/delete right after the owning server is killed — retry
        // until `find_session` reports it absent, so no `orcr_test_*` session ever leaks).
        for _ in 0..20 {
            let _ = self.bin.session_stop(&self.session);
            let _ = self.bin.session_delete(&self.session);
            match self.bin.find_session(&self.session) {
                Ok(None) => break,
                _ => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Every §9 recipe runs end-to-end against the mock provider (spec M7 acceptance).
#[test]
fn e2e_recipes_run_against_mock() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start(&[]);
    for rel in [
        "recipes/fan-out-and-merge.ts",
        "recipes/classify-and-act.ts",
        "recipes/adversarial-verification.ts",
        "recipes/generate-and-filter.ts",
        "recipes/tournament.ts",
        "recipes/fix-until-green.ts",
    ] {
        let (ok, log) = ts.run_recipe(rel, &[]);
        assert!(ok, "recipe {rel} failed:\n{log}");
    }
    // Recipes clean up after themselves (gc:immediate / killOnThrow / explicit kills).
    assert!(
        wait_until(Duration::from_secs(15), || ts.active_agents().is_empty()),
        "recipes left active agents behind: {:?}",
        ts.active_agents()
    );
}

/// Two copies of fan-out-and-merge and tournament, started concurrently under distinct top
/// scopes, run clean (spec M7 acceptance — concurrency fixtures).
#[test]
fn e2e_concurrent_fanout_and_tournament() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start(&[]);
    // A copy of fan-out-and-merge and a copy of tournament, running concurrently under distinct
    // top scopes — proves scope isolation (no path collision / cross-talk) between concurrent
    // workflows. The stronger two-copies-each fixture is `e2e_concurrent_burst_high` below.
    let jobs = [
        ("recipes/fan-out-and-merge.ts", "review_a"),
        ("recipes/tournament.ts", "tourney_a"),
    ];
    let ts_ref = &ts;
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|(rel, scope)| {
                s.spawn(move || ts_ref.run_recipe(rel, &[("ORCR_RECIPE_SCOPE", scope)]))
            })
            .collect();
        for (h, (rel, scope)) in handles.into_iter().zip(jobs.iter()) {
            let (ok, log) = h.join().unwrap();
            assert!(ok, "concurrent {rel} (scope {scope}) failed:\n{log}");
        }
    });
    assert!(
        wait_until(Duration::from_secs(15), || ts.active_agents().is_empty()),
        "concurrent recipes left agents behind"
    );
}

/// The full concurrency-fixtures acceptance (spec M7): **two copies each** of fan-out-and-merge
/// and tournament, started concurrently under distinct top scopes, run clean. This exercises two
/// copies of the *same* scope-parameterized recipe coexisting without collision — the exact
/// singleton-vs-scope behavior the spec's §9 preamble calls out. (Fixed by making the herdr
/// agent `name` the full session-unique path; herdr 0.7.2 enforces session-global name
/// uniqueness — see m7-sdk-skill/notes.md.)
#[test]
fn e2e_concurrent_burst_high() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start(&[]);
    let jobs = [
        ("recipes/fan-out-and-merge.ts", "review_a"),
        ("recipes/fan-out-and-merge.ts", "review_b"),
        ("recipes/tournament.ts", "tourney_a"),
        ("recipes/tournament.ts", "tourney_b"),
    ];
    let ts_ref = &ts;
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|(rel, scope)| {
                s.spawn(move || ts_ref.run_recipe(rel, &[("ORCR_RECIPE_SCOPE", scope)]))
            })
            .collect();
        for (h, (rel, scope)) in handles.into_iter().zip(jobs.iter()) {
            let (ok, log) = h.join().unwrap();
            assert!(ok, "concurrent {rel} (scope {scope}) failed:\n{log}");
        }
    });
    assert!(
        wait_until(Duration::from_secs(15), || ts.active_agents().is_empty()),
        "concurrent recipes left agents behind"
    );
}

/// §9.7 durable handoff: kickoff hands off to a loop; driving its runs drains the queue and the
/// loop self-terminates (`loop.rm` from resume.ts).
#[test]
fn e2e_loop_until_done_self_terminates() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let qfile = std::env::temp_dir().join(format!("orcr_q_{}.json", uuid::Uuid::new_v4().simple()));
    std::fs::write(&qfile, r#"["a","b","c"]"#).unwrap();
    let loop_name = format!("burn_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    // The loop command (spawned by the scheduler) must inherit the queue file + loop name.
    let ts = TestServer::start(&[
        ("ORCR_RECIPE_QUEUE_FILE", qfile.to_str().unwrap()),
        ("ORCR_RECIPE_LOOP", &loop_name),
    ]);

    // Kickoff: work one item now (BUDGET=1), then hand off (2 remain).
    let (ok, log) = ts.run_recipe(
        "recipes/loop-until-done/kickoff.ts",
        &[
            ("ORCR_RECIPE_QUEUE_FILE", qfile.to_str().unwrap()),
            ("ORCR_RECIPE_LOOP", &loop_name),
            ("ORCR_RECIPE_BUDGET", "1"),
        ],
    );
    assert!(ok, "kickoff failed:\n{log}");
    // The loop exists.
    let loops = ts.request("loop.ls", json!({})).unwrap();
    assert!(
        loops["loops"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["name"] == json!(loop_name)),
        "kickoff did not create the loop"
    );

    // Drive runs until the queue drains and resume.ts removes the loop (each run works one item).
    let mut removed = false;
    for _ in 0..5 {
        let _ = ts.request("loop.run.start", json!({ "name": loop_name }));
        // Wait for the run to finish (its process group exits).
        std::thread::sleep(Duration::from_secs(2));
        let gone = ts
            .request("loop.ls", json!({}))
            .map(|r| {
                !r["loops"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|l| l["name"] == json!(loop_name))
            })
            .unwrap_or(false);
        if gone {
            removed = true;
            break;
        }
    }
    let _ = std::fs::remove_file(&qfile);
    assert!(
        removed,
        "loop did not self-terminate after draining the queue"
    );
}

/// `orcr scaffold` + `npx tsx workflow.ts` runs green against the mock; re-run → state_conflict;
/// pinned SDK version == CLI version (spec M7 acceptance — scaffold).
#[test]
fn e2e_scaffold_runs_green() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    ensure_sdk_built();
    // Pack the built SDK into a tarball so `npm install` resolves the local package offline.
    let pack = Command::new("npm")
        .args(["pack", "--silent"])
        .current_dir(sdk_dir())
        .output()
        .expect("npm pack");
    assert!(pack.status.success(), "npm pack failed");
    let tgz = String::from_utf8_lossy(&pack.stdout).trim().to_string();
    let tarball = sdk_dir().join(tgz.lines().last().unwrap_or_default());
    assert!(tarball.is_file(), "tarball not produced: {tarball:?}");
    let sdk_spec = format!("file:{}", tarball.display());

    let ts = TestServer::start(&[]);
    let proj = tempfile::tempdir().unwrap();
    let proj_dir = proj.path().join("wf");

    // Scaffold with the SDK spec pointing at the local tarball; scaffold runs `npm install`.
    let out = Command::new(orcr_bin())
        .args(["scaffold", proj_dir.to_str().unwrap(), "--json"])
        .env("ORCR_HOME", ts.home_path())
        .env("ORCR_SDK_SPEC", &sdk_spec)
        .output()
        .expect("orcr scaffold");
    assert!(
        out.status.success(),
        "scaffold failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Exactly three files; pinned version (default, no override) equals the CLI version.
    for f in ["package.json", "tsconfig.json", "workflow.ts"] {
        assert!(proj_dir.join(f).is_file(), "{f} not scaffolded");
    }
    let pkg: Value =
        serde_json::from_str(&std::fs::read_to_string(proj_dir.join("package.json")).unwrap())
            .unwrap();
    // With the ORCR_SDK_SPEC override the dep points at the tarball; without it, it pins the
    // CLI version. Verify the pin default separately.
    let default_pkg = orchestratr::scaffold::sdk_version();
    let no_override = tempfile::tempdir().unwrap();
    let _ = Command::new(orcr_bin())
        .args([
            "scaffold",
            no_override.path().join("p").to_str().unwrap(),
            "--json",
        ])
        .env("ORCR_HOME", ts.home_path())
        .env_remove("ORCR_SDK_SPEC")
        .output();
    // (npm install may fail offline for the unpublished version; we only assert the written pin.)
    if let Ok(txt) = std::fs::read_to_string(no_override.path().join("p").join("package.json")) {
        let v: Value = serde_json::from_str(&txt).unwrap();
        assert_eq!(
            v["dependencies"]["@orchestratr/sdk"],
            json!(default_pkg),
            "pinned SDK version must equal the CLI version"
        );
    }
    assert_eq!(pkg["dependencies"]["@orchestratr/sdk"], json!(sdk_spec));

    // Run the scaffolded workflow.ts against the mock provider — it must run green.
    let run = Command::new(tsx_bin())
        .arg(proj_dir.join("workflow.ts"))
        .current_dir(&proj_dir)
        .env("ORCR_HOME", ts.home_path())
        .env("ORCR_BIN", orcr_bin())
        .output()
        .expect("npx tsx workflow.ts");
    assert!(
        run.status.success(),
        "scaffolded workflow.ts failed:\n{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // Re-running scaffold on the same dir fails state_conflict, touching nothing.
    let out2 = Command::new(orcr_bin())
        .args(["scaffold", proj_dir.to_str().unwrap(), "--json"])
        .env("ORCR_HOME", ts.home_path())
        .env("ORCR_SDK_SPEC", &sdk_spec)
        .output()
        .expect("orcr scaffold re-run");
    let env: Value = serde_json::from_slice(&out2.stdout).unwrap_or(json!({}));
    assert_eq!(env["ok"], json!(false), "re-run should fail");
    assert_eq!(env["error"]["code"], json!("state_conflict"));

    let _ = std::fs::remove_file(&tarball);
}

/// SDK-composed paths equal the CLI's for the same nested scope (spec M7 acceptance — the
/// scope property, cross-checked against the real server rather than an oracle).
#[test]
fn e2e_sdk_scope_matches_cli() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start(&[]);
    // The SDK helper spawns an agent inside nested scopes and prints its resolved path; the CLI
    // spawns the equivalent (same `--path` fragments + `--name`) and we compare the two
    // server-resolved paths. Both use gc:immediate + wait so they self-clean, and we run them
    // sequentially so the shared name is free (herdr enforces session-global name uniqueness).
    let script = r#"
      import { orcr } from "@orchestratr/sdk";
      const a = await orcr.scope("prop_root", async () =>
        orcr.scope("phase_1", async () => {
          const h = await orcr.agent.run({ agent: "mock", name: "worker", gc: "immediate", prompt: "hi @say=x" });
          return h;
        }));
      console.log(a.path);
      await a.wait();
    "#;
    let scriptfile = sdk_dir().join("recipes").join("_prop_scope.ts");
    std::fs::write(&scriptfile, script).unwrap();
    let (ok, log) = ts.run_recipe("recipes/_prop_scope.ts", &[]);
    let _ = std::fs::remove_file(&scriptfile);
    assert!(ok, "scope script failed:\n{log}");
    let sdk_path = log
        .lines()
        .find(|l| l.contains('/'))
        .unwrap_or("")
        .trim()
        .to_string();
    assert_eq!(
        sdk_path, "prop_root/phase_1/worker",
        "SDK composed path mismatch (got `{sdk_path}`)"
    );

    // Wait for the SDK agent to fully drain from herdr so its `worker` name is free again.
    assert!(
        wait_until(Duration::from_secs(15), || ts.active_agents().is_empty()),
        "SDK scope agent did not clean up before CLI parity spawn"
    );

    // Spawn the CLI equivalent with the same nested scope fragments (the SDK composes
    // scope-path + name, so the CLI single `--path` is `prop_root/phase_1/worker`, leaf = name)
    // and compare the two server-resolved paths.
    let out = Command::new(orcr_bin())
        .args([
            "agent",
            "run",
            "--json",
            "--path",
            "prop_root/phase_1/worker",
            "--agent",
            "mock",
            "--gc",
            "immediate",
            "-p",
            "hi @say=x",
        ])
        .env("ORCR_HOME", ts.home_path())
        .output()
        .expect("orcr agent run");
    assert!(
        out.status.success(),
        "CLI agent run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let env: Value = serde_json::from_slice(&out.stdout).expect("CLI agent run JSON");
    let cli_path = env["result"]["agent"]["path"]
        .as_str()
        .or_else(|| env["agent"]["path"].as_str())
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        cli_path, sdk_path,
        "SDK-composed path (`{sdk_path}`) must equal the CLI's (`{cli_path}`)"
    );
    // Let the gc:immediate CLI agent clean up so no agents leak.
    let _ = wait_until(Duration::from_secs(15), || ts.active_agents().is_empty());
}

/// `AgentHandle.followLogs()` (spec §8, taught in skill/references/sdk.md) yields transcript
/// entries and terminates cleanly once the agent ends. Regression guard: `followLogs` must not
/// pass the agent uuid as an `agent.ls` glob pattern (a uuid is not a valid path segment, so the
/// server would reject the termination check with invalid_request).
#[test]
fn e2e_follow_logs_streams_to_completion() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let ts = TestServer::start(&[]);
    // Spawn an agent (gc:never so the ended record + transcript survive the drain), consume
    // followLogs in the background, and once it has done a turn, kill it so its status flips to
    // `ended` and the iterator terminates cleanly. Asserts entries are yielded and the async
    // iterator returns (a regression guard: the old `pattern: this.uuid` arg made the first
    // termination check throw invalid_request, so the iterator could never complete).
    let script = r#"
      import { orcr } from "@orchestratr/sdk";
      const h = await orcr.agent.run({ agent: "mock", name: "follower", gc: "never", prompt: "hi @say=hello" });
      const collected = [];
      const done = (async () => {
        for await (const e of h.followLogs({ intervalMs: 100 })) collected.push(e);
      })();
      // Wait until followLogs has streamed at least one transcript entry (proves live yielding),
      // then end the agent so its status flips to `ended` and the iterator drains + returns.
      const deadline = Date.now() + 20000;
      while (collected.length < 1 && Date.now() < deadline) await new Promise((r) => setTimeout(r, 100));
      if (collected.length < 1) { console.error("followLogs yielded no entries in time"); process.exit(1); }
      await h.kill();
      await done; // must terminate cleanly now that the agent has ended
      console.log("FOLLOW_OK entries=" + collected.length);
    "#;
    let scriptfile = sdk_dir().join("recipes").join("_follow_logs.ts");
    std::fs::write(&scriptfile, script).unwrap();
    let (ok, log) = ts.run_recipe("recipes/_follow_logs.ts", &[]);
    let _ = std::fs::remove_file(&scriptfile);
    assert!(ok, "followLogs script failed:\n{log}");
    assert!(
        log.contains("FOLLOW_OK entries="),
        "followLogs did not complete cleanly:\n{log}"
    );
    let _ = wait_until(Duration::from_secs(15), || ts.active_agents().is_empty());
}
