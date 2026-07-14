//! The completion monitor (spec §5.6, §11.2): the background loop that turns herdr's raw
//! per-pane `agent_status` into orcr's verified turn completion, external-turn detection,
//! blocked tracking, and `gc immediate` teardown.
//!
//! Runtime model: one thread ticks every [`MONITOR_TICK`], polling the owned session's
//! herdr `agent.list` once per tick (cheap, one socket round-trip) and driving each
//! monitorable agent's turn state machine. All state is persisted in the `turns` table +
//! agent columns (`idle_since`), so a server restart re-derives it conservatively.

use super::Server;
use crate::driver::{
    locate_transcript, normalize_done, tuning_for, AgentInfo, AgentStatus, HerdrDriver,
    TranscriptLocator, TuningParams,
};
use crate::error::Result;
use crate::store::{now_millis, AgentFull};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the completion monitor polls herdr and advances turn state.
const MONITOR_TICK: Duration = Duration::from_millis(200);

impl Server {
    /// Start the background completion monitor thread (spec §5.6/§11.2).
    pub(super) fn start_completion_monitor(&self) {
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                server.completion_tick();
                std::thread::sleep(MONITOR_TICK);
            }
        });
    }

    fn completion_tick(&self) {
        let agents = {
            let store = self.inner.store.lock().unwrap();
            store.monitorable_agents().unwrap_or_default()
        };
        if agents.is_empty() {
            return;
        }
        let driver = match self.owned_driver() {
            Ok(d) => d,
            Err(_) => return, // herdr transiently unreachable — try next tick
        };
        // One `agent.list` per tick, indexed by pane_id.
        let by_pane: HashMap<String, AgentInfo> = match driver.agent_list() {
            Ok(list) => list.into_iter().map(|a| (a.pane_id.clone(), a)).collect(),
            Err(_) => return,
        };
        for a in agents {
            let Some(pane) = a.pane_id.as_deref() else {
                continue;
            };
            let Some(info) = by_pane.get(pane) else {
                continue; // pane not visible this tick — reconciler/M4 owns lost handling
            };
            self.drive_completion(&driver, &a, info);
        }
    }

    /// Advance one agent's turn state machine from the herdr-reported status (§5.6).
    fn drive_completion(&self, driver: &HerdrDriver, a: &AgentFull, info: &AgentInfo) {
        // Capture a late-arriving transcript pointer (the gate for `logs`).
        if a.agent_session_value.is_none() {
            if let Some(sess) = &info.agent_session {
                let kind = match sess.kind {
                    crate::driver::AgentSessionRefKind::Id => "id",
                    crate::driver::AgentSessionRefKind::Path => "path",
                };
                let mut store = self.inner.store.lock().unwrap();
                let _ = store.record_agent_session(&a.uuid, kind, &sess.value);
            }
        }

        let status = normalize_done(info.agent_status);
        let provider = a.agent.as_deref().unwrap_or_default();
        let tuning = tuning_for(provider, &self.inner.config.integrations);
        let now = now_millis();
        let turn = {
            let store = self.inner.store.lock().unwrap();
            store.latest_turn(&a.uuid).unwrap_or(None)
        };
        let open_turn = turn.as_ref().filter(|t| t.is_open());

        match status {
            AgentStatus::Working => {
                if let Some(t) = open_turn {
                    let mut store = self.inner.store.lock().unwrap();
                    let _ = store.set_working_seen(&a.uuid, t.input_seq, now);
                    drop(store);
                    // An idle/blocked public status un-settles back to working.
                    if matches!(a.status.as_str(), "idle" | "blocked" | "parked") {
                        let mut store = self.inner.store.lock().unwrap();
                        if let Ok(ev) = store.mark_working(&a.uuid) {
                            drop(store);
                            self.publish(ev);
                        }
                    }
                } else {
                    // herdr reports `working` with no open turn — input orcr didn't deliver
                    // (typed via attach/herdr UI). Record a synthetic external turn (§5.6).
                    // Fires once: the turn it opens is then the open turn on later ticks. It
                    // never fires for a no-prompt agent at startup (the mock reports `idle`
                    // there, so this branch isn't entered).
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store.open_external_turn(&a.uuid, now)
                    };
                    if let Ok((seq, ev)) = ev {
                        self.publish(ev);
                        self.log().info(format!(
                            "external turn {} for {} (input_seq {seq})",
                            a.uuid, a.path
                        ));
                    }
                }
            }
            AgentStatus::Blocked => {
                let input_seq = open_turn.map(|t| t.input_seq).unwrap_or(a.input_seq);
                let kind = classify_blocked(info);
                let ev = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.mark_blocked(&a.uuid, input_seq, kind)
                };
                if let Ok(ev) = ev {
                    self.publish(ev);
                }
            }
            AgentStatus::Idle | AgentStatus::Done => {
                if let Some(t) = open_turn.cloned() {
                    // Start (or read) the idle streak.
                    let idle_since = match a.idle_since {
                        Some(s) => s,
                        None => {
                            let mut store = self.inner.store.lock().unwrap();
                            let _ = store.set_idle_since(&a.uuid, Some(now));
                            now
                        }
                    };
                    let working_ok = t.working_seen_at.is_some();
                    let fast_ok = !working_ok
                        && t.delivered_at
                            .map(|d| {
                                idle_since.saturating_sub(d) <= tuning.fast_turn_grace_ms as i64
                            })
                            .unwrap_or(false);
                    let stable = now.saturating_sub(idle_since) >= tuning.idle_stable_ms as i64;
                    if (working_ok || fast_ok) && stable && self.transcript_settled(a, &tuning) {
                        self.complete(driver, a, t.input_seq, &tuning);
                    }
                }
                // idle with no open turn (e.g. a no-prompt agent, or an already-completed
                // turn) → nothing to do; the public status is unchanged.
            }
            AgentStatus::Unknown => {
                // No signal to act on this tick.
            }
        }
    }

    /// Complete an open turn, then run `gc immediate` teardown if applicable (§11.2).
    fn complete(&self, driver: &HerdrDriver, a: &AgentFull, input_seq: i64, tuning: &TuningParams) {
        // Capture the transcript locator/cursor *before* any teardown so a waiting `ask`
        // (and post-kill `logs`) can read the response from the native file (§11.2).
        let (locator, cursor) = self.capture(a);
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            store
                .complete_turn(&a.uuid, input_seq, now_millis(), cursor.as_deref())
                .unwrap_or(0)
        };
        if ev == 0 {
            return; // already completed / not working — nothing to do
        }
        if let Some(loc) = &locator {
            if let Some(c) = &cursor {
                let mut store = self.inner.store.lock().unwrap();
                if let Ok(cap_ev) = store.record_capture(&a.uuid, loc, c) {
                    drop(store);
                    self.publish(cap_ev);
                }
            }
        }
        self.publish(ev);
        self.log()
            .info(format!("turn {input_seq} complete for {}", a.path));

        if a.gc_mode.as_deref() == Some("immediate") {
            let _ = tuning;
            self.graceful_shutdown(driver, a, a.pane_id.as_deref().unwrap_or_default());
            self.end_agent(&a.uuid, "completed");
            self.log()
                .info(format!("gc immediate: {} ended (completed)", a.path));
        }
    }

    /// Whether the provider transcript has settled (no writes for `transcript_settle_ms`).
    /// A settle window of 0 (mock) or an un-locatable transcript is permissive (the
    /// stable-idle check alone governs completion).
    fn transcript_settled(&self, a: &AgentFull, tuning: &TuningParams) -> bool {
        if tuning.transcript_settle_ms == 0 {
            return true;
        }
        match self.agent_transcript(a) {
            Ok(loc) => match loc.mtime_ms() {
                Some(mt) => now_millis().saturating_sub(mt) >= tuning.transcript_settle_ms as i64,
                None => true,
            },
            Err(_) => true,
        }
    }

    /// Best-effort capture of the transcript locator + cursor at completion (no response copy
    /// is stored; the cursor is the file's mtime marker). `(None, None)` when unavailable.
    fn capture(&self, a: &AgentFull) -> (Option<String>, Option<String>) {
        match self.agent_transcript(a) {
            Ok(loc) => {
                let cursor = loc.mtime_ms().map(|m| m.to_string());
                (Some(loc.as_string()), cursor)
            }
            Err(_) => (None, None),
        }
    }

    /// Locate an agent's native transcript via the provider adapter (spec §11.4). Applies the
    /// identity gate; returns `transcript_unavailable` when it can't be resolved.
    pub(super) fn agent_transcript(&self, a: &AgentFull) -> Result<TranscriptLocator> {
        let provider = a.agent.as_deref().unwrap_or_default();
        locate_transcript(
            provider,
            a.agent_session_kind.as_deref(),
            a.agent_session_value.as_deref(),
            a.cwd.as_deref(),
            a.created_at,
            &a.uuid,
            &a.status,
        )
    }
}

/// Best-effort blocked-kind classification (spec §5.6: question|limit|login|unknown). herdr
/// exposes no structured reason, so this is a coarse guess from any custom/title text;
/// detailed per-provider parsing is future work (§17).
fn classify_blocked(info: &AgentInfo) -> &'static str {
    let hay = format!(
        "{} {}",
        info.title.as_deref().unwrap_or_default(),
        info.name.as_deref().unwrap_or_default()
    )
    .to_lowercase();
    if hay.contains("login") || hay.contains("sign in") || hay.contains("auth") {
        "login"
    } else if hay.contains("limit") || hay.contains("quota") || hay.contains("usage") {
        "limit"
    } else if hay.contains('?') || hay.contains("question") || hay.contains("permission") {
        "question"
    } else {
        "unknown"
    }
}
