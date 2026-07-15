//! The agent engine: the queue worker, the spawn pipeline, the `agent.*` socket handlers,
//! and start-up reconciliation.
//!
//! Runtime model: a single **queue worker** thread ticks every [`QUEUE_TICK`], running the
//! stuck-start guard and promoting queued agents (global + per-provider caps, FIFO). Each
//! promotion spawns a short-lived **pipeline** thread that drives the herdr side (ensure
//! session/workspace → `agent.start` → record location → capture `agent_session` → deliver
//! the first prompt → `working`), checking the `cancel_requested` interlock between steps.

use super::params::{str_array_param, str_param};
use super::{agent_row_json, Server};
use crate::driver::{
    ensure_supported, launch_plan, tuning_for, AgentStartParams, AgentStatus, HerdrBinary,
    HerdrDriver, ReadSource, TuningParams,
};
use crate::error::{OrcrError, Result};
use crate::path::{self, NameOrPath};
use crate::store::{now_millis, AgentFilter, AgentFull, NewAgent, Store, UuidLookup};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the queue worker ticks (promotion + stuck-start guard).
const QUEUE_TICK: Duration = Duration::from_millis(200);
/// Delay between `send_text` and the submitting `Enter` (the two-call rule).
const ENTER_DELAY: Duration = Duration::from_millis(1000);
/// Interval between submit-confirmation polls / Enter re-sends (known-issues #2).
const SUBMIT_POLL: Duration = Duration::from_millis(400);
/// Delay between a *re-sent* `send_text` and its `Enter` (shorter than the first delivery's
/// [`ENTER_DELAY`] so the adaptive re-send loop can make several attempts within the budget).
const RESEND_ENTER_DELAY: Duration = Duration::from_millis(300);
/// How long to poll for herdr to report the `agent_session` transcript pointer.
const SESSION_POLL: Duration = Duration::from_millis(3000);

/// The `launch.json` audit/recovery payload written to the agent's data dir.
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
    /// The exact env injected into the pane — never the caller's whole environment.
    pub env: BTreeMap<String, String>,
    pub created_at: i64,
}

impl Server {
    // --- owned-session driver ---

    /// Connect to (and cache) the owned herdr session's driver, bootstrapping the session's
    /// headless server if needed. Reconnects if the cached driver went stale.
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

    /// Connect a driver to the herdr session an agent's pane actually lives in.
    /// Managed agents live in the owned session (cached driver); an **unmanaged** agent lives
    /// in a *foreign* session (per-socket, per m0 notes) whose socket we discover here — its
    /// `pane_id` is workspace-scoped and only meaningful on that session's socket, so routing
    /// `send`/`kill` through the owned driver would hit the wrong (or no) pane.
    pub(super) fn driver_for_agent(&self, a: &AgentFull) -> Result<HerdrDriver> {
        let owned = &self.inner.config.herdr.session;
        let session = a.herdr_session.as_deref().unwrap_or(owned);
        if session == owned {
            return self.owned_driver();
        }
        let bin = HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str()))?;
        let sess = bin.find_session(session)?.ok_or_else(|| {
            OrcrError::state_conflict(format!("herdr session `{session}` is not running"))
        })?;
        let socket = sess.socket_path.ok_or_else(|| {
            OrcrError::state_conflict(format!("herdr session `{session}` has no socket"))
        })?;
        HerdrDriver::connect(&socket)
    }

    /// The agent's current pane id **on `driver`'s session**. `terminal_id` is globally unique
    /// and stable across pane moves, so we resolve the live pane by it (a stale `pane_id` from a
    /// prior discovery tick or a pane move would otherwise mis-address); falls back to the
    /// recorded `pane_id` when the terminal can't be located.
    pub(super) fn live_pane_id(&self, driver: &HerdrDriver, a: &AgentFull) -> Option<String> {
        if let Some(term) = a.terminal_id.as_deref() {
            if let Ok(panes) = driver.pane_list(None) {
                if let Some(p) = panes.iter().find(|p| p.terminal_id == term) {
                    return Some(p.pane_id.clone());
                }
            }
        }
        a.pane_id.clone()
    }

    /// [`live_pane_id`](Self::live_pane_id) tolerating an unavailable driver (herdr unreachable):
    /// falls back to the recorded `pane_id`.
    pub(super) fn live_pane(&self, driver: Option<&HerdrDriver>, a: &AgentFull) -> Option<String> {
        match driver {
            Some(d) => self.live_pane_id(d, a),
            None => a.pane_id.clone(),
        }
    }

    /// The per-agent move mutex (created on first use). Held across a two-phase park/un-park so
    /// a GC park and a `send` un-park for the same agent can never interleave.
    pub(super) fn lock_move(&self, uuid: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
        let mut map = self.inner.move_locks.lock().unwrap();
        map.entry(uuid.to_string())
            .or_insert_with(|| std::sync::Arc::new(std::sync::Mutex::new(())))
            .clone()
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

    /// Fail any agent stuck in `starting` past `max_starting` with no pane recorded,
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

    // --- spawn pipeline ---

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
            // If agent.start already created the pane and a *later* step failed (deliver / send /
            // settle), close it before failing the row: an `ended(failed)` row is never revisited
            // by reconciliation, so a live (paid) provider process would otherwise leak — only
            // surfacing later as an unknown_marked_pane in drift, cleaned by hand.
            // Best-effort; no-op when the failure preceded agent.start (nothing was created).
            self.close_pipeline_pane(&uuid);
            self.end_agent(&uuid, "failed");
        }
    }

    /// Close the pane recorded for a mid-pipeline-failed agent (best-effort). A no-op when no
    /// pane was ever recorded (failure before `agent.start`, or the agent.start-itself failure
    /// which already closes its own root pane) — nothing was leaked in those cases.
    fn close_pipeline_pane(&self, uuid: &str) {
        let row = {
            let store = self.inner.store.lock().unwrap();
            store.agent_full(uuid).ok().flatten()
        };
        let Some(a) = row else { return };
        if a.pane_id.is_none() {
            return;
        }
        let driver = self.driver_for_agent(&a).ok();
        if let (Some(d), Some(pane)) = (&driver, self.live_pane(driver.as_ref(), &a)) {
            let _ = d.pane_close(&pane);
        }
    }

    fn pipeline_inner(&self, agent: &AgentFull) -> Result<()> {
        let uuid = &agent.uuid;

        // Read the launch payload (argv/env/prompt) written at enqueue time.
        let payload = self.read_launch(agent)?;

        self.bail_if_cancelled(uuid, None)?;
        let driver = self.owned_driver()?;

        // Ensure the level-1 workspace (label = home workspace). A freshly created
        // workspace carries a root shell pane we close once the agent pane exists, so the
        // workspace auto-removes when the last agent leaves.
        self.bail_if_cancelled(uuid, None)?;
        let (workspace_id, root_pane) =
            self.ensure_workspace(&driver, &path::home_workspace(&agent.path))?;

        // agent.start — herdr creates the tab + pane; returned ids are authoritative.
        self.bail_if_cancelled(uuid, None)?;
        let params = AgentStartParams {
            name: path::herdr_name(&agent.path),
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

        // Capture the transcript pointer if herdr reports it (best-effort).
        self.capture_agent_session(&driver, uuid, &info.pane_id);

        // Deliver the first prompt (two-call rule) if one was given, opening turn 1; else
        // settle the primed agent to `idle`. The completion monitor takes over from here.
        self.bail_if_cancelled(uuid, Some((&driver, &info.pane_id)))?;
        let phase = if let Some(prompt) = payload.prompt.as_deref().filter(|p| !p.is_empty()) {
            // Open turn 1 / bump input_seq / re-arm to `working` BEFORE the herdr send, so the
            // agent's public status leads delivery (input_seq increments before delivery;
            // a stale idle can never satisfy the new turn, and no synthetic external turn can be
            // opened in the send gap). A herdr send failure here propagates to run_pipeline, which
            // closes the pane and ends the row `failed` (no leaked provider process).
            let ev = {
                let mut store = self.inner.store.lock().unwrap();
                store.deliver_input(uuid, "orcr", now_millis())?
            };
            // A `None` here means a concurrent kill already ended the row in the gap after the
            // last cancel check: don't revive it or send to its (closing) pane — surface
            // the cancel and let the kill path finish the teardown.
            let Some((_, ev)) = ev else {
                return self.bail_if_cancelled(uuid, Some((&driver, &info.pane_id)));
            };
            self.publish(ev);
            // Deliver the first prompt with readiness + submission verification: the real-provider
            // boot race can drop the first `send_text`/`Enter`, leaving the prompt unsubmitted so
            // the turn never starts (known-issues #2 / E02).
            self.deliver_prompt(&driver, &info.pane_id, prompt, &payload.provider, true)?;
            "working"
        } else {
            // No prompt: the agent is primed and waiting for input, not processing. Settle it to
            // `idle` (with an idle clock) so `wait` completes as turn_complete, gc-auto can park
            // it, and a later `send` re-arms it normally. Guarded on `starting` so a
            // concurrent kill that already ended the row isn't revived to `idle`.
            let ev = {
                let mut store = self.inner.store.lock().unwrap();
                store.settle_primed_idle(uuid, now_millis())?
            };
            let Some(ev) = ev else {
                return self.bail_if_cancelled(uuid, Some((&driver, &info.pane_id)));
            };
            self.publish(ev);
            "idle"
        };
        self.log().info(format!(
            "agent {} {phase} (pane {})",
            agent.path, info.pane_id
        ));
        Ok(())
    }

    /// Ensure a workspace labeled `label` exists; returns its id and, when freshly created,
    /// the root shell pane to close after the agent pane is in place. Serialized so
    /// concurrent spawns under the same level-1 segment never create duplicate workspaces.
    pub(super) fn ensure_workspace(
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
                    let mut store = self.inner.store.lock().unwrap();
                    let _ = store.record_agent_session(uuid, sess.kind.as_str(), &sess.value);
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

    /// Deliver a prompt via the two-call rule and, for a managed real-provider agent,
    /// robustly verify it actually submitted — re-driving the delivery if the provider TUI
    /// dropped it on a slow boot (known-issues #2 / E02).
    ///
    /// `verify` selects the robust path (readiness wait → send → verify → adaptive re-send).
    /// When it is `false` (an unmanaged agent, whose input orcr doesn't own) or the provider's
    /// `submit_confirm_ms` tuning is `0` (the mock's line-based stdin, which accepts the first
    /// `Enter` reliably), this degrades to the plain two-call delivery. The initial `send_text`
    /// / `send_keys` are mandatory (a herdr failure fails the whole spawn/send); re-sends are
    /// best-effort.
    fn deliver_prompt(
        &self,
        driver: &HerdrDriver,
        pane_id: &str,
        prompt: &str,
        provider: &str,
        verify: bool,
    ) -> Result<()> {
        let tuning = tuning_for(provider, &self.inner.config.integrations);
        if verify && tuning.submit_confirm_ms > 0 {
            // Wait (bounded) for the TUI to be ready to accept input, so the first `send_text`
            // isn't dropped mid-boot (the deeper half of the flake).
            self.await_input_ready(driver, pane_id, &tuning);
        }
        driver.pane_send_text(pane_id, prompt)?;
        std::thread::sleep(ENTER_DELAY);
        driver.pane_send_keys(pane_id, &["Enter"])?;
        if verify && tuning.submit_confirm_ms > 0 {
            self.confirm_submitted(driver, pane_id, prompt, &tuning);
        }
        Ok(())
    }

    /// Wait (bounded by `submit_ready_ms`) for the provider TUI to be ready to accept input:
    /// herdr reports the pane's agent in a real state (the integration attached and drew the
    /// input box) or the rendered pane content has stabilized (boot spew settled). Best-effort —
    /// returns as soon as it looks ready, or when the budget elapses (delivery proceeds anyway).
    fn await_input_ready(&self, driver: &HerdrDriver, pane_id: &str, tuning: &TuningParams) {
        if tuning.submit_ready_ms == 0 {
            return;
        }
        let deadline = std::time::Instant::now() + Duration::from_millis(tuning.submit_ready_ms);
        let mut prev: Option<String> = None;
        loop {
            // A real reported agent_status (not `unknown`) means the provider integration has
            // attached — the TUI is interactive and accepting input.
            if matches!(self.pane_agent_status(driver, pane_id), Some(s) if s != AgentStatus::Unknown)
            {
                return;
            }
            // Fall back to content stability: identical non-empty renders across a poll means the
            // boot output has settled and the input box is drawn.
            if let Ok(read) = driver.pane_read(pane_id, ReadSource::Visible, None) {
                let text = read.text.trim().to_string();
                if !text.is_empty() && prev.as_deref() == Some(text.as_str()) {
                    return;
                }
                prev = Some(text);
            }
            if std::time::Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(SUBMIT_POLL);
        }
    }

    /// Verify a just-delivered prompt actually submitted, re-driving the delivery until a turn is
    /// underway or `submit_confirm_ms` elapses (known-issues #2 / E02).
    ///
    /// Two distinct real-provider failure modes, distinguished by READING the pane:
    /// - **Dropped `Enter`** (observed with claude on a slow boot): the `send_text` lands so the
    ///   prompt is sitting in the input box, but the submitting `Enter` was silently dropped. The
    ///   fix is to re-send a bare `Enter` — the prompt is already typed, so re-typing it would
    ///   stack duplicates. Once submitted, the input box clears: a prompt we saw and that is now
    ///   gone means it submitted (even if herdr's `agent_status` lags), so we stop.
    /// - **Dropped `send_text`** (the TUI wasn't accepting input yet): the prompt was never typed,
    ///   so the pane never shows it. Only here do we re-send the FULL delivery (bounded by
    ///   `submit_attempts`) — never after a prompt we already saw, so a real provider's typed box
    ///   is never double-delivered. A read failure is treated conservatively as "prompt present".
    fn confirm_submitted(
        &self,
        driver: &HerdrDriver,
        pane_id: &str,
        prompt: &str,
        tuning: &TuningParams,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_millis(tuning.submit_confirm_ms);
        let mut ever_shown = false;
        let mut resends = 0u32;
        loop {
            std::thread::sleep(SUBMIT_POLL);
            if self.pane_submitted(driver, pane_id) {
                return; // a turn is underway — the prompt was accepted
            }
            if std::time::Instant::now() >= deadline {
                self.log().warn(format!(
                    "submit-confirm: pane {pane_id} still idle after {}ms — prompt may not have \
                     been accepted by the provider TUI",
                    tuning.submit_confirm_ms
                ));
                return;
            }
            if self.pane_shows_prompt(driver, pane_id, prompt) {
                // The prompt is in the box (or the read failed — conservative): it just needs the
                // submitting Enter (the dropped-Enter case). Re-typing here would stack duplicates.
                ever_shown = true;
                let _ = driver.pane_send_keys(pane_id, &["Enter"]);
            } else if ever_shown {
                // We saw the prompt and now it's gone: a bare Enter submitted it and the box
                // cleared — the turn is underway even if herdr's status still lags. Done.
                return;
            } else if resends < tuning.submit_attempts {
                // The prompt was never seen: the earlier `send_text` was dropped before the TUI
                // was ready. Re-send the FULL delivery (not just a bare Enter).
                resends += 1;
                self.log().warn(format!(
                    "submit-confirm: pane {pane_id} never showed the prompt — re-delivering \
                     (attempt {resends})"
                ));
                let _ = driver.pane_send_text(pane_id, prompt);
                std::thread::sleep(RESEND_ENTER_DELAY);
                let _ = driver.pane_send_keys(pane_id, &["Enter"]);
            } else {
                // Exhausted full re-deliveries: keep nudging with Enter until the deadline.
                let _ = driver.pane_send_keys(pane_id, &["Enter"]);
            }
        }
    }

    /// The pane's herdr-reported `agent_status`, if the pane is currently in `agent.list`.
    fn pane_agent_status(&self, driver: &HerdrDriver, pane_id: &str) -> Option<AgentStatus> {
        driver
            .agent_list()
            .ok()
            .and_then(|list| list.into_iter().find(|a| a.pane_id == pane_id))
            .map(|a| a.agent_status)
    }

    /// True once the pane's herdr agent reports a non-idle state (working/blocked/done) — the
    /// prompt was accepted and a turn is (or was) underway. `idle`/`unknown` (or an unreadable
    /// pane) means not-yet-submitted.
    fn pane_submitted(&self, driver: &HerdrDriver, pane_id: &str) -> bool {
        matches!(
            self.pane_agent_status(driver, pane_id),
            Some(AgentStatus::Working | AgentStatus::Blocked | AgentStatus::Done)
        )
    }

    /// True if the pane's rendered content still shows the (unsubmitted) prompt — so it only needs
    /// the submitting `Enter`, not a re-typed prompt. Conservative: a failed/empty read, or an
    /// empty prompt, returns `true` so orcr never spuriously re-types into a box that may already
    /// hold the prompt (which would double-deliver on a real provider).
    fn pane_shows_prompt(&self, driver: &HerdrDriver, pane_id: &str, prompt: &str) -> bool {
        // Match on the first non-empty line (capped) — enough to identify the prompt without
        // tripping over terminal wrapping of a long prompt.
        let needle: String = prompt
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .chars()
            .take(60)
            .collect();
        if needle.is_empty() {
            return true;
        }
        match driver.pane_read(pane_id, ReadSource::Visible, None) {
            Ok(read) => read.text.contains(&needle),
            Err(_) => true,
        }
    }

    /// Bail out of the pipeline if cancellation was requested; close the pane first when one
    /// exists, then end the row `canceled`.
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

    // --- reconciliation on start ---

    /// Repair the store against herdr reality on start (crash recovery):
    /// managed agents left `starting`/`working` are matched to live panes; unmatched panes
    /// that belong to an in-flight spawn (by tab label in the home workspace) are closed so
    /// no duplicate survives; rows whose pane vanished are failed/lost.
    pub(super) fn reconcile_on_start(&self) {
        // Conservative re-arm: forget any pre-crash idle streak for mid-turn agents so
        // completion re-measures from a fresh transition, and restart the park clock for
        // already-idle agents so GC still parks them after a restart.
        {
            let mut store = self.inner.store.lock().unwrap();
            let _ = store.rearm_idle_clocks_on_restart();
        }
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
                // herdr unreachable: never free names on an outage alone. Leave rows.
                self.log()
                    .warn(format!("reconcile: herdr unreachable, leaving rows: {e}"));
                return;
            }
        };
        let panes = driver.pane_list(None).unwrap_or_default();
        for a in agents {
            // Agents with a move in flight are settled by terminal_id in move recovery below —
            // the pane-id confirm pass would wrongly see a just-moved pane as vanished.
            if a.move_state != "none" {
                continue;
            }
            self.reconcile_agent(&driver, &panes, &a);
        }
        // Recover any half-done park/un-park moves + refresh drift. Lost
        // *resolution* is deferred to a following periodic poll.
        self.reconcile_moves_on_start();
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
                    // A running agent's pane vanished outside orcr's control → lost.
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
            // orphan pane by its herdr name/label (the full path — session-unique) and close
            // it, then fail the row — no duplicate pane survives.
            None => {
                let label = path::herdr_name(&a.path);
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

    /// `agent.run`: validate + resolve identity, write the launch payload,
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

        // Both-layers-required: fail fast before any resolution/side effect.
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
        // Validate --timeout up front (units required); persist the deadline durably.
        let timeout_ms = match &timeout {
            Some(t) => Some(crate::duration::parse_duration(t)?.as_millis() as i64),
            None => None,
        };
        let cwd = str_param(params, "cwd").filter(|s| !s.is_empty());
        let prompt = str_param(params, "prompt");

        // Caller identity → scope + lineage. An agent's scope is its path minus its
        // name; a **loop run** is a directory, so its scope is its whole run path; a plain
        // shell has none.
        let caller_id = str_param(params, "caller_id").filter(|s| !s.is_empty());
        let caller_path = str_param(params, "caller_path").filter(|s| !s.is_empty());
        let ctx = self.caller_context(caller_id.as_deref(), caller_path.as_deref());
        let scope = ctx.scope.clone();

        let effective = path::resolve_create(scope.as_deref(), &input)?;

        // Build the launch plan (argv + model/effort mapping).
        let plan = launch_plan(&provider, model.as_deref(), effort.as_deref())?;

        // Allocate identity + the launch token (unique per attempt).
        let uuid = uuid::Uuid::now_v7().to_string();
        let launch_token = uuid::Uuid::new_v4().to_string();
        let data_dir = self.agent_data_dir(&effective, &uuid);

        // Env contract. All values absolute.
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        env.insert("ORCR_ID".into(), uuid.clone());
        env.insert("ORCR_PATH".into(), effective.clone());
        if let (Some(pid), Some(ppath)) = (&ctx.parent_id, &ctx.parent_path) {
            env.insert("ORCR_PARENT_ID".into(), pid.clone());
            env.insert("ORCR_PARENT_PATH".into(), ppath.clone());
        }
        env.insert("ORCR_AGENT_DATA_DIR".into(), data_dir.display().to_string());
        // ORCR_LOOP_DATA_DIR is set for every agent descended from a loop run: the
        // loop's shared scratch dir, derived from the effective path's level-1 loop name.
        if let Some(dir) = self.loop_data_dir_for(&effective) {
            env.insert("ORCR_LOOP_DATA_DIR".into(), dir);
        }
        // So a nested `orcr` call reaches the same server (relocated homes, tests).
        env.insert(
            "ORCR_HOME".into(),
            self.inner.home.root().display().to_string(),
        );
        // The launch token rides in pane env for crash recovery (not part of the contract).
        env.insert("ORCR_LAUNCH_TOKEN".into(), launch_token.clone());
        // Test-only: the `mock` provider is scriptable via `ORCR_MOCK_*` env. Forward any such
        // vars from the server's environment into the pane so e2e tests can drive mock behavior
        // (e.g. `ORCR_MOCK_NO_TRANSCRIPT`) deterministically. The `mock` provider only exists
        // under `ORCR_ALLOW_MOCK_PROVIDER`, so real providers never see these.
        if provider == "mock" {
            for (k, v) in std::env::vars() {
                if k.starts_with("ORCR_MOCK_") {
                    env.insert(k, v);
                }
            }
        }

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
            parent_id: ctx.parent_id.clone(),
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
        // Active-loop namespace protection + run admission barrier and the
        // path-reserving insert run under **one** store-lock hold, so loop-namespace ownership
        // is enforced atomically with path reservation (no TOCTOU — the single writer means a
        // `loop.create` cannot interleave between the check and the insert).
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            let result = self
                .check_loop_namespace(&store, &effective, caller_path.as_deref())
                .and_then(|()| store.enqueue_agent(&new));
            match result {
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
        if let Some(pid) = &ctx.parent_id {
            agent_obj["parent_id"] = json!(pid);
        }
        if let Some(ppath) = &ctx.parent_path {
            agent_obj["parent_path"] = json!(ppath);
        }
        Ok(json!({ "agent": agent_obj, "permissions": "bypass" }))
    }

    /// `agent.send`: exact target; deliver the prompt (two-call) and report
    /// `delivered_while` + `input_seq`. Wildcards are rejected; ended targets → `not_found`.
    pub(super) fn handle_agent_send(&self, params: &Value) -> Result<Value> {
        let target = str_param(params, "target").ok_or_else(|| {
            OrcrError::invalid_request("send requires a target", "target_required")
        })?;
        let prompt = str_param(params, "prompt").unwrap_or_default();
        let scope = self.caller_scope_full(params);
        let mut row = self.resolve_singleton(&scope, &target)?;
        if row.status == "ended" || row.status == "lost" {
            return Err(OrcrError::not_found_target(
                format!("agent `{target}` is not active (status {})", row.status),
                target.clone(),
            ));
        }

        // Route to the herdr session the pane actually lives in: an unmanaged agent's pane is
        // in a *foreign* session, and its `pane_id` is only meaningful on that session's socket.
        // Managed agents use the owned session's cached driver.
        let driver = self.driver_for_agent(&row)?;

        // Managed agents can be parked/moved by GC concurrently. Hold the per-agent move lock
        // across un-park + delivery so a park can't relocate the pane mid-send, and re-read the
        // row under the lock so a park that committed just before we acquired it is observed
        // (avoids a send racing a live two-phase move).
        let move_lock = if row.managed {
            Some(self.lock_move(&row.uuid))
        } else {
            None
        };
        let _held = move_lock.as_ref().map(|m| m.lock().unwrap());
        if move_lock.is_some() {
            row = {
                let store = self.inner.store.lock().unwrap();
                store.agent_full(&row.uuid)?.ok_or_else(|| {
                    OrcrError::not_found_target(
                        format!("agent `{target}` vanished"),
                        target.clone(),
                    )
                })?
            };
            if row.status == "ended" || row.status == "lost" {
                return Err(OrcrError::not_found_target(
                    format!("agent `{target}` is not active (status {})", row.status),
                    target.clone(),
                ));
            }
        }

        let delivered_while = row.status.clone();
        // Sending to a parked (or mid-move) agent un-parks it first — atomically, before
        // delivery — and delivery then addresses the confirmed post-move location. The
        // per-agent move lock is already held, so this never pre-empts a live GC park.
        if row.status == "parked" || row.move_state != "none" {
            row = self.unpark_for_send(&driver, &row)?;
        }
        let pane_id = self.live_pane_id(&driver, &row).ok_or_else(|| {
            OrcrError::state_conflict(format!(
                "agent `{}` has no live pane yet (status {})",
                row.path, row.status
            ))
            .with_details(json!({ "current_status": row.status }))
        })?;
        // Open a new turn / bump input_seq / re-arm to `working` BEFORE the herdr send (
        // input_seq increments before delivery, so a `wait` issued after this send can never be
        // satisfied by a stale idle and no synthetic external turn is opened in the send gap). A
        // herdr send failure then leaves the turn open (agent shows `working`, never completes →
        // visible in `top`) rather than dropping the input silently. An unmanaged agent's turn is
        // tracked as `external`: orcr doesn't own its input epochs.
        let delivered = {
            let mut store = self.inner.store.lock().unwrap();
            if row.managed {
                store.deliver_input(&row.uuid, "orcr", now_millis())?
            } else {
                store.open_external_turn(&row.uuid, now_millis())?
            }
        };
        // `None` = the row ended concurrently (kill/reconcile/discovery) between the active-check
        // above and delivery: refuse to revive it — report it as gone rather than sending.
        let Some((input_seq, ev)) = delivered else {
            return Err(OrcrError::not_found_target(
                format!("agent `{target}` is not active (ended concurrently)"),
                target.clone(),
            ));
        };
        self.publish(ev);
        // Deliver with readiness + submission verification for a managed real-provider agent
        // (known-issues #2 / E02). Unmanaged agents live in a foreign session and orcr
        // doesn't drive their input epochs, so it uses the plain two-call delivery for them.
        self.deliver_prompt(
            &driver,
            &pane_id,
            &prompt,
            row.agent.as_deref().unwrap_or_default(),
            row.managed,
        )?;
        Ok(json!({
            "uuid": row.uuid,
            "path": row.path,
            "delivered_while": delivered_while,
            "input_seq": input_seq,
        }))
    }

    /// `agent.kill`: patterns + uuids. With `preview`, returns the matched set
    /// (for the CLI's TTY confirmation) without side effects. Otherwise kills each matched
    /// active agent and returns `{killed, skipped, all_killed}`.
    pub(super) fn handle_agent_kill(&self, params: &Value) -> Result<Value> {
        let targets = str_array_param(params, "targets");
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
        let scope = self.caller_scope_full(params);

        let matched = self.resolve_targets(&scope, &targets)?;
        if matched.is_empty() {
            // An exact (path/uuid) target that resolves only to an already-ended
            // agent is a "matched but skipped" case (reason: ended) → exit 7, not a no-match
            // (exit 6). Globs range over active agents only, so this is exact targets alone.
            let ended = self.resolve_ended_targets(&scope, &targets)?;
            if ended.is_empty() {
                return Err(OrcrError::not_found_target(
                    format!("no active agents matched {targets:?}"),
                    json!(targets),
                ));
            }
            if preview {
                let rows: Vec<Value> = ended
                    .iter()
                    .map(|a| {
                        json!({ "uuid": a.uuid, "path": a.path, "status": a.status, "managed": a.managed })
                    })
                    .collect();
                return Ok(json!({ "preview": true, "targets": rows }));
            }
            let skipped: Vec<Value> = ended
                .iter()
                .map(|a| json!({ "uuid": a.uuid, "path": a.path, "reason": "ended" }))
                .collect();
            return Ok(json!({ "killed": [], "skipped": skipped, "all_killed": false }));
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

        let mut killed = Vec::new();
        let mut skipped = Vec::new();
        for a in matched {
            if !a.managed && !force {
                skipped.push(json!({ "uuid": a.uuid, "path": a.path, "reason": "force_required" }));
                continue;
            }
            // Test-only: widen the kill-during-promotion window (E07) so the race is
            // deterministic in the regression test. A no-op in production.
            kill_iter_delay_hook();
            // Re-read the row under the store lock at action time. The queue worker's
            // `promote_and_dispatch` runs on a separate thread and can transition this agent
            // queued → starting → running (spawning its herdr pane) in the window between the
            // kill's target snapshot and here (E07). Acting on the stale snapshot would classify
            // a now-running agent as `queued` and dequeue it pane-less, leaking the just-spawned
            // pane. A row that already ended since the snapshot (raced by another kill / the
            // stuck-start guard) is skipped.
            let a = {
                let store = self.inner.store.lock().unwrap();
                match store.agent_full(&a.uuid)? {
                    Some(cur) if cur.status != "ended" => cur,
                    _ => continue,
                }
            };
            // Route to the session the pane lives in: an unmanaged agent's pane is in a foreign
            // herdr session, so closing it via the owned driver would miss it (or close a
            // colliding owned pane). Managed agents resolve to the owned session's driver.
            let driver = self.driver_for_agent(&a).ok();
            if a.status == "queued" {
                // Dequeue atomically, but ONLY while still queued. Promotion moves queued →
                // starting under the store lock *before* it ever spawns a pane, so a successful
                // guarded end proves no pane exists — the pane-less cancel is safe. If it
                // lost the race to promotion, fall through to `kill_live_agent` so the freshly
                // spawned pane is closed rather than leaked (E07).
                let canceled = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.end_if_status(&a.uuid, "queued", "canceled")?
                };
                if let Some(seq) = canceled {
                    self.publish(seq);
                    killed.push(json!({ "uuid": a.uuid, "path": a.path }));
                    continue;
                }
                // Promoted since the re-read — re-read once more and kill it as a live agent.
                let promoted = {
                    let store = self.inner.store.lock().unwrap();
                    store.agent_full(&a.uuid)?
                };
                match promoted {
                    Some(cur) if cur.status != "ended" => {
                        self.kill_live_agent(&cur, driver.as_ref(), &mut killed)
                    }
                    _ => continue,
                }
                continue;
            }
            self.kill_live_agent(&a, driver.as_ref(), &mut killed);
        }
        let all_killed = skipped.is_empty();
        Ok(json!({ "killed": killed, "skipped": skipped, "all_killed": all_killed }))
    }

    /// Kill an agent that is starting or running (and may already have a live herdr pane): cancel
    /// an in-flight spawn via the interlock and close its pane, or graceful-shutdown a running
    /// agent, then end the row. Shared by the kill's direct starting/running path and the
    /// queued→promoted fall-through (E07). `driver` addresses the session the pane lives in.
    fn kill_live_agent(
        &self,
        a: &AgentFull,
        driver: Option<&HerdrDriver>,
        killed: &mut Vec<Value>,
    ) {
        if a.status == "starting" {
            // Set the cancel interlock BEFORE closing, so the spawn pipeline closes any pane it
            // created but has not yet recorded (the pane-created-but-unrecorded sub-window, its
            // post-`agent.start` re-check and `bail_if_cancelled` both close on a set cancel /
            // ended row); close the pane here too if one is already recorded.
            {
                let mut store = self.inner.store.lock().unwrap();
                let _ = store.request_cancel(&a.uuid);
            }
            if let (Some(d), Some(pane)) = (driver, self.live_pane(driver, a)) {
                let _ = d.pane_close(&pane);
            }
            self.end_agent(&a.uuid, "canceled");
            killed.push(json!({ "uuid": a.uuid, "path": a.path }));
            return;
        }
        // working / idle / blocked / parked / lost: graceful shutdown → pane close. Hold the
        // per-agent move lock (managed) so GC can't relocate the pane mid-kill.
        let move_lock = if a.managed {
            Some(self.lock_move(&a.uuid))
        } else {
            None
        };
        let _held = move_lock.as_ref().map(|m| m.lock().unwrap());
        if let (Some(d), Some(pane)) = (driver, self.live_pane(driver, a)) {
            self.graceful_shutdown(d, a, &pane);
        }
        // An explicit kill resolving a `lost` agent ends it as `lost`, not `killed`
        // (reconciliation OR explicit kill → ended (exit_reason: lost)).
        let reason = if a.status == "lost" { "lost" } else { "killed" };
        self.end_agent(&a.uuid, reason);
        killed.push(json!({ "uuid": a.uuid, "path": a.path }));
    }

    /// The per-integration graceful shutdown recipe → pane close. Best-effort: the
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

    /// `agent.ls`: active (and, with `all`, ended) agents as flat rows.
    pub(super) fn handle_agent_ls(&self, params: &Value) -> Result<Value> {
        let scope = self.caller_scope_full(params);
        let raw = str_param(params, "pattern").filter(|s| !s.is_empty());

        // Accepts `<pattern|uuid>`: a full uuid (dashes can't be path chars) or a ≥8-hex
        // prefix that names no path resolves to that single agent (git-style), mirroring
        // wait/kill — not a literal path glob.
        if let Some(r) = &raw {
            let store = self.inner.store.lock().unwrap();
            let is_uuidish = r.contains('-')
                || (path::looks_like_uuid_selector(r)
                    && store
                        .find_by_path(&path::resolve_selector(scope.as_deref(), r)?)?
                        .is_none());
            if is_uuidish {
                let a = uuid_lookup(store.find_by_uuid_or_prefix(r)?, r)?;
                let row = agent_row_json(&store, &a);
                return Ok(json!({ "agents": [row] }));
            }
        }

        let pattern = match raw {
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

    /// `agent.wait`: block until **every** snapshotted target settles, then
    /// return one `{uuid,path,status,ok,reason,exit_reason?,next}` row per target plus
    /// `all_ok`/`timed_out`/`decision_seq`. Membership is the set of **active** agents
    /// matching the targets at invocation (snapshot-then-subscribe on the event bus, so no
    /// transition is missed). A target that un-settles is waited on again — the result is the
    /// state at one simultaneous `decision_seq`.
    pub(super) fn handle_agent_wait(&self, params: &Value) -> Result<Value> {
        let targets = str_array_param(params, "targets");
        if targets.is_empty() {
            return Err(OrcrError::invalid_request(
                "wait requires at least one target",
                "target_required",
            ));
        }
        let scope = self.caller_scope_full(params);
        let timeout_ms = match str_param(params, "timeout").filter(|s| !s.is_empty()) {
            Some(t) => Some(crate::duration::parse_duration(&t)?.as_millis() as i64),
            None => None,
        };

        // Snapshot membership: the active agents matching any target at invocation.
        let members = self.resolve_targets(&scope, &targets)?;
        if members.is_empty() {
            return Err(OrcrError::not_found_target(
                format!("no active agents matched {targets:?}"),
                json!(targets),
            ));
        }
        let member_uuids: Vec<String> = members.iter().map(|a| a.uuid.clone()).collect();

        let deadline =
            timeout_ms.map(|ms| std::time::Instant::now() + Duration::from_millis(ms as u64));
        loop {
            // One consistent read: all target rows + the current event cursor.
            let (rows, decision_seq) = {
                let store = self.inner.store.lock().unwrap();
                let mut rows = Vec::with_capacity(member_uuids.len());
                for u in &member_uuids {
                    if let Some(a) = store.agent_full(u)? {
                        rows.push(a);
                    }
                }
                (rows, store.latest_event_seq().unwrap_or(0))
            };

            let all_settled = rows.iter().all(|a| settle_of(a).is_some());
            let timed_out = deadline
                .map(|d| std::time::Instant::now() >= d)
                .unwrap_or(false);

            if all_settled || timed_out {
                return Ok(wait_result(&rows, decision_seq, timed_out));
            }

            // Wait for the next event (bounded so timeout is honored promptly).
            let poll = Duration::from_millis(250);
            match self.inner.bus.wait_for(decision_seq + 1, poll) {
                crate::events::WaitOutcome::ShuttingDown => {
                    return Ok(wait_result(&rows, decision_seq, timed_out));
                }
                _ => continue,
            }
        }
    }

    /// `agent.ask`: documented sugar — `run --gc immediate` → settle `wait` →
    /// `logs --last-response`. Naming rules are identical to `run`. Blocks through the queue
    /// and the first completion, then returns `{uuid, path, response}`.
    pub(super) fn handle_agent_ask(&self, params: &Value) -> Result<Value> {
        // Force gc=immediate, then reuse the run path (naming enforcement included).
        let mut run_params = params.clone();
        if let Some(obj) = run_params.as_object_mut() {
            obj.insert("gc".into(), json!("immediate"));
        }
        let run = self.handle_agent_run(&run_params)?;
        let uuid = run["agent"]["uuid"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let path = run["agent"]["path"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let timeout_str = str_param(params, "timeout").filter(|s| !s.is_empty());
        let wait_params = json!({ "targets": [uuid.clone()], "timeout": timeout_str });
        let waited = self.handle_agent_wait(&wait_params)?;
        let target = &waited["targets"][0];
        let reason = target["reason"].as_str().unwrap_or("");
        // Blocked → exit 4.
        if reason.starts_with("blocked") {
            let kind = reason.strip_prefix("blocked:").unwrap_or("unknown");
            return Err(
                OrcrError::new(crate::error::ErrorCode::Blocked, "agent is blocked")
                    .with_details(json!({ "blocked_kind": kind, "uuid": uuid, "path": path })),
            );
        }
        // The agent's OWN `--timeout` expiring (ended with exit_reason=timeout) → exit 3 (
        // the `timeout` code is reserved for an agent's/run's own deadline). This can win the race
        // against the wait's own timer when the agent's deadline kills it first — settle_of then
        // reports reason "timeout" with timed_out=false, so we must map it here explicitly.
        if reason == "timeout" {
            return Err(
                OrcrError::new(crate::error::ErrorCode::Timeout, "agent timed out")
                    .with_details(json!({ "uuid": uuid, "path": path })),
            );
        }
        if waited["timed_out"].as_bool() == Some(true) {
            return Err(OrcrError::new(
                crate::error::ErrorCode::Timeout,
                "ask timed out waiting for completion",
            )
            .with_details(json!({ "uuid": uuid, "path": path })));
        }

        // Read the last response from the native transcript (fails loudly).
        let text = {
            let store = self.inner.store.lock().unwrap();
            let a = store.agent_full(&uuid)?.ok_or_else(|| {
                OrcrError::not_found_target(format!("agent {uuid} vanished"), uuid.clone())
            })?;
            drop(store);
            let loc = self.agent_transcript(&a)?;
            self.last_response_fresh(&a, &loc)?
        };
        Ok(json!({
            "uuid": uuid,
            "path": path,
            "response": { "text": text, "final": true },
        }))
    }

    /// `agent.logs`: read the provider's native transcript. `last_response` returns
    /// only the final assistant message (fails loudly); otherwise structured entries
    /// (optionally the last `tail`). History is addressed by uuid; a path resolves active-first.
    pub(super) fn handle_agent_logs(&self, params: &Value) -> Result<Value> {
        let target = str_param(params, "target").ok_or_else(|| {
            OrcrError::invalid_request("logs requires a target", "target_required")
        })?;
        let scope = self.caller_scope_full(params);
        let last_response = params
            .get("last_response")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let tail = params.get("tail").and_then(|v| v.as_u64());

        let (row, resolved) = self.resolve_singleton_tagged(&scope, &target)?;

        // Both-layers-required for logs: an unsupported provider → integration_missing.
        if let Some(provider) = &row.agent {
            ensure_supported(&self.integration_state_typed(), provider)?;
        }

        let loc = self.agent_transcript(&row)?;
        if last_response {
            let text = self.last_response_fresh(&row, &loc)?;
            return Ok(json!({
                "uuid": row.uuid, "path": row.path, "resolved": resolved,
                "created_at": row.created_at,
                "response": { "text": text, "final": true },
            }));
        }
        let mut entries = loc.read_entries()?;
        if let Some(n) = tail {
            let n = n as usize;
            if entries.len() > n {
                entries = entries.split_off(entries.len() - n);
            }
        }
        Ok(json!({
            "uuid": row.uuid, "path": row.path, "resolved": resolved,
            "created_at": row.created_at,
            "entries": entries,
        }))
    }

    /// `agent.attach.prepare`: the one terminal-mediated verb. Validates the
    /// target, **inserts the attach lease first** and reads the live location under the same
    /// transaction (so GC can never move/reap between resolution and lease), and returns the
    /// `herdr agent attach` exec command. Queued/starting/ended/lost → `state_conflict`.
    pub(super) fn handle_agent_attach_prepare(&self, params: &Value) -> Result<Value> {
        let target = str_param(params, "target").ok_or_else(|| {
            OrcrError::invalid_request("attach requires a target", "target_required")
        })?;
        let takeover = params
            .get("takeover")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let client_pid = params
            .get("client_pid")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let scope = self.caller_scope_full(params);
        let row = self.resolve_singleton(&scope, &target)?;

        let lease_id = uuid::Uuid::new_v4().to_string();
        let mode = if takeover { "takeover" } else { "observe" };
        let ttl_ms = self.inner.config.timings.attach_lease_ttl.as_millis() as i64;
        let (info, ev) = {
            let mut store = self.inner.store.lock().unwrap();
            store.prepare_attach(&row.uuid, &lease_id, mode, "cli", client_pid, ttl_ms)?
        };
        self.publish(ev);

        // Build the exec command. `terminal_id` is globally unique and stable across pane
        // moves, so it addresses the target even after a park/un-park.
        let bin = HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str()))?;
        let session = info
            .herdr_session
            .clone()
            .unwrap_or_else(|| self.inner.config.herdr.session.clone());
        let mut command = vec![
            bin.path().display().to_string(),
            "--session".to_string(),
            session,
            "agent".to_string(),
            "attach".to_string(),
            info.terminal_id.clone(),
        ];
        if takeover {
            command.push("--takeover".to_string());
        }
        Ok(json!({
            "uuid": row.uuid,
            "path": row.path,
            "lease_id": lease_id,
            "takeover": takeover,
            "ttl_ms": ttl_ms,
            "command": command,
        }))
    }

    /// `agent.attach.heartbeat`: keep the lease fresh while the CLI is attached.
    pub(super) fn handle_agent_attach_heartbeat(&self, params: &Value) -> Result<Value> {
        let lease_id = str_param(params, "lease_id").ok_or_else(|| {
            OrcrError::invalid_request("heartbeat requires lease_id", "lease_required")
        })?;
        let ttl_ms = self.inner.config.timings.attach_lease_ttl.as_millis() as i64;
        let ok = {
            let mut store = self.inner.store.lock().unwrap();
            store.heartbeat_lease(&lease_id, ttl_ms)?
        };
        Ok(json!({ "ok": ok }))
    }

    /// `agent.attach.release`: drop the lease on detach; GC resumes.
    pub(super) fn handle_agent_attach_release(&self, params: &Value) -> Result<Value> {
        let lease_id = str_param(params, "lease_id").ok_or_else(|| {
            OrcrError::invalid_request("release requires lease_id", "lease_required")
        })?;
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.release_lease(&lease_id)?
        };
        self.publish(ev);
        Ok(json!({ "released": ev > 0 }))
    }

    // --- resolution helpers ---

    /// Resolve an exact singleton target: wildcards rejected; path-first then uuid.
    fn resolve_singleton(&self, scope: &Option<String>, raw: &str) -> Result<AgentFull> {
        Ok(self.resolve_singleton_tagged(scope, raw)?.0)
    }

    /// Like [`resolve_singleton`] but also reports how it resolved (`active` | `latest_ended`)
    /// — used by `logs`.
    fn resolve_singleton_tagged(
        &self,
        scope: &Option<String>,
        raw: &str,
    ) -> Result<(AgentFull, &'static str)> {
        if path::is_pattern(raw) {
            return Err(OrcrError::invalid_request(
                format!("`{raw}` is a pattern; this verb takes an exact target"),
                "wildcard_not_allowed",
            ));
        }
        let store = self.inner.store.lock().unwrap();
        let tag_of = |a: &AgentFull| {
            if a.status == "ended" {
                "latest_ended"
            } else {
                "active"
            }
        };
        if raw.contains('-') {
            let a = uuid_lookup(store.find_by_uuid_or_prefix(raw)?, raw)?;
            let tag = tag_of(&a);
            return Ok((a, tag));
        }
        let resolved = path::resolve_selector(scope.as_deref(), raw)?;
        if let Some(res) = store.find_by_path(&resolved)? {
            let tag = res.tag();
            return Ok((res.row().clone(), tag));
        }
        if path::looks_like_uuid_selector(raw) {
            let a = uuid_lookup(store.find_by_uuid_or_prefix(raw)?, raw)?;
            let tag = tag_of(&a);
            return Ok((a, tag));
        }
        Err(OrcrError::not_found_target(
            format!("no agent matched `{raw}`"),
            raw,
        ))
    }

    /// Resolve bulk kill targets: each target may be a pattern, a path, or a uuid.
    /// Returns the deduplicated set of matched **active** agents.
    fn resolve_targets(
        &self,
        scope: &Option<String>,
        targets: &[String],
    ) -> Result<Vec<AgentFull>> {
        self.resolve_targets_where(scope, targets, true, |a| a.status != "ended")
    }

    /// Exact (path/uuid) targets that resolve only to an **ended** agent. Kill uses
    /// this when no active agent matched, to emit `skipped:[{reason:"ended"}]` (exit 7) instead
    /// of a no-match (exit 6). Patterns are excluded — a glob ranges over active agents only.
    fn resolve_ended_targets(
        &self,
        scope: &Option<String>,
        targets: &[String],
    ) -> Result<Vec<AgentFull>> {
        self.resolve_targets_where(scope, targets, false, |a| a.status == "ended")
    }

    /// Shared resolver behind [`resolve_targets`]/[`resolve_ended_targets`]: walk each
    /// target (pattern / uuid-with-dash / path-or-uuid-prefix), keep every matched row for which
    /// `keep` holds, deduplicated by uuid. `allow_patterns=false` skips glob targets entirely (a
    /// glob ranges over active agents only), so the ended-target path never lists the fleet.
    fn resolve_targets_where(
        &self,
        scope: &Option<String>,
        targets: &[String],
        allow_patterns: bool,
        keep: impl Fn(&AgentFull) -> bool,
    ) -> Result<Vec<AgentFull>> {
        let store = self.inner.store.lock().unwrap();
        let active = if allow_patterns {
            store.list_agents(&AgentFilter::default())?
        } else {
            Vec::new()
        };
        let mut out: Vec<AgentFull> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        // Add an agent iff `keep` holds and it wasn't already collected (dedup by uuid).
        let push = |a: AgentFull,
                    out: &mut Vec<AgentFull>,
                    seen: &mut std::collections::BTreeSet<String>| {
            if keep(&a) && seen.insert(a.uuid.clone()) {
                out.push(a);
            }
        };
        for raw in targets {
            if path::is_pattern(raw) {
                if !allow_patterns {
                    continue;
                }
                let resolved = path::resolve_selector(scope.as_deref(), raw)?;
                let pat = crate::path::Pattern::compile(&resolved)?;
                for a in active.iter().filter(|a| pat.matches(&a.path)) {
                    push(a.clone(), &mut out, &mut seen);
                }
            } else if raw.contains('-') {
                if let UuidLookup::Found(a) = store.find_by_uuid_or_prefix(raw)? {
                    push(*a, &mut out, &mut seen);
                }
            } else {
                let resolved = path::resolve_selector(scope.as_deref(), raw)?;
                if let Some(res) = store.find_by_path(&resolved)? {
                    push(res.row().clone(), &mut out, &mut seen);
                } else if path::looks_like_uuid_selector(raw) {
                    if let UuidLookup::Found(a) = store.find_by_uuid_or_prefix(raw)? {
                        push(*a, &mut out, &mut seen);
                    }
                }
            }
        }
        Ok(out)
    }

    /// The typed integration state (both-layers picture).
    pub(super) fn integration_state_typed(&self) -> crate::driver::IntegrationState {
        let raw = HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str()))
            .and_then(|b| b.integration_status_raw())
            .unwrap_or_default();
        crate::driver::IntegrationState::from_herdr_status(&raw)
    }

    /// Resolve the caller's scope + lineage. A **loop run** caller is a directory:
    /// its scope is its whole run path and it parents children *inside* it. An **agent** caller
    /// is a file: its scope is its path minus its name, and children land beside it. A plain
    /// shell has no scope.
    pub(super) fn caller_context(
        &self,
        caller_id: Option<&str>,
        caller_path: Option<&str>,
    ) -> CallerContext {
        let caller_id = caller_id.filter(|s| !s.is_empty());
        let caller_path = caller_path.filter(|s| !s.is_empty());
        // A loop-run caller: caller_id is a loop_run uuid.
        if let Some(id) = caller_id {
            let is_run = {
                let store = self.inner.store.lock().unwrap();
                store.run_by_uuid(id).ok().flatten().is_some()
            };
            if is_run {
                return CallerContext {
                    scope: caller_path.map(String::from),
                    parent_id: Some(id.to_string()),
                    parent_path: caller_path.map(String::from),
                };
            }
        }
        CallerContext {
            scope: caller_path.and_then(path::scope_of_agent),
            parent_id: caller_id.map(String::from),
            parent_path: caller_path.map(String::from),
        }
    }

    /// The caller scope for target-resolution verbs (send/kill/ls/wait/logs): full run path for
    /// a loop-run caller, else the agent's directory.
    pub(super) fn caller_scope_full(&self, params: &Value) -> Option<String> {
        self.caller_context(
            str_param(params, "caller_id").as_deref(),
            str_param(params, "caller_path").as_deref(),
        )
        .scope
    }

    /// Enforce active-loop namespace protection + the run admission barrier on a creation path.
    /// A root/unrelated context may not create anything under an
    /// active loop's name; only a context *inside* one of that loop's runs may, and only while
    /// that run is still accepting work (`running`).
    ///
    /// Reads the loop/run tables from the **already-locked** `store` passed in, so the caller
    /// can run this check and the agent insert under one contiguous store-lock hold. Because
    /// the store is the single writer (everything, incl. `loop.create`, goes through the same
    /// `Mutex<Store>`), holding the lock across check+insert makes the namespace/ownership
    /// enforcement atomic with path reservation (no TOCTOU).
    fn check_loop_namespace(
        &self,
        store: &Store,
        effective: &str,
        caller_path: Option<&str>,
    ) -> Result<()> {
        let level1 = effective.split('/').next().unwrap_or("");
        let active = store.active_loop_names().unwrap_or_default();
        if !active.iter().any(|n| n == level1) {
            return Ok(());
        }
        // Allowed only from inside a run of this loop (the caller's path is under `<loop>/…`).
        let within = caller_path
            .map(|p| p.split('/').next() == Some(level1))
            .unwrap_or(false);
        if !within {
            return Err(OrcrError::invalid_request(
                format!("`{level1}` is an active loop — create agents only inside its runs"),
                "reserved_name",
            )
            .with_details(json!({ "reason": "reserved_name", "name": level1 })));
        }
        // The run admission barrier: the caller must target a *real* run of this loop that is
        // still `running` (not stopping/ended). This also blocks a within-loop caller from
        // escaping into a phantom run subtree or the loop root itself via an absolute `--path`
        // (e.g. `/nightly/x` or `/nightly/r99999/x`): agents land under an active loop only as
        // descendants of one of its runs.
        let run_id = effective.split('/').nth(1).unwrap_or("");
        let run = store
            .find_loop_by_name(level1)
            .ok()
            .flatten()
            .and_then(|l| store.run_by_id_or_uuid(&l.uuid, run_id).ok().flatten());
        match run {
            None => Err(OrcrError::invalid_request(
                format!(
                    "`{level1}/{run_id}` is not a run of active loop `{level1}` — create agents \
                     only under one of its runs"
                ),
                "reserved_name",
            )
            .with_details(json!({ "reason": "reserved_name", "name": level1 }))),
            Some(run) if run.status != "running" => Err(OrcrError::state_conflict(format!(
                "run `{level1}/{run_id}` is {} — not accepting new agents",
                run.status
            ))
            .with_details(json!({ "current_status": run.status, "reason": "run_stopping" }))),
            Some(_) => Ok(()),
        }
    }

    /// The loop data dir for an agent's effective path, if it descends from an active loop run
    /// (`<loop>/<run_id>/…`), else `None` (`ORCR_LOOP_DATA_DIR`).
    fn loop_data_dir_for(&self, effective: &str) -> Option<String> {
        let level1 = effective.split('/').next().unwrap_or("");
        // Must be an agent nested under a loop run (≥ 2 segments), not a bare top-level agent.
        if effective.split('/').count() < 2 {
            return None;
        }
        let active = {
            let store = self.inner.store.lock().unwrap();
            store.active_loop_names().unwrap_or_default()
        };
        if active.iter().any(|n| n == level1) {
            Some(
                self.inner
                    .home
                    .data_dir()
                    .join(level1)
                    .display()
                    .to_string(),
            )
        } else {
            None
        }
    }

    /// The agent's data dir, mirroring its path: `<home>/data/<segs>/<uuid>`.
    pub(super) fn agent_data_dir(&self, path: &str, uuid: &str) -> PathBuf {
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

/// A test-only fault-injection hook (E07 regression): if `ORCR_TEST_KILL_ITER_DELAY_MS` is set,
/// sleep that many milliseconds at the top of each `agent.kill` per-agent iteration. This widens
/// the window between the kill's target snapshot and its per-agent action so the queue worker can
/// deterministically promote a queued agent and spawn its herdr pane mid-kill (the E07 race).
/// Never fires in a normal build (the env var is only set by the e2e harness).
fn kill_iter_delay_hook() {
    if let Ok(ms) = std::env::var("ORCR_TEST_KILL_ITER_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// A settled wait target. `None` from [`settle_of`] = not yet settled.
struct Settled {
    ok: bool,
    reason: String,
    next_kind: &'static str,
}

/// Map an agent's `status × exit_reason` to its wait settle outcome.
/// Returns `None` for queued/starting/working — the caller keeps waiting.
fn settle_of(a: &AgentFull) -> Option<Settled> {
    let s = match a.status.as_str() {
        "idle" | "parked" => Settled {
            ok: true,
            reason: "turn_complete".to_string(),
            next_kind: "logs_last_response",
        },
        "blocked" => {
            let kind = a.blocked_kind.as_deref().unwrap_or("unknown");
            Settled {
                ok: false,
                reason: format!("blocked:{kind}"),
                next_kind: "attach",
            }
        }
        "lost" => Settled {
            ok: false,
            reason: "lost".to_string(),
            next_kind: "none",
        },
        "ended" => {
            let er = a.exit_reason.as_deref().unwrap_or("failed");
            let (ok, reason, next) = match er {
                "completed" => (true, "completed", "logs_last_response"),
                "reaped" => (true, "reaped", "logs_history"),
                "timeout" => (false, "timeout", "none"),
                "lost" => (false, "lost", "none"),
                other => (false, other, "none"), // killed | canceled | failed
            };
            Settled {
                ok,
                reason: reason.to_string(),
                next_kind: next,
            }
        }
        _ => return None,
    };
    Some(s)
}

/// The structured `next` hint: a stable enum kind + a rendered command string.
fn next_hint(kind: &str, path: &str, uuid: &str) -> Value {
    let command = match kind {
        "logs_last_response" => format!("orcr agent logs {path} --last-response"),
        "attach" => format!("orcr agent attach {path}"),
        "logs_history" => format!("orcr agent logs {uuid}"),
        _ => String::new(),
    };
    json!({ "kind": kind, "command": command })
}

/// Build the `agent.wait` result envelope from the snapshot of target rows.
/// Unsettled targets (only possible on a timed-out wait) report `wait_timeout`.
fn wait_result(rows: &[AgentFull], decision_seq: i64, timed_out: bool) -> Value {
    let mut rows: Vec<&AgentFull> = rows.iter().collect();
    rows.sort_by(|a, b| a.path.cmp(&b.path));
    let mut targets = Vec::with_capacity(rows.len());
    let mut all_ok = true;
    for a in rows {
        let (ok, reason, next_kind) = match settle_of(a) {
            Some(s) => (s.ok, s.reason, s.next_kind),
            None => (false, "wait_timeout".to_string(), "none"),
        };
        if !ok {
            all_ok = false;
        }
        let mut row = json!({
            "uuid": a.uuid,
            "path": a.path,
            "status": a.status,
            "ok": ok,
            "reason": reason,
            "next": next_hint(next_kind, &a.path, &a.uuid),
        });
        if let Some(er) = &a.exit_reason {
            row["exit_reason"] = json!(er);
        }
        targets.push(row);
    }
    json!({
        "targets": targets,
        "all_ok": all_ok,
        "timed_out": timed_out,
        "decision_seq": decision_seq,
    })
}

/// The caller's resolved scope + lineage.
pub(super) struct CallerContext {
    pub scope: Option<String>,
    pub parent_id: Option<String>,
    pub parent_path: Option<String>,
}

/// The shortest git-style prefix (≥ 8 chars) of `u` that no *other* candidate shares.
/// UUIDv7 values created in the same millisecond share their first ~12 characters, so a fixed
/// slice would report identical, non-disambiguating prefixes; this grows the prefix until it is
/// unique (falling back to the full uuid if two are somehow indistinguishable).
fn shortest_distinct_prefix(u: &str, cands: &[String]) -> String {
    let chars: Vec<char> = u.chars().collect();
    let min = 8.min(chars.len());
    for len in min..=chars.len() {
        let pfx: String = chars[..len].iter().collect();
        if !cands.iter().any(|o| o != u && o.starts_with(&pfx)) {
            return pfx;
        }
    }
    u.to_string()
}

/// Turn a [`UuidLookup`] into a single row or the right error.
fn uuid_lookup(lookup: UuidLookup, raw: &str) -> Result<AgentFull> {
    match lookup {
        UuidLookup::Found(a) => Ok(*a),
        UuidLookup::Ambiguous(cands) => {
            // Report actually-disambiguating prefixes, not a fixed slice.
            let prefixes: Vec<String> = cands
                .iter()
                .map(|u| shortest_distinct_prefix(u, &cands))
                .collect();
            Err(
                OrcrError::not_found(format!("uuid prefix `{raw}` is ambiguous"))
                    .with_details(json!({ "target": raw, "candidates": prefixes })),
            )
        }
        UuidLookup::NotFound => Err(OrcrError::not_found_target(
            format!("no agent matched `{raw}`"),
            raw,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(status: &str, exit_reason: Option<&str>) -> AgentFull {
        AgentFull {
            uuid: "u1".into(),
            path: "review/worker".into(),
            managed: true,
            origin: "run".into(),
            parent_id: None,
            agent: Some("mock".into()),
            model: None,
            effort: None,
            gc_mode: Some("auto".into()),
            cwd: None,
            herdr_session: None,
            terminal_id: None,
            pane_id: None,
            launch_token: None,
            agent_session_kind: None,
            agent_session_value: None,
            status: status.into(),
            move_state: "none".into(),
            move_token: None,
            blocked_kind: None,
            input_seq: 1,
            cancel_requested: false,
            exit_reason: exit_reason.map(String::from),
            queue_seq: None,
            deadline_at: None,
            created_at: 0,
            starting_at: None,
            idle_since: None,
            parked_at: None,
            last_status_change_at: None,
            ended_at: None,
        }
    }

    #[test]
    fn settle_mapping_covers_the_table() {
        for s in ["queued", "starting", "working"] {
            assert!(settle_of(&agent(s, None)).is_none(), "{s} must not settle");
        }
        for s in ["idle", "parked"] {
            let st = settle_of(&agent(s, None)).unwrap();
            assert!(st.ok);
            assert_eq!(st.reason, "turn_complete");
            assert_eq!(st.next_kind, "logs_last_response");
        }
        let cases = [
            ("completed", true, "completed", "logs_last_response"),
            ("reaped", true, "reaped", "logs_history"),
            ("killed", false, "killed", "none"),
            ("canceled", false, "canceled", "none"),
            ("failed", false, "failed", "none"),
            ("timeout", false, "timeout", "none"),
            ("lost", false, "lost", "none"),
        ];
        for (er, ok, reason, next) in cases {
            let st = settle_of(&agent("ended", Some(er))).unwrap();
            assert_eq!(st.ok, ok, "exit_reason {er}");
            assert_eq!(st.reason, reason);
            assert_eq!(st.next_kind, next);
        }
        let st = settle_of(&agent("lost", None)).unwrap();
        assert!(!st.ok);
        assert_eq!(st.reason, "lost");
    }

    #[test]
    fn ambiguous_prefixes_are_actually_distinguishing() {
        // Two UUIDv7 values created in the same millisecond share their first 12 characters
        // (timestamp prefix). A fixed 12-char slice would report identical prefixes; the
        // shortest-distinct-prefix must grow past the shared run so the candidates differ.
        let a = "0197b2c3-d4e5-7a11-8000-000000000001".to_string();
        let b = "0197b2c3-d4e5-7a11-8000-000000000002".to_string();
        let cands = vec![a.clone(), b.clone()];
        let pa = shortest_distinct_prefix(&a, &cands);
        let pb = shortest_distinct_prefix(&b, &cands);
        assert_ne!(
            pa, pb,
            "reported candidate prefixes must be distinguishable"
        );
        assert!(a.starts_with(&pa) && b.starts_with(&pb));
        assert!(pa.len() >= 8 && pb.len() >= 8);
        // A prefix that is unique at 8 chars stays short.
        let c = vec![
            "abcdef01-0000-7000-8000-000000000000".to_string(),
            "12345678-0000-7000-8000-000000000000".to_string(),
        ];
        assert_eq!(shortest_distinct_prefix(&c[0], &c), "abcdef01");
    }

    #[test]
    fn blocked_reason_carries_kind() {
        let mut a = agent("blocked", None);
        a.blocked_kind = Some("question".into());
        let st = settle_of(&a).unwrap();
        assert_eq!(st.reason, "blocked:question");
        assert_eq!(st.next_kind, "attach");
        assert!(!st.ok);
        let st2 = settle_of(&agent("blocked", None)).unwrap();
        assert_eq!(st2.reason, "blocked:unknown");
    }

    #[test]
    fn next_hint_renders_commands() {
        assert_eq!(
            next_hint("logs_last_response", "a/b", "u")["command"],
            json!("orcr agent logs a/b --last-response")
        );
        assert_eq!(
            next_hint("attach", "a/b", "u")["command"],
            json!("orcr agent attach a/b")
        );
        assert_eq!(
            next_hint("logs_history", "a/b", "uuid-x")["command"],
            json!("orcr agent logs uuid-x")
        );
        assert_eq!(next_hint("none", "a/b", "u")["command"], json!(""));
    }

    #[test]
    fn wait_result_aggregates_all_ok_and_timeout() {
        let rows = vec![agent("idle", None), agent("ended", Some("completed"))];
        let r = wait_result(&rows, 42, false);
        assert_eq!(r["all_ok"], json!(true));
        assert_eq!(r["timed_out"], json!(false));
        assert_eq!(r["decision_seq"], json!(42));
        assert_eq!(r["targets"].as_array().unwrap().len(), 2);

        let rows = vec![agent("working", None)];
        let r = wait_result(&rows, 7, true);
        assert_eq!(r["all_ok"], json!(false));
        assert_eq!(r["timed_out"], json!(true));
        assert_eq!(r["targets"][0]["reason"], json!("wait_timeout"));
    }
}
