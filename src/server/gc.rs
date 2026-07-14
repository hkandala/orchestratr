//! The GC engine + reconciliation (spec §5.4, §11.2, §11.5): park/reap/timeout of managed
//! agents, two-phase crash-safe pane moves, un-park on `send`, attach-lease interlock, and
//! the drift-repair pass between the store and herdr reality.
//!
//! Runtime model: one thread ticks every `timings.gc_tick`. Each tick expires stale attach
//! leases, enforces explicit `--timeout` deadlines, parks idle-past-`idle_after` agents,
//! reaps parked-past-`kill_after` agents, and runs a reconciliation pass (recover half-done
//! moves, mark/resolve `lost`, and refresh the `server status` drift counts). All pane moves
//! are two-phase and CAS-versioned through the store's `move_token` exclusive lease, so a
//! crash mid-move is completed or rolled back on restart.

use super::Server;
use crate::driver::{HerdrDriver, PaneInfo, PaneMoveDestination};
use crate::error::{OrcrError, Result};
use crate::path;
use crate::store::{now_millis, AgentFull};
use std::collections::HashSet;
use std::sync::atomic::Ordering;

/// The reconciler's drift snapshot, surfaced in `server status` (spec §11.5, §13).
#[derive(Debug, Clone, Default)]
pub(super) struct DriftSnapshot {
    /// Managed agents currently `lost` (pane vanished, path reserved).
    pub lost: i64,
    /// Panes in the owned session that carry an agent but have no store row — reported,
    /// never touched (clean up via herdr).
    pub unknown_marked_panes: i64,
    /// Plain (non-agent) panes in the owned session — foreign user shells; reported, untouched.
    pub unmarked_panes: i64,
}

impl Server {
    /// Start the background GC + reconciliation engine (spec §11.2, §11.5).
    pub(super) fn start_gc_engine(&self) {
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                server.gc_tick();
                std::thread::sleep(server.inner.config.timings.gc_tick);
            }
        });
    }

    fn gc_tick(&self) {
        let now = now_millis();
        // Clean up attach leases whose heartbeat expired (§5.4).
        if let Ok(ev) = {
            let mut store = self.inner.store.lock().unwrap();
            store.expire_leases(now)
        } {
            self.publish(ev);
        }
        // Explicit `--timeout` deadlines fire in every gc mode (§5.4).
        self.timeout_sweep();
        // Park / reap need herdr; if it is transiently unreachable, skip this tick.
        if let Ok(driver) = self.owned_driver() {
            self.park_sweep(&driver);
            self.reap_sweep(&driver);
            self.periodic_reconcile(&driver);
        }
    }

    // --- timeout (spec §5.4) ---

    fn timeout_sweep(&self) {
        let due = {
            let store = self.inner.store.lock().unwrap();
            store.timed_out_agents(now_millis()).unwrap_or_default()
        };
        for a in due {
            self.log()
                .warn(format!("--timeout expired for {} — killing", a.path));
            if let (Ok(driver), Some(pane)) = (self.owned_driver(), a.pane_id.as_deref()) {
                self.graceful_shutdown(&driver, &a, pane);
            }
            self.end_agent(&a.uuid, "timeout");
        }
    }

    // --- park (spec §5.4, §11.2) ---

    fn park_sweep(&self, driver: &HerdrDriver) {
        let cutoff = now_millis() - self.inner.config.timings.idle_after.as_millis() as i64;
        let cands = {
            let store = self.inner.store.lock().unwrap();
            store.park_candidates(cutoff).unwrap_or_default()
        };
        for a in cands {
            if self.lease_fresh(&a.uuid) {
                self.log()
                    .info(format!("park deferred for {} (attached)", a.path));
                continue;
            }
            if let Err(e) = self.park_one(driver, &a) {
                self.log().warn(format!("park of {} failed: {e}", a.path));
            }
        }
    }

    /// Two-phase park: claim the move lease, move the pane to the `idle` workspace, then
    /// finish (status → parked) only if we still own the lease (spec §5.4).
    fn park_one(&self, driver: &HerdrDriver, a: &AgentFull) -> Result<()> {
        // Hold the per-agent move lock for the whole two-phase move so a concurrent `send`
        // un-park can never observe (and roll back) this move while its pane relocation is still
        // pending — status and location must always agree (spec §5.4).
        let move_lock = self.lock_move(&a.uuid);
        let _held = move_lock.lock().unwrap();
        let token = uuid::Uuid::new_v4().to_string();
        let won = {
            let mut store = self.inner.store.lock().unwrap();
            store.begin_move(&a.uuid, "idle", "parking", &token)?
        };
        if !won {
            return Ok(()); // raced (send un-parked, or another sweep) — leave it
        }
        let Some(pane) = a.pane_id.as_deref() else {
            let mut store = self.inner.store.lock().unwrap();
            store.rollback_move(&a.uuid, &token)?;
            return Ok(());
        };

        // Fault-injection hook (tests): crash before the herdr move → reconciler rolls back.
        crash_hook("before_move");

        // Ensure the idle workspace and move the pane into a fresh tab there.
        let (idle_ws, root_pane) = self.ensure_workspace(driver, "idle")?;
        let moved = driver.pane_move(
            pane,
            PaneMoveDestination::NewTab {
                workspace_id: Some(idle_ws),
                label: Some(path::herdr_name(&a.path)),
            },
        )?;
        // Close the idle workspace's root shell (if we just created it) so it holds only
        // parked agent panes — no leftover foreign-looking shell (§11.5 unmarked count).
        if let Some(root) = &root_pane {
            let _ = driver.pane_close(root);
        }

        // Fault-injection hook (tests): crash after the herdr move → reconciler completes.
        crash_hook("after_move");

        let session = self.inner.config.herdr.session.clone();
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.finish_park(
                &a.uuid,
                &token,
                &session,
                &moved.pane.terminal_id,
                &moved.pane.pane_id,
            )?
        };
        self.publish(ev);
        self.log().info(format!("parked {}", a.path));
        Ok(())
    }

    // --- reap (spec §5.4) ---

    fn reap_sweep(&self, driver: &HerdrDriver) {
        let cutoff = now_millis() - self.inner.config.timings.kill_after.as_millis() as i64;
        let cands = {
            let store = self.inner.store.lock().unwrap();
            store.reap_candidates(cutoff).unwrap_or_default()
        };
        for a in cands {
            // Fault-injection hook (tests): widen the window between candidate selection and lock
            // acquisition so a `send` racing the reap can deterministically win the move lock.
            reap_delay_hook();
            // Hold the per-agent move lock for the whole reap so a concurrent `send` un-park can
            // cancel the reap atomically before we act — a send acquires the same lock, un-parks
            // (parked → idle, pane back home, new pane_id) and delivers input; without this the
            // reap could shut down the just-un-parked live agent and orphan its home pane (§5.4).
            let move_lock = self.lock_move(&a.uuid);
            let _held = move_lock.lock().unwrap();
            // Re-read under the lock: only reap if it is still parked with no move in flight — a
            // send that won the race has already flipped it to idle (or begun a move).
            let cur = {
                let store = self.inner.store.lock().unwrap();
                store.agent_full(&a.uuid).ok().flatten()
            };
            let Some(cur) = cur else { continue };
            if cur.status != "parked" || cur.move_state != "none" {
                continue; // un-parked (or moving) — send cancelled the reap
            }
            if self.lease_fresh(&cur.uuid) {
                self.log()
                    .info(format!("reap deferred for {} (attached)", cur.path));
                continue;
            }
            if let Some(pane) = cur.pane_id.as_deref() {
                self.graceful_shutdown(driver, &cur, pane);
            }
            // Status-guarded end (CAS on status='parked'): a concurrent un-park still wins even
            // if it commits between our re-read and here.
            let ended = {
                let mut store = self.inner.store.lock().unwrap();
                store.end_if_status(&cur.uuid, "parked", "reaped")
            };
            match ended {
                Ok(Some(seq)) => {
                    self.publish(seq);
                    self.log().info(format!("reaped {}", cur.path));
                }
                Ok(None) => {} // un-parked between the re-read and the CAS — leave it
                Err(e) => self.log().warn(format!("reap of {} failed: {e}", cur.path)),
            }
        }
    }

    // --- un-park on send (spec §5.4, §11.2) ---

    /// Un-park a parked agent (or complete/roll back a move in flight) before delivering a
    /// `send` (spec §5.4). Returns the refreshed row (status `idle`, back in its home
    /// workspace). A no-op for a non-parked agent with no move in flight.
    /// The caller (`handle_agent_send`) already holds this agent's move lock (see
    /// [`Server::lock_move`]), so a GC park cannot interleave with the recovery/move below.
    pub(super) fn unpark_for_send(&self, driver: &HerdrDriver, a: &AgentFull) -> Result<AgentFull> {
        // A move in flight from a crash/GC: settle it first (by its exact token). Locked variant
        // — the move lock is already held by the send handler.
        if a.move_state != "none" {
            self.recover_one_move_locked(driver, a);
        }
        let cur = {
            let store = self.inner.store.lock().unwrap();
            store
                .agent_full(&a.uuid)?
                .ok_or_else(|| OrcrError::not_found(format!("agent {} vanished", a.uuid)))?
        };
        if cur.status != "parked" {
            return Ok(cur);
        }

        let token = uuid::Uuid::new_v4().to_string();
        let won = {
            let mut store = self.inner.store.lock().unwrap();
            store.begin_move(&cur.uuid, "parked", "unparking", &token)?
        };
        if !won {
            // Someone else is moving it; re-read and proceed with whatever it is now.
            let store = self.inner.store.lock().unwrap();
            return store
                .agent_full(&cur.uuid)?
                .ok_or_else(|| OrcrError::not_found(format!("agent {} vanished", cur.uuid)));
        }
        let pane = cur.pane_id.clone().ok_or_else(|| {
            OrcrError::state_conflict(format!("parked agent {} has no pane", cur.path))
        })?;
        // Move back to the home workspace (recreating the tab; the workspace is remade if gone).
        let home = path::home_workspace(&cur.path);
        let (home_ws, root_pane) = self.ensure_workspace(driver, &home)?;
        let moved = driver.pane_move(
            &pane,
            PaneMoveDestination::NewTab {
                workspace_id: Some(home_ws),
                label: Some(path::herdr_name(&cur.path)),
            },
        )?;
        if let Some(root) = &root_pane {
            let _ = driver.pane_close(root);
        }
        let session = self.inner.config.herdr.session.clone();
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store.finish_unpark(
                &cur.uuid,
                &token,
                &session,
                &moved.pane.terminal_id,
                &moved.pane.pane_id,
            )?
        };
        self.publish(ev);
        self.log().info(format!("un-parked {}", cur.path));
        let store = self.inner.store.lock().unwrap();
        store
            .agent_full(&cur.uuid)?
            .ok_or_else(|| OrcrError::not_found(format!("agent {} vanished", cur.uuid)))
    }

    /// Un-park an agent the completion monitor just observed go `working` again — the §5.4
    /// background-subagent caveat, which guarantees a resumed parked agent is moved back to
    /// its home workspace so work is not lost. Independent of transcript-based subagent
    /// *detection* (§17 future work): this fires only on an already-observed `working`
    /// transition. Takes the per-agent move lock (the monitor otherwise holds none), then
    /// reuses [`Server::unpark_for_send`] (which lands the agent `idle` in its home
    /// workspace); the monitor's own logic then flips it back to `working`.
    pub(super) fn unpark_on_resume(&self, driver: &HerdrDriver, a: &AgentFull) {
        let move_lock = self.lock_move(&a.uuid);
        let _held = move_lock.lock().unwrap();
        let cur = {
            let store = self.inner.store.lock().unwrap();
            store.agent_full(&a.uuid).ok().flatten()
        };
        let Some(cur) = cur else { return };
        // Nothing to do unless it is still parked (or a move is mid-flight to settle).
        if cur.status != "parked" && cur.move_state == "none" {
            return;
        }
        if let Err(e) = self.unpark_for_send(driver, &cur) {
            self.log()
                .warn(format!("un-park on resume for {} failed: {e}", cur.path));
        }
    }

    // --- reconciliation (spec §11.5) ---

    /// The periodic reconciliation pass (spec §11.5): recover half-done moves, resolve
    /// already-`lost` agents whose terminal is still gone, detect newly-vanished panes, and
    /// refresh the drift counts. `lost` detection and resolution are ordered so a freshly-lost
    /// agent is only resolved on a *following* poll (never the one that marked it).
    fn periodic_reconcile(&self, driver: &HerdrDriver) {
        let panes = match driver.pane_list(None) {
            Ok(p) => p,
            Err(_) => return,
        };
        let live_terminals: HashSet<&str> = panes.iter().map(|p| p.terminal_id.as_str()).collect();

        // 1) Resolve agents that were already lost on a prior poll and are still gone (§11.5).
        let lost = {
            let store = self.inner.store.lock().unwrap();
            store.lost_agents().unwrap_or_default()
        };
        for a in &lost {
            let gone = a
                .terminal_id
                .as_deref()
                .map(|t| !live_terminals.contains(t))
                .unwrap_or(true);
            if gone {
                self.end_agent(&a.uuid, "lost");
                self.log()
                    .warn(format!("reconcile: {} resolved to ended (lost)", a.path));
            }
        }

        // 2) Recover half-done moves (complete or roll back by token).
        let in_move = {
            let store = self.inner.store.lock().unwrap();
            store.agents_in_move().unwrap_or_default()
        };
        for a in &in_move {
            self.recover_one_move(driver, a);
        }

        // 3) Detect newly-vanished panes for running agents → lost (first miss).
        let active = {
            let store = self.inner.store.lock().unwrap();
            store.active_managed_agents().unwrap_or_default()
        };
        for a in &active {
            if a.move_state != "none" {
                continue; // a move settles location; handled above
            }
            if !matches!(a.status.as_str(), "working" | "idle" | "blocked" | "parked") {
                continue;
            }
            let Some(term) = a.terminal_id.as_deref() else {
                continue;
            };
            if !live_terminals.contains(term) {
                let ev = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.transition_status(&a.uuid, "lost", None)
                };
                if let Ok(seq) = ev {
                    self.publish(seq);
                    self.log()
                        .warn(format!("reconcile: {} marked lost", a.path));
                }
            }
        }

        // 4) Refresh the drift counts for `server status`.
        self.refresh_drift(&panes);
    }

    /// Complete or roll back one in-flight move by its exact `move_token` (spec §5.4, §11.5),
    /// taking the per-agent move lock first so it never pre-empts a *live* park/un-park still
    /// executing on another thread (that owner is completing the very same move). Used by the
    /// periodic/startup reconcilers, which do not otherwise hold the lock.
    fn recover_one_move(&self, driver: &HerdrDriver, a: &AgentFull) {
        let move_lock = self.lock_move(&a.uuid);
        let _held = move_lock.lock().unwrap();
        self.recover_one_move_locked(driver, a);
    }

    /// The body of [`recover_one_move`], assuming the caller already holds this agent's move
    /// lock. The pane's current workspace decides the outcome, so status and location always
    /// agree afterward.
    fn recover_one_move_locked(&self, driver: &HerdrDriver, a: &AgentFull) {
        let Some(token) = a.move_token.clone() else {
            return;
        };
        let term = a.terminal_id.as_deref();
        let pane = match term.and_then(|t| find_pane_by_terminal(driver, t)) {
            Some(p) => p,
            None => {
                // Pane vanished mid-move → clear the lease and mark lost.
                let mut store = self.inner.store.lock().unwrap();
                let _ = store.rollback_move(&a.uuid, &token);
                if let Ok(seq) = store.transition_status(&a.uuid, "lost", None) {
                    drop(store);
                    self.publish(seq);
                }
                self.log()
                    .warn(format!("reconcile: {} lost mid-move", a.path));
                return;
            }
        };
        let ws_label = workspace_label(driver, &pane.workspace_id);
        let session = self.inner.config.herdr.session.clone();
        let home = path::home_workspace(&a.path);
        // `changed` is true only if the store call actually completed or rolled back this move
        // (its token still matched). A no-op — e.g. a `send` un-park already settled the move and
        // cleared the token before we ran — must not inflate the `repaired` drift metric.
        let (changed, ev) = {
            let mut store = self.inner.store.lock().unwrap();
            match a.move_state.as_str() {
                "parking" => {
                    if ws_label.as_deref() == Some("idle") {
                        let seq = store
                            .finish_park(
                                &a.uuid,
                                &token,
                                &session,
                                &pane.terminal_id,
                                &pane.pane_id,
                            )
                            .unwrap_or(0);
                        (seq > 0, seq)
                    } else {
                        (store.rollback_move(&a.uuid, &token).unwrap_or(false), 0)
                    }
                }
                "unparking" => {
                    if ws_label.as_deref() == Some(home.as_str()) {
                        let seq = store
                            .finish_unpark(
                                &a.uuid,
                                &token,
                                &session,
                                &pane.terminal_id,
                                &pane.pane_id,
                            )
                            .unwrap_or(0);
                        (seq > 0, seq)
                    } else {
                        (store.rollback_move(&a.uuid, &token).unwrap_or(false), 0)
                    }
                }
                _ => (false, 0),
            }
        };
        if !changed {
            return; // token no longer ours (someone else settled the move) — not a repair
        }
        self.inner.repaired.fetch_add(1, Ordering::SeqCst);
        if ev > 0 {
            self.publish(ev);
        }
        self.log()
            .info(format!("reconcile: recovered move for {}", a.path));
    }

    /// Recover half-done moves + refresh drift at server start (called from
    /// [`Server::reconcile_on_start`]). Does NOT resolve `lost` agents — that waits for a
    /// following periodic poll (spec §11.5 "one following poll").
    pub(super) fn reconcile_moves_on_start(&self) {
        let driver = match self.owned_driver() {
            Ok(d) => d,
            Err(_) => return,
        };
        let in_move = {
            let store = self.inner.store.lock().unwrap();
            store.agents_in_move().unwrap_or_default()
        };
        for a in &in_move {
            self.recover_one_move(&driver, a);
        }
        if let Ok(panes) = driver.pane_list(None) {
            self.refresh_drift(&panes);
        }
    }

    /// Count drift panes in the owned session and cache the snapshot (spec §11.5): a pane whose
    /// terminal has no active managed store row is "unknown marked" if it carries an agent, else
    /// an "unmarked" foreign shell. Both are reported and never touched.
    fn refresh_drift(&self, panes: &[PaneInfo]) {
        let (managed_terminals, lost) = {
            let store = self.inner.store.lock().unwrap();
            let active = store.active_managed_agents().unwrap_or_default();
            let terms: HashSet<String> = active
                .iter()
                .filter_map(|a| a.terminal_id.clone())
                .collect();
            let lost = store.lost_agents().map(|v| v.len() as i64).unwrap_or(0);
            (terms, lost)
        };
        let mut unknown_marked = 0i64;
        let mut unmarked = 0i64;
        for p in panes {
            if managed_terminals.contains(&p.terminal_id) {
                continue; // a tracked managed pane
            }
            if p.agent.is_some() || p.agent_session.is_some() {
                unknown_marked += 1;
            } else {
                unmarked += 1;
            }
        }
        let mut d = self.inner.drift.lock().unwrap();
        d.lost = lost;
        d.unknown_marked_panes = unknown_marked;
        d.unmarked_panes = unmarked;
    }

    /// Whether an agent has a fresh attach lease (the GC interlock, §5.4).
    fn lease_fresh(&self, uuid: &str) -> bool {
        let store = self.inner.store.lock().unwrap();
        store.has_fresh_lease(uuid, now_millis()).unwrap_or(false)
    }
}

/// Find a pane by its (globally-unique, move-stable) terminal id.
fn find_pane_by_terminal(driver: &HerdrDriver, terminal_id: &str) -> Option<PaneInfo> {
    driver
        .pane_list(None)
        .ok()?
        .into_iter()
        .find(|p| p.terminal_id == terminal_id)
}

/// The workspace label for a workspace id (for deciding a move's completion side).
fn workspace_label(driver: &HerdrDriver, workspace_id: &str) -> Option<String> {
    driver
        .workspace_list()
        .ok()?
        .into_iter()
        .find(|w| w.workspace_id == workspace_id)
        .map(|w| w.label)
}

/// A test-only fault-injection hook: if `ORCR_TEST_PARK_CRASH == phase`, hard-exit the
/// process to simulate a crash at that point in the park pipeline. Never fires in a normal
/// build (the env var is only set by the e2e harness).
fn crash_hook(phase: &str) {
    if std::env::var("ORCR_TEST_PARK_CRASH").as_deref() == Ok(phase) {
        std::process::exit(137);
    }
}

/// A test-only fault-injection hook: if `ORCR_TEST_REAP_DELAY_MS` is set, sleep that many
/// milliseconds before a reap candidate acquires its move lock, so an e2e test can drive a
/// `send` into the reap window and assert the send-cancels-reap interlock (spec §5.4). Never
/// fires in a normal build (the env var is only set by the e2e harness).
fn reap_delay_hook() {
    if let Ok(ms) = std::env::var("ORCR_TEST_REAP_DELAY_MS") {
        if let Ok(ms) = ms.parse::<u64>() {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
}
