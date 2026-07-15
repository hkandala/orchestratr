//! The completion monitor: the background loop that turns herdr's raw
//! per-pane `agent_status` into orcr's verified turn completion, external-turn detection,
//! blocked tracking, and `gc immediate` teardown.
//!
//! Runtime model: one thread ticks every [`MONITOR_TICK`], polling the owned session's
//! herdr `agent.list` once per tick (cheap, one socket round-trip) and driving each
//! monitorable agent's turn state machine. All state is persisted in the `turns` table +
//! agent columns (`idle_since`), so a server restart re-derives it conservatively.

use super::Server;
use crate::driver::{
    locate_transcript, normalize_done, transcript_fresh, transcript_unavailable, tuning_for,
    AgentInfo, AgentStatus, HerdrDriver, TranscriptLocator, TuningParams,
};
use crate::error::Result;
use crate::store::{now_millis, AgentFull};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often the completion monitor polls herdr and advances turn state.
const MONITOR_TICK: Duration = Duration::from_millis(200);

impl Server {
    /// Start the background completion monitor thread.
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

    /// Advance one agent's turn state machine from the herdr-reported status.
    fn drive_completion(&self, driver: &HerdrDriver, a: &AgentFull, info: &AgentInfo) {
        // Capture a late-arriving transcript pointer (the gate for `logs`).
        if a.agent_session_value.is_none() {
            if let Some(sess) = &info.agent_session {
                let mut store = self.inner.store.lock().unwrap();
                let _ = store.record_agent_session(&a.uuid, sess.kind.as_str(), &sess.value);
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
                // Background-subagent caveat: a parked agent herdr reports `working`
                // again must be un-parked back to its home workspace so status and pane
                // location agree (work is not lost). Move it home before marking working,
                // under the per-agent move lock; the branches below then flip it to working.
                if a.status == "parked" {
                    self.unpark_on_resume(driver, a);
                }
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
                    // (typed via attach/herdr UI). Record a synthetic external turn.
                    // Fires once: the turn it opens is then the open turn on later ticks. It
                    // never fires for a no-prompt agent at startup (the mock reports `idle`
                    // there, so this branch isn't entered).
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store.open_external_turn(&a.uuid, now)
                    };
                    if let Ok(Some((seq, ev))) = ev {
                        self.publish(ev);
                        self.log().info(format!(
                            "external turn {} for {} (input_seq {seq})",
                            a.uuid, a.path
                        ));
                    }
                }
            }
            AgentStatus::Blocked => {
                // A `blocked` report on a freshly re-armed turn we haven't yet observed
                // `working` — and whose fast-turn grace window hasn't elapsed since delivery —
                // is a stale report from the *previous* turn (the provider hasn't read the new
                // input yet). Suppress it until the turn is genuinely active, so `send`
                // reliably clears a prior block. This mirrors the idle branch's guard
                // against a stale idle satisfying a newer send.
                if let Some(t) = open_turn {
                    let armed = t.working_seen_at.is_some()
                        || t.delivered_at
                            .map(|d| now.saturating_sub(d) >= tuning.fast_turn_grace_ms as i64)
                            .unwrap_or(true);
                    if !armed {
                        return;
                    }
                }
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
                    // Fast-turn grace: the turn completed so fast the monitor never observed a
                    // `working` transition. We may only conclude this once the **full** grace
                    // window has elapsed since delivery with continuous idle — otherwise a turn
                    // whose provider simply hasn't started working yet (still-stale idle from a
                    // prior turn) is falsely completed before `working` is ever seen (an
                    // old idle can never satisfy a newer send). Requiring `now - delivered >=
                    // grace` guarantees any provider that starts working within the grace window
                    // sets `working_seen_at` first, so the fast path never applies to it.
                    let fast_ok = !working_ok
                        && t.delivered_at
                            .map(|d| {
                                idle_since.saturating_sub(d) <= tuning.fast_turn_grace_ms as i64
                                    && now.saturating_sub(d) >= tuning.fast_turn_grace_ms as i64
                            })
                            .unwrap_or(false);
                    let stable = now.saturating_sub(idle_since) >= tuning.idle_stable_ms as i64;
                    if (working_ok || fast_ok) && stable && self.transcript_settled(a, &tuning) {
                        self.complete(driver, a, t.input_seq);
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

    /// Complete an open turn, then run `gc immediate` teardown if applicable.
    fn complete(&self, driver: &HerdrDriver, a: &AgentFull, input_seq: i64) {
        // Capture the transcript locator/cursor *before* any teardown so a waiting `ask`
        // (and post-kill `logs`) can read the response from the native file.
        let (locator, cursor) = self.capture(a);
        let immediate = a.gc_mode.as_deref() == Some("immediate");

        // A `gc immediate` agent is only torn down once its final response is **verified
        // readable** from the native transcript (the locator/cursor recorded below is exactly
        // what a waiting `ask`/post-kill `logs` reads after the pane dies). If the response is
        // not readable yet — the provider reported idle before flushing its transcript, or the
        // transcript isn't located — do NOT complete/kill this tick; the public status stays
        // `working` and we retry on the next tick rather than racing teardown ahead of the
        // provider's first-turn flush (known-issues #2). Non-immediate modes keep the agent
        // alive after `idle`, so a slightly-late transcript is read fine by a later `logs`.
        if immediate {
            let readable = locator
                .as_ref()
                .is_some_and(|loc| loc.last_response(&a.uuid, &a.status).is_ok());
            if !readable {
                return;
            }
        }

        let cursor_str = cursor.as_deref();
        // gc immediate goes `working → ended (completed)` with **no** transient public `idle`
        // (so a waiting `ask`/`wait` settles on `completed`, not `turn_complete`). Other
        // modes flip `working → idle`.
        let ev = {
            let mut store = self.inner.store.lock().unwrap();
            if immediate {
                store.complete_turn_row(&a.uuid, input_seq, now_millis(), cursor_str)
            } else {
                store.complete_turn(&a.uuid, input_seq, now_millis(), cursor_str)
            }
            .unwrap_or(0)
        };
        if ev == 0 {
            return; // already completed / not working — nothing to do
        }
        if let (Some(loc), Some(c)) = (&locator, &cursor) {
            let mut store = self.inner.store.lock().unwrap();
            if let Ok(cap_ev) = store.record_capture(&a.uuid, &loc.as_string(), c) {
                drop(store);
                self.publish(cap_ev);
            }
        }
        self.publish(ev);
        self.log()
            .info(format!("turn {input_seq} complete for {}", a.path));

        if immediate {
            self.graceful_shutdown(driver, a, a.pane_id.as_deref().unwrap_or_default());
            self.end_agent(&a.uuid, "completed");
            self.log()
                .info(format!("gc immediate: {} ended (completed)", a.path));
        }
    }

    /// Whether the provider transcript has settled (no writes for `transcript_settle_ms`).
    /// A settle window of 0 (the mock, and any provider without a native transcript) is
    /// permissive — the stable-idle check alone governs completion.
    ///
    /// For a provider WITH a real settle window (`transcript_settle_ms > 0`, i.e. claude/codex)
    /// an un-locatable transcript means **not settled**: the turn cannot be concluded complete
    /// on an absent transcript. This is the fix for known-issues #2 — a freshly-launched real
    /// provider reports herdr `idle` during boot before it has registered its session or written
    /// any transcript, and the old permissive behavior let the fast-turn-grace + stable-idle
    /// path complete (and, under `gc immediate`, tear down) the agent in ~2.5s — before claude
    /// ever captured a session (`no_session`) or flushed its transcript (`not_found`). Requiring
    /// the transcript to be located AND quiet for `transcript_settle_ms` gates completion on the
    /// provider having genuinely done work and stopped producing output.
    fn transcript_settled(&self, a: &AgentFull, tuning: &TuningParams) -> bool {
        if tuning.transcript_settle_ms == 0 {
            return true;
        }
        match self.agent_transcript(a) {
            Ok(loc) => match loc.mtime_ms() {
                Some(mt) => now_millis().saturating_sub(mt) >= tuning.transcript_settle_ms as i64,
                // Located but un-stat'able → permissive (the stable-idle check governs).
                None => true,
            },
            // Not locatable yet (no session captured, or no transcript file written) → the turn
            // has not settled; wait for the provider's transcript rather than complete on nothing.
            Err(_) => false,
        }
    }

    /// Best-effort capture of the transcript locator + cursor at completion (no response copy
    /// is stored; the cursor is the file's mtime marker). `(None, None)` when unavailable.
    fn capture(&self, a: &AgentFull) -> (Option<TranscriptLocator>, Option<String>) {
        match self.agent_transcript(a) {
            Ok(loc) => {
                let cursor = loc.mtime_ms().map(|m| m.to_string());
                (Some(loc), cursor)
            }
            Err(_) => (None, None),
        }
    }

    /// Read an agent's final response through the **freshness gate**: a final
    /// response is only reported once the located transcript has advanced past the cursor
    /// recorded at the observed completion. The recorded cursor is the file mtime captured
    /// when the turn completed (`turns.transcript_cursor`); the current file must be at least
    /// that fresh. We poll up to `transcript_freshness_timeout_ms` for it to reach that point;
    /// a file that never does (rotated/truncated to an older state, or vanished) →
    /// `transcript_unavailable{cause:"stale"}` rather than a not-yet-advanced/stale read.
    ///
    /// Agents with no recorded completion cursor (e.g. the mock, which has no native
    /// transcript) never reach here — `locate_transcript`/`agent_transcript` fails first.
    pub(super) fn last_response_fresh(
        &self,
        a: &AgentFull,
        loc: &TranscriptLocator,
    ) -> Result<String> {
        let provider = a.agent.as_deref().unwrap_or_default();
        let tuning = tuning_for(provider, &self.inner.config.integrations);
        let threshold = {
            let store = self.inner.store.lock().unwrap();
            store
                .latest_turn(&a.uuid)
                .ok()
                .flatten()
                .filter(|t| t.completed_at.is_some())
                .and_then(|t| t.transcript_cursor)
                .and_then(|c| c.parse::<i64>().ok())
        };
        if let Some(threshold) = threshold {
            let deadline = now_millis() + tuning.transcript_freshness_timeout_ms as i64;
            while !transcript_fresh(loc.mtime_ms(), threshold) {
                if now_millis() >= deadline {
                    return Err(transcript_unavailable(
                        &a.uuid,
                        &a.status,
                        "stale",
                        "transcript has not advanced past the recorded completion \
                         (rotated, truncated, or not yet written)",
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        loc.last_response(&a.uuid, &a.status)
    }

    /// Locate an agent's native transcript via the provider adapter. Applies the
    /// identity gate; returns `transcript_unavailable` when it can't be resolved.
    pub(super) fn agent_transcript(&self, a: &AgentFull) -> Result<TranscriptLocator> {
        let provider = a.agent.as_deref().unwrap_or_default();
        // The agent's data dir mirrors its path — used by the `mock` transcript adapter.
        let data_dir = self.agent_data_dir(&a.path, &a.uuid);
        locate_transcript(
            provider,
            a.agent_session_kind.as_deref(),
            a.agent_session_value.as_deref(),
            a.cwd.as_deref(),
            a.created_at,
            &a.uuid,
            &a.status,
            data_dir.to_str(),
        )
    }
}

/// Best-effort blocked-kind classification (question|limit|login|unknown). herdr
/// exposes no structured reason, so this is a coarse guess from any custom/title text;
/// detailed per-provider parsing is future work.
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
