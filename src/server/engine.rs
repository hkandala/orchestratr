//! The agent engine: the queue worker, the spawn pipeline, the `agent.*` socket handlers,
//! and start-up reconciliation (spec §5.5, §11.1, §11.5).
//!
//! Runtime model: a single **queue worker** thread ticks every [`QUEUE_TICK`], running the
//! stuck-start guard and promoting queued agents (global + per-provider caps, FIFO). Each
//! promotion spawns a short-lived **pipeline** thread that drives the herdr side (ensure
//! session/workspace → `agent.start` → record location → capture `agent_session` → deliver
//! the first prompt → `working`), checking the `cancel_requested` interlock between steps.

use super::{agent_row_json, Server};
use crate::driver::{ensure_supported, launch_plan, AgentStartParams, HerdrBinary, HerdrDriver};
use crate::error::{OrcrError, Result};
use crate::path::{self, NameOrPath};
use crate::store::{now_millis, AgentFilter, AgentFull, NewAgent, UuidLookup};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the queue worker ticks (promotion + stuck-start guard).
const QUEUE_TICK: Duration = Duration::from_millis(200);
/// Delay between `send_text` and the submitting `Enter` (the two-call rule, §5.6).
const ENTER_DELAY: Duration = Duration::from_millis(1000);
/// How long to poll for herdr to report the `agent_session` transcript pointer (§11.1).
const SESSION_POLL: Duration = Duration::from_millis(3000);

/// The `launch.json` audit/recovery payload written to the agent's data dir (spec §12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchPayload {
    pub version: u32,
    pub uuid: String,
    pub path: String,
    pub provider: String,
    pub argv: Vec<String>,
    pub prompt: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub cwd: Option<String>,
    pub gc_mode: String,
    pub timeout: Option<String>,
    pub launch_token: String,
    /// How the effective path was derived (scope + input), for audit.
    pub effective_path: String,
    pub scope: Option<String>,
    /// The exact env injected into the pane (§5.3) — never the caller's whole environment.
    pub env: BTreeMap<String, String>,
    pub created_at: i64,
}

impl Server {
    // --- owned-session driver ---

    /// Connect to (and cache) the owned herdr session's driver, bootstrapping the session's
    /// headless server if needed (spec §5.2). Reconnects if the cached driver went stale.
    pub(super) fn owned_driver(&self) -> Result<HerdrDriver> {
        {
            let guard = self.inner.driver.lock().unwrap();
            if let Some(d) = guard.as_ref() {
                if d.ping().is_ok() {
                    return Ok(d.clone());
                }
            }
        }
        let bin = HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str()))?;
        let socket = bin.ensure_session(&self.inner.config.herdr.session)?;
        let driver = HerdrDriver::connect(&socket)?;
        *self.inner.driver.lock().unwrap() = Some(driver.clone());
        Ok(driver)
    }

    // --- queue worker ---

    /// Start the background queue worker (promotion + spawn dispatch + stuck-start guard).
    pub(super) fn start_queue_worker(&self) {
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                server.stuck_start_sweep();
                server.promote_and_dispatch();
                std::thread::sleep(QUEUE_TICK);
            }
        });
    }

    /// Fail any agent stuck in `starting` past `max_starting` with no pane recorded (§5.5),
    /// releasing its slot.
    fn stuck_start_sweep(&self) {
        let cutoff = now_millis() - self.inner.config.timings.max_starting.as_millis() as i64;
        let stuck = {
            let store = self.inner.store.lock().unwrap();
            store.stuck_starting(cutoff).unwrap_or_default()
        };
        for a in stuck {
            self.log().warn(format!(
                "stuck-start guard: agent {} ({}) made no progress in {:?} — failing",
                a.path, a.uuid, self.inner.config.timings.max_starting
            ));
            self.end_agent(&a.uuid, "failed");
        }
    }

    /// Promote queued agents (one tx) and spawn a pipeline thread per promotion.
    fn promote_and_dispatch(&self) {
        let promoted = {
            let mut store = self.inner.store.lock().unwrap();
            match store.promote_queued(
                self.inner.config.concurrency.max,
                &self.inner.config.concurrency.per_provider,
                now_millis(),
            ) {
                Ok((agents, ev)) => {
                    drop(store);
                    self.publish(ev);
                    agents
                }
                Err(e) => {
                    self.log().warn(format!("promotion failed: {e}"));
                    return;
                }
            }
        };
        for agent in promoted {
            let server = self.clone();
            std::thread::spawn(move || server.run_pipeline(agent));
        }
    }

    // --- spawn pipeline (§11.1) ---

    /// Drive one promoted agent through the herdr spawn pipeline. Any error fails the row
    /// (releasing its slot); a `cancel_requested` at any step ends it `canceled`.
    fn run_pipeline(&self, agent: AgentFull) {
        let uuid = agent.uuid.clone();
        if let Err(e) = self.pipeline_inner(&agent) {
            // A cancellation surfaced mid-pipeline is not a failure.
            if self.cancelled(&uuid) {
                return;
            }
            self.log()
                .warn(format!("spawn pipeline for {} failed: {e}", agent.path));
            self.end_agent(&uuid, "failed");
        }
    }

    fn pipeline_inner(&self, agent: &AgentFull) -> Result<()> {
        let uuid = &agent.uuid;

        // Read the launch payload (argv/env/prompt) written at enqueue time.
        let payload = self.read_launch(agent)?;

        self.bail_if_cancelled(uuid, None)?;
        let driver = self.owned_driver()?;

        // Ensure the level-1 workspace (label = home workspace, §5.2). A freshly created
        // workspace carries a root shell pane we close once the agent pane exists, so the
        // workspace auto-removes when the last agent leaves.
        self.bail_if_cancelled(uuid, None)?;
        let (workspace_id, root_pane) =
            self.ensure_workspace(&driver, &path::home_workspace(&agent.path))?;

        // agent.start — herdr creates the tab + pane; returned ids are authoritative (§11.7).
        self.bail_if_cancelled(uuid, None)?;
        let params = AgentStartParams {
            name: path::tab_label(&agent.path),
            argv: payload.argv.clone(),
            cwd: payload.cwd.clone(),
            env: payload.env.clone(),
            focus: false,
            split: None,
            tab_id: None,
            workspace_id: Some(workspace_id),
        };
        let info = match driver.agent_start(&params) {
            Ok(i) => i,
            Err(e) => {
                if let Some(root) = &root_pane {
                    let _ = driver.pane_close(root);
                }
                return Err(e);
            }
        };
        if let Some(root) = &root_pane {
            let _ = driver.pane_close(root);
        }

        // The stuck-start guard or a kill may have ended the row while agent.start ran; if
        // so, close the pane we just created (no duplicate survives) and stop.
        let session = self.inner.config.herdr.session.clone();
        {
            let store = self.inner.store.lock().unwrap();
            match store.agent_full(uuid)? {
                Some(cur) if cur.status == "starting" && !cur.cancel_requested => {}
                _ => {
                    drop(store);
                    let _ = driver.pane_close(&info.pane_id);
                    self.bail_if_cancelled(uuid, None)?;
                    return Ok(()); // row already ended (e.g. by the guard)
                }
            }
        }

        // Record the location — this is the "progress marker" that disarms the guard.
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.record_location(uuid, &session, &info.terminal_id, &info.pane_id)?
        };
        self.publish(ev);

        // Capture the transcript pointer if herdr reports it (best-effort, §11.1).
        self.capture_agent_session(&driver, uuid, &info.pane_id);

        // Deliver the first prompt (two-call rule) if one was given, opening turn 1; else
        // just move `starting → working`. The completion monitor takes over from here (§5.6).
        self.bail_if_cancelled(uuid, Some((&driver, &info.pane_id)))?;
        let ev = if let Some(prompt) = payload.prompt.as_deref().filter(|p| !p.is_empty()) {
            driver.pane_send_text(&info.pane_id, prompt)?;
            std::thread::sleep(ENTER_DELAY);
            driver.pane_send_keys(&info.pane_id, &["Enter"])?;
            let mut store = self.inner.store.lock().unwrap();
            let (_seq, ev) = store.deliver_input(uuid, "orcr", now_millis())?;
            ev
        } else {
            let mut store = self.inner.store.lock().unwrap();
            store.transition_status(uuid, "working", None)?
        };
        self.publish(ev);
        self.log().info(format!(
            "agent {} working (pane {})",
            agent.path, info.pane_id
        ));
        Ok(())
    }

    /// Ensure a workspace labeled `label` exists; returns its id and, when freshly created,
    /// the root shell pane to close after the agent pane is in place. Serialized so
    /// concurrent spawns under the same level-1 segment never create duplicate workspaces.
    fn ensure_workspace(
        &self,
        driver: &HerdrDriver,
        label: &str,
    ) -> Result<(String, Option<String>)> {
        let _guard = self.inner.spawn_lock.lock().unwrap();
        if let Some(w) = driver
            .workspace_list()?
            .into_iter()
            .find(|w| w.label == label)
        {
            return Ok((w.workspace_id, None));
        }
        let created = driver.workspace_create(Some(label), None, &BTreeMap::new())?;
        Ok((
            created.workspace.workspace_id,
            Some(created.root_pane.pane_id),
        ))
    }

    /// Poll herdr briefly for the agent's `agent_session` transcript pointer; record it when
    /// present (the gate for `logs` in M3). Missing is fine here.
    fn capture_agent_session(&self, driver: &HerdrDriver, uuid: &str, pane_id: &str) {
        let deadline = std::time::Instant::now() + SESSION_POLL;
        loop {
            if let Ok(pane) = driver.pane_get(pane_id) {
                if let Some(sess) = pane.agent_session {
                    let kind = match sess.kind {
                        crate::driver::AgentSessionRefKind::Id => "id",
                        crate::driver::AgentSessionRefKind::Path => "path",
                    };
                    let mut store = self.inner.store.lock().unwrap();
                    let _ = store.record_agent_session(uuid, kind, &sess.value);
                    return;
                }
            }
            // A pending cancel makes waiting for the transcript pointer pointless — bail so
            // the kill resolves promptly.
            if std::time::Instant::now() >= deadline || self.cancelled(uuid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Bail out of the pipeline if cancellation was requested; close the pane first when one
    /// exists, then end the row `canceled` (§5.5).
    fn bail_if_cancelled(&self, uuid: &str, pane: Option<(&HerdrDriver, &str)>) -> Result<()> {
        if self.cancelled(uuid) {
            if let Some((driver, pane_id)) = pane {
                let _ = driver.pane_close(pane_id);
            }
            self.end_agent(uuid, "canceled");
            return Err(OrcrError::server_error("canceled", "spawn canceled"));
        }
        Ok(())
    }

    fn cancelled(&self, uuid: &str) -> bool {
        let store = self.inner.store.lock().unwrap();
        store.is_cancel_requested(uuid).unwrap_or(false)
    }

    /// End an agent row (`ended` + `exit_reason`) and publish the events. Idempotent-ish:
    /// a missing row is ignored.
    pub(super) fn end_agent(&self, uuid: &str, exit_reason: &str) {
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.transition_status(uuid, "ended", Some(exit_reason))
        };
        match ev {
            Ok(seq) => self.publish(seq),
            Err(e) => self.log().warn(format!("end_agent {uuid} failed: {e}")),
        }
    }

    // --- reconciliation on start (§11.5) ---

    /// Repair the store against herdr reality on start (spec §11.1 crash recovery, §11.5):
    /// managed agents left `starting`/`working` are matched to live panes; unmatched panes
    /// that belong to an in-flight spawn (by tab label in the home workspace) are closed so
    /// no duplicate survives; rows whose pane vanished are failed/lost.
    pub(super) fn reconcile_on_start(&self) {
        let agents = {
            let store = self.inner.store.lock().unwrap();
            store.active_managed_agents().unwrap_or_default()
        };
        if agents.is_empty() {
            return;
        }
        let driver = match self.owned_driver() {
            Ok(d) => d,
            Err(e) => {
                // herdr unreachable: never free names on an outage alone (§11.5). Leave rows.
                self.log()
                    .warn(format!("reconcile: herdr unreachable, leaving rows: {e}"));
                return;
            }
        };
        let panes = driver.pane_list(None).unwrap_or_default();
        for a in agents {
            self.reconcile_agent(&driver, &panes, &a);
        }
    }

    fn reconcile_agent(
        &self,
        driver: &HerdrDriver,
        panes: &[crate::driver::PaneInfo],
        a: &AgentFull,
    ) {
        match a.pane_id.as_deref() {
            // A pane was recorded before the crash: confirm it, else the pane vanished.
            Some(pane_id) => {
                let present = panes.iter().any(|p| {
                    p.pane_id == pane_id
                        && a.terminal_id
                            .as_deref()
                            .map(|t| p.terminal_id == t)
                            .unwrap_or(true)
                });
                if present {
                    if a.status == "starting" {
                        // Recovered mid-spawn but the pane exists → complete to working.
                        let ev = {
                            let mut s = self.inner.store.lock().unwrap();
                            s.transition_status(&a.uuid, "working", None)
                        };
                        if let Ok(seq) = ev {
                            self.publish(seq);
                        }
                        self.log()
                            .info(format!("reconcile: repaired {} to working", a.path));
                    }
                } else if a.status == "starting" {
                    self.end_agent(&a.uuid, "failed");
                } else {
                    // A running agent's pane vanished outside orcr's control → lost (§5.6).
                    let ev = {
                        let mut s = self.inner.store.lock().unwrap();
                        s.transition_status(&a.uuid, "lost", None)
                    };
                    if let Ok(seq) = ev {
                        self.publish(seq);
                    }
                    self.log()
                        .warn(format!("reconcile: {} marked lost", a.path));
                }
            }
            // No pane recorded: an in-flight spawn crashed before recording it. Match any
            // orphan pane by its tab label (unique among active paths) and close it, then
            // fail the row — no duplicate pane survives (§11.1).
            None => {
                let label = path::tab_label(&a.path);
                for p in panes.iter().filter(|p| p.label.as_deref() == Some(&label)) {
                    let _ = driver.pane_close(&p.pane_id);
                    self.log().warn(format!(
                        "reconcile: closed orphan pane {} for {}",
                        p.pane_id, a.path
                    ));
                }
                self.end_agent(&a.uuid, "failed");
            }
        }
    }

    // --- handlers ---

    /// `agent.run` (spec §6.1, §11.1): validate + resolve identity, write the launch payload,
    /// enqueue the durable row, and return `{agent, permissions}`.
    pub(super) fn handle_agent_run(&self, params: &Value) -> Result<Value> {
        let name = str_param(params, "name");
        let path_in = str_param(params, "path");
        let input = match (name, path_in) {
            (Some(n), None) => NameOrPath::Name(n),
            (None, Some(p)) => NameOrPath::Path(p),
            (Some(_), Some(_)) => {
                return Err(OrcrError::invalid_request(
                    "pass exactly one of --name or --path",
                    "name_and_path",
                ))
            }
            (None, None) => {
                return Err(OrcrError::invalid_request(
                    "naming is mandatory: pass --name or --path",
                    "name_required",
                ))
            }
        };

        let provider = str_param(params, "agent")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.inner.config.defaults.agent.clone());

        // Both-layers-required (§11.4): fail fast before any resolution/side effect.
        ensure_supported(&self.integration_state_typed(), &provider)?;

        let gc = str_param(params, "gc").unwrap_or_else(|| "auto".to_string());
        if !matches!(gc.as_str(), "auto" | "immediate" | "never") {
            return Err(OrcrError::invalid_request(
                format!("invalid --gc `{gc}` (auto|immediate|never)"),
                "invalid_gc",
            ));
        }
        let model = str_param(params, "model").filter(|s| !s.is_empty());
        let effort = str_param(params, "effort").filter(|s| !s.is_empty());
        let timeout = str_param(params, "timeout").filter(|s| !s.is_empty());
        // Validate --timeout up front (units required, §6); persist the deadline durably.
        let timeout_ms = match &timeout {
            Some(t) => Some(crate::duration::parse_duration(t)?.as_millis() as i64),
            None => None,
        };
        let cwd = str_param(params, "cwd").filter(|s| !s.is_empty());
        let prompt = str_param(params, "prompt");

        // Caller identity → scope + lineage (§5.3). A managed agent's scope is its path minus
        // its name; a plain shell has none. (loop-run scope lands in M5.)
        let caller_id = str_param(params, "caller_id").filter(|s| !s.is_empty());
        let caller_path = str_param(params, "caller_path").filter(|s| !s.is_empty());
        let scope = caller_path.as_deref().and_then(path::scope_of_agent);

        let effective = path::resolve_create(scope.as_deref(), &input)?;

        // Build the launch plan (argv + model/effort mapping).
        let plan = launch_plan(&provider, model.as_deref(), effort.as_deref())?;

        // Allocate identity + the launch token (unique per attempt, §11.1).
        let uuid = uuid::Uuid::now_v7().to_string();
        let launch_token = uuid::Uuid::new_v4().to_string();
        let data_dir = self.agent_data_dir(&effective, &uuid);

        // Env contract (§5.3). All values absolute.
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("ORCR_ID".into(), uuid.clone());
        env.insert("ORCR_PATH".into(), effective.clone());
        if let (Some(pid), Some(ppath)) = (&caller_id, &caller_path) {
            env.insert("ORCR_PARENT_ID".into(), pid.clone());
            env.insert("ORCR_PARENT_PATH".into(), ppath.clone());
        }
        env.insert("ORCR_AGENT_DATA_DIR".into(), data_dir.display().to_string());
        // So a nested `orcr` call reaches the same server (relocated homes, tests).
        env.insert(
            "ORCR_HOME".into(),
            self.inner.home.root().display().to_string(),
        );
        // The launch token rides in pane env for crash recovery (not part of the contract).
        env.insert("ORCR_LAUNCH_TOKEN".into(), launch_token.clone());

        // Write the data dir + launch.json before the durable row so the pipeline (which may
        // promote immediately) always finds the payload; a failed enqueue cleans the dir.
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            OrcrError::server_error("data_dir", format!("cannot create data dir: {e}"))
        })?;
        let payload = LaunchPayload {
            version: 1,
            uuid: uuid.clone(),
            path: effective.clone(),
            provider: provider.clone(),
            argv: plan.argv.clone(),
            prompt: prompt.clone(),
            model: model.clone(),
            effort: effort.clone(),
            cwd: cwd.clone(),
            gc_mode: gc.clone(),
            timeout: timeout.clone(),
            launch_token: launch_token.clone(),
            effective_path: effective.clone(),
            scope: scope.clone(),
            env,
            created_at: now_millis(),
        };
        std::fs::write(
            data_dir.join("launch.json"),
            serde_json::to_vec_pretty(&payload).unwrap(),
        )
        .map_err(|e| {
            OrcrError::server_error("data_dir", format!("cannot write launch.json: {e}"))
        })?;

        let new = NewAgent {
            uuid: uuid.clone(),
            path: effective.clone(),
            managed: true,
            origin: "run".into(),
            parent_id: caller_id.clone(),
            agent: Some(provider.clone()),
            model: model.clone(),
            effort: effort.clone(),
            gc_mode: Some(gc.clone()),
            cwd: cwd.clone(),
            herdr_session: Some(self.inner.config.herdr.session.clone()),
            terminal_id: None,
            pane_id: None,
            launch_token: Some(launch_token.clone()),
            status: "queued".into(),
            deadline_at: timeout_ms.map(|ms| now_millis() + ms),
            created_at: now_millis(),
        };
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            match store.enqueue_agent(&new) {
                Ok((_seq, ev)) => ev,
                Err(e) => {
                    drop(store);
                    let _ = std::fs::remove_dir_all(&data_dir);
                    return Err(e);
                }
            }
        };
        self.publish(ev);

        let queue_position = {
            let store = self.inner.store.lock().unwrap();
            store.queue_position(&uuid).ok().flatten()
        };
        let mut agent_obj = json!({
            "uuid": uuid,
            "path": effective,
            "status": "queued",
            "agent": provider,
            "managed": true,
            "cwd": cwd,
            "data_dir": data_dir.display().to_string(),
        });
        if let Some(q) = queue_position {
            agent_obj["queue_position"] = json!(q);
        }
        if let Some(pid) = &caller_id {
            agent_obj["parent_id"] = json!(pid);
        }
        if let Some(ppath) = &caller_path {
            agent_obj["parent_path"] = json!(ppath);
        }
        Ok(json!({ "agent": agent_obj, "permissions": "bypass" }))
    }

    /// `agent.send` (spec §6.1): exact target; deliver the prompt (two-call) and report
    /// `delivered_while` + `input_seq`. Wildcards are rejected; ended targets → `not_found`.
    pub(super) fn handle_agent_send(&self, params: &Value) -> Result<Value> {
        let target = str_param(params, "target").ok_or_else(|| {
            OrcrError::invalid_request("send requires a target", "target_required")
        })?;
        let prompt = str_param(params, "prompt").unwrap_or_default();
        let scope = caller_scope(params);
        let row = self.resolve_singleton(&scope, &target)?;
        if row.status == "ended" || row.status == "lost" {
            return Err(OrcrError::not_found(format!(
                "agent `{target}` is not active (status {})",
                row.status
            )));
        }
        let pane_id = row.pane_id.clone().ok_or_else(|| {
            OrcrError::state_conflict(format!(
                "agent `{}` has no live pane yet (status {})",
                row.path, row.status
            ))
            .with_details(json!({ "current_status": row.status }))
        })?;
        let delivered_while = row.status.clone();

        let driver = self.owned_driver()?;
        driver.pane_send_text(&pane_id, &prompt)?;
        std::thread::sleep(ENTER_DELAY);
        driver.pane_send_keys(&pane_id, &["Enter"])?;
        // Open a new turn and re-arm to `working` (a `send` cancels any pending block/idle so
        // a `wait` issued after it cannot be satisfied by a stale idle, §5.6).
        let (input_seq, ev) = {
            let mut store = self.inner.store.lock().unwrap();
            store.deliver_input(&row.uuid, "orcr", now_millis())?
        };
        self.publish(ev);
        Ok(json!({
            "uuid": row.uuid,
            "path": row.path,
            "delivered_while": delivered_while,
            "input_seq": input_seq,
        }))
    }

    /// `agent.kill` (spec §6.1): patterns + uuids. With `preview`, returns the matched set
    /// (for the CLI's TTY confirmation) without side effects. Otherwise kills each matched
    /// active agent and returns `{killed, skipped, all_killed}`.
    pub(super) fn handle_agent_kill(&self, params: &Value) -> Result<Value> {
        let targets: Vec<String> = params
            .get("targets")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if targets.is_empty() {
            return Err(OrcrError::invalid_request(
                "kill requires targets",
                "target_required",
            ));
        }
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let preview = params
            .get("preview")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let scope = caller_scope(params);

        let matched = self.resolve_targets(&scope, &targets)?;
        if matched.is_empty() {
            return Err(OrcrError::not_found(format!(
                "no active agents matched {targets:?}"
            )));
        }

        if preview {
            let rows: Vec<Value> = matched
                .iter()
                .map(|a| {
                    json!({
                        "uuid": a.uuid, "path": a.path, "status": a.status, "managed": a.managed,
                    })
                })
                .collect();
            return Ok(json!({ "preview": true, "targets": rows }));
        }

        let driver = self.owned_driver().ok();
        let mut killed = Vec::new();
        let mut skipped = Vec::new();
        for a in matched {
            if !a.managed && !force {
                skipped.push(json!({ "uuid": a.uuid, "path": a.path, "reason": "force_required" }));
                continue;
            }
            match a.status.as_str() {
                "queued" => {
                    self.end_agent(&a.uuid, "canceled");
                    killed.push(json!({ "uuid": a.uuid, "path": a.path }));
                }
                "starting" => {
                    // Cancel via the interlock; close the pane if one already exists.
                    {
                        let mut store = self.inner.store.lock().unwrap();
                        let _ = store.request_cancel(&a.uuid);
                    }
                    if let (Some(d), Some(pane)) = (&driver, &a.pane_id) {
                        let _ = d.pane_close(pane);
                    }
                    self.end_agent(&a.uuid, "canceled");
                    killed.push(json!({ "uuid": a.uuid, "path": a.path }));
                }
                _ => {
                    // working / idle / blocked / parked: graceful shutdown → pane close.
                    if let (Some(d), Some(pane)) = (&driver, &a.pane_id) {
                        self.graceful_shutdown(d, &a, pane);
                    }
                    self.end_agent(&a.uuid, "killed");
                    killed.push(json!({ "uuid": a.uuid, "path": a.path }));
                }
            }
        }
        let all_killed = skipped.is_empty();
        Ok(json!({ "killed": killed, "skipped": skipped, "all_killed": all_killed }))
    }

    /// The per-integration graceful shutdown recipe → pane close (§6.1). Best-effort: the
    /// pane close is the hard guarantee (herdr then clears empty tabs/workspaces).
    pub(super) fn graceful_shutdown(&self, driver: &HerdrDriver, a: &AgentFull, pane_id: &str) {
        if let Some(provider) = &a.agent {
            if let Ok(plan) = launch_plan(provider, None, None) {
                if let Some(line) = plan.shutdown_line {
                    let _ = driver.pane_send_text(pane_id, &line);
                    let _ = driver.pane_send_keys(pane_id, &["Enter"]);
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        let _ = driver.pane_close(pane_id);
    }

    /// `agent.ls` (spec §6.1): active (and, with `all`, ended) agents as flat rows.
    pub(super) fn handle_agent_ls(&self, params: &Value) -> Result<Value> {
        let scope = caller_scope(params);
        let pattern = match str_param(params, "pattern").filter(|s| !s.is_empty()) {
            Some(p) => Some(path::resolve_selector(scope.as_deref(), &p)?),
            None => None,
        };
        let filter = AgentFilter {
            pattern,
            provider: str_param(params, "agent").filter(|s| !s.is_empty()),
            status: str_param(params, "status").filter(|s| !s.is_empty()),
            managed: match (
                params
                    .get("managed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                params
                    .get("unmanaged")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            ) {
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            },
            include_ended: params.get("all").and_then(|v| v.as_bool()).unwrap_or(false),
        };
        let store = self.inner.store.lock().unwrap();
        let rows = store.list_agents(&filter)?;
        let agents: Vec<Value> = rows.iter().map(|a| agent_row_json(&store, a)).collect();
        Ok(json!({ "agents": agents }))
    }

    // --- resolution helpers ---

    /// Resolve an exact singleton target (§5.1): wildcards rejected; path-first then uuid.
    fn resolve_singleton(&self, scope: &Option<String>, raw: &str) -> Result<AgentFull> {
        if path::is_pattern(raw) {
            return Err(OrcrError::invalid_request(
                format!("`{raw}` is a pattern; this verb takes an exact target"),
                "wildcard_not_allowed",
            ));
        }
        let store = self.inner.store.lock().unwrap();
        if raw.contains('-') {
            return uuid_lookup(store.find_by_uuid_or_prefix(raw)?, raw);
        }
        let resolved = path::resolve_selector(scope.as_deref(), raw)?;
        if let Some(res) = store.find_by_path(&resolved)? {
            return Ok(res.row().clone());
        }
        // Fall back to a uuid prefix (≥ 8 hex) only if nothing matched as a path.
        if is_uuid_prefix(raw) {
            return uuid_lookup(store.find_by_uuid_or_prefix(raw)?, raw);
        }
        Err(OrcrError::not_found(format!("no agent matched `{raw}`")))
    }

    /// Resolve bulk kill targets (§5.1): each target may be a pattern, a path, or a uuid.
    /// Returns the deduplicated set of matched **active** agents.
    fn resolve_targets(
        &self,
        scope: &Option<String>,
        targets: &[String],
    ) -> Result<Vec<AgentFull>> {
        let store = self.inner.store.lock().unwrap();
        let active = store.list_agents(&AgentFilter::default())?;
        let mut out: Vec<AgentFull> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for raw in targets {
            if path::is_pattern(raw) {
                let resolved = path::resolve_selector(scope.as_deref(), raw)?;
                let pat = crate::path::Pattern::compile(&resolved)?;
                for a in active.iter().filter(|a| pat.matches(&a.path)) {
                    if seen.insert(a.uuid.clone()) {
                        out.push(a.clone());
                    }
                }
            } else if raw.contains('-') {
                if let UuidLookup::Found(a) = store.find_by_uuid_or_prefix(raw)? {
                    if a.status != "ended" && seen.insert(a.uuid.clone()) {
                        out.push(*a);
                    }
                }
            } else {
                let resolved = path::resolve_selector(scope.as_deref(), raw)?;
                if let Some(res) = store.find_by_path(&resolved)? {
                    let a = res.row().clone();
                    if a.status != "ended" && seen.insert(a.uuid.clone()) {
                        out.push(a);
                    }
                } else if is_uuid_prefix(raw) {
                    if let UuidLookup::Found(a) = store.find_by_uuid_or_prefix(raw)? {
                        if a.status != "ended" && seen.insert(a.uuid.clone()) {
                            out.push(*a);
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// The typed integration state (both-layers picture, §11.4).
    fn integration_state_typed(&self) -> crate::driver::IntegrationState {
        let raw = HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str()))
            .and_then(|b| b.integration_status_raw())
            .unwrap_or_default();
        crate::driver::IntegrationState::from_herdr_status(&raw)
    }

    /// The agent's data dir, mirroring its path (§8): `<home>/data/<segs>/<uuid>`.
    fn agent_data_dir(&self, path: &str, uuid: &str) -> PathBuf {
        let mut dir = self.inner.home.data_dir();
        for seg in path.split('/') {
            dir.push(seg);
        }
        dir.push(uuid);
        dir
    }

    /// Read the launch payload for an agent from its data dir.
    fn read_launch(&self, agent: &AgentFull) -> Result<LaunchPayload> {
        let file = self
            .agent_data_dir(&agent.path, &agent.uuid)
            .join("launch.json");
        let text = std::fs::read_to_string(&file).map_err(|e| {
            OrcrError::server_error(
                "launch_missing",
                format!("cannot read {}: {e}", file.display()),
            )
        })?;
        serde_json::from_str(&text)
            .map_err(|e| OrcrError::server_error("launch_decode", format!("bad launch.json: {e}")))
    }
}

/// Extract a string param.
fn str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(|v| v.as_str()).map(String::from)
}

/// The caller scope from `caller_path` (an agent's scope = its path minus its name, §5.3).
fn caller_scope(params: &Value) -> Option<String> {
    str_param(params, "caller_path")
        .filter(|s| !s.is_empty())
        .and_then(|p| path::scope_of_agent(&p))
}

/// A uuid prefix candidate: ≥ 8 chars, all hex (§5.1).
fn is_uuid_prefix(s: &str) -> bool {
    s.len() >= 8 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Turn a [`UuidLookup`] into a single row or the right error.
fn uuid_lookup(lookup: UuidLookup, raw: &str) -> Result<AgentFull> {
    match lookup {
        UuidLookup::Found(a) => Ok(*a),
        UuidLookup::Ambiguous(cands) => {
            let prefixes: Vec<String> =
                cands.iter().map(|u| u.chars().take(12).collect()).collect();
            Err(
                OrcrError::not_found(format!("uuid prefix `{raw}` is ambiguous"))
                    .with_details(json!({ "target": raw, "candidates": prefixes })),
            )
        }
        UuidLookup::NotFound => Err(OrcrError::not_found(format!("no agent matched `{raw}`"))),
    }
}
