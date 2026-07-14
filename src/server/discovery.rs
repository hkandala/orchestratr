//! Unmanaged agent discovery (spec §5.7, §11.5): the server polls the user's *other* herdr
//! sessions and tracks the agents it finds there — for supported providers only — as
//! read-only `unmanaged` rows. orcr never manages, queues, GCs, or (without `--force`) kills
//! them; it only mirrors herdr's reporting into the store so `ls`/`top`/`wait` see them.
//!
//! Sessions are per-socket (see the driver reference), so discovery fans out: enumerate
//! sessions via the herdr binary, and for every session that is NOT the owned one, connect to
//! its socket and read `agent.list`. Rows are keyed by (herdr session, `terminal_id`); a new
//! terminal is a new row (new uuid), and a terminal that disappears is marked `ended`.

use super::Server;
use crate::driver::{
    mock_provider_enabled, normalize_done, HerdrBinary, HerdrDriver, IntegrationState,
    MOCK_PROVIDER,
};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// How often discovery polls non-owned sessions (spec §5.7 "every few seconds").
const DISCOVERY_TICK: Duration = Duration::from_secs(3);

impl Server {
    /// Start the unmanaged-discovery poller (spec §5.7). `ORCR_DISABLE_DISCOVERY=1` suppresses
    /// it — used by tests that assert an exact event stream and must not pull in the developer's
    /// real, non-owned herdr sessions.
    pub(super) fn start_unmanaged_discovery(&self) {
        if std::env::var("ORCR_DISABLE_DISCOVERY").as_deref() == Ok("1") {
            return;
        }
        let server = self.clone();
        std::thread::spawn(move || {
            while !server.inner.shutdown.load(Ordering::SeqCst) {
                server.discovery_tick();
                std::thread::sleep(DISCOVERY_TICK);
            }
        });
    }

    fn discovery_tick(&self) {
        let bin = match HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str())) {
            Ok(b) => b,
            Err(_) => return,
        };
        let sessions = match bin.session_list() {
            Ok(s) => s,
            Err(_) => return,
        };
        let owned = &self.inner.config.herdr.session;
        // Integration state is only needed once we actually have a foreign session to scan;
        // compute it lazily (it shells out to `herdr integration status`).
        let mut state: Option<IntegrationState> = None;

        for s in sessions {
            if s.name == *owned || !s.running {
                continue;
            }
            let Some(sock) = s.socket_path.as_deref() else {
                continue;
            };
            let driver = match HerdrDriver::connect(sock) {
                Ok(d) => d,
                Err(_) => continue, // session unreachable — never free its names (§11.5)
            };
            let agents = match driver.agent_list() {
                Ok(a) => a,
                Err(_) => continue,
            };
            let st = state.get_or_insert_with(|| self.integration_state_typed());
            self.reconcile_session(&s.name, &agents, st);
        }
    }

    /// Upsert the supported-provider agents found in one non-owned session, and mark any
    /// previously-tracked terminal that has vanished as `ended` (spec §5.7).
    fn reconcile_session(
        &self,
        session: &str,
        agents: &[crate::driver::AgentInfo],
        state: &IntegrationState,
    ) {
        let mut seen: HashSet<String> = HashSet::new();
        for info in agents {
            let Some(provider) = info.agent.as_deref() else {
                continue; // no provider → not a supported agent
            };
            if !self.discovery_supported(provider, state) {
                continue; // unsupported providers are ignored entirely (§5.7)
            }
            seen.insert(info.terminal_id.clone());
            let status = normalize_done(info.agent_status).as_str();
            let sess = info.agent_session.as_ref().map(|s| {
                let kind = match s.kind {
                    crate::driver::AgentSessionRefKind::Id => "id",
                    crate::driver::AgentSessionRefKind::Path => "path",
                };
                (kind, s.value.as_str())
            });

            let existing = {
                let store = self.inner.store.lock().unwrap();
                store
                    .find_unmanaged(session, &info.terminal_id)
                    .ok()
                    .flatten()
            };
            match existing {
                Some(row) => {
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store.update_unmanaged(&row.uuid, status, &info.pane_id, sess)
                    };
                    if let Ok(ev) = ev {
                        self.publish(ev);
                    }
                }
                None => {
                    let path = self.unmanaged_path(session, &info.pane_id, &info.terminal_id);
                    let uuid = uuid::Uuid::now_v7().to_string();
                    let ev = {
                        let mut store = self.inner.store.lock().unwrap();
                        store.insert_unmanaged(
                            &uuid,
                            &path,
                            session,
                            &info.terminal_id,
                            &info.pane_id,
                            Some(provider),
                            status,
                            sess,
                        )
                    };
                    if let Ok(ev) = ev {
                        self.publish(ev);
                        self.log().info(format!(
                            "discovered unmanaged {provider} agent at {path} (session {session})"
                        ));
                    }
                }
            }
        }

        // Terminals we tracked but no longer see → ended (the pane closed, §5.7). Only runs
        // on a session we successfully read, so an outage never ends rows.
        let tracked = {
            let store = self.inner.store.lock().unwrap();
            store.active_unmanaged(session).unwrap_or_default()
        };
        for a in tracked {
            let gone = a
                .terminal_id
                .as_deref()
                .map(|t| !seen.contains(t))
                .unwrap_or(true);
            if gone {
                let ev = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.transition_status(&a.uuid, "ended", None)
                };
                if let Ok(seq) = ev {
                    self.publish(seq);
                    self.log()
                        .info(format!("unmanaged {} ended (terminal gone)", a.path));
                }
            }
        }
    }

    /// Whether a provider is tracked by discovery (spec §5.7 / §11.4: both integrations
    /// present). The test-only `mock` provider counts as supported when enabled.
    fn discovery_supported(&self, provider: &str, state: &IntegrationState) -> bool {
        if provider == MOCK_PROVIDER && mock_provider_enabled() {
            return true;
        }
        state.get(provider).map(|p| p.supported()).unwrap_or(false)
    }

    /// Build the unmanaged path `unmanaged/<session>/<pane>` (spec §5.7), slugifying each
    /// component and appending a deterministic terminal-hash suffix on a slug collision with an
    /// existing active row.
    fn unmanaged_path(&self, session: &str, pane_id: &str, terminal_id: &str) -> String {
        let sess = slug(session);
        let pane = slug(pane_id);
        let base = format!("unmanaged/{sess}/{pane}");
        let collides = {
            let store = self.inner.store.lock().unwrap();
            store.path_active(&base).unwrap_or(false)
        };
        if collides {
            format!("{base}_{}", short_hash(terminal_id))
        } else {
            base
        }
    }
}

/// Normalize a component to a legal identity segment (`[a-z0-9_]`, ≤ 64 chars, §5.1). Any
/// other character (incl. herdr's `:` in pane ids, `-`/`.` in session names) becomes `_`.
fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if out.is_empty() {
        out.push('x');
    }
    out
}

/// A short, deterministic hex hash of a terminal id (for slug de-collision, §5.7).
fn short_hash(s: &str) -> String {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:06x}", h.finish() & 0xff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_normalizes_pane_and_session() {
        assert_eq!(slug("w6:p1"), "w6_p1");
        assert_eq!(slug("main"), "main");
        assert_eq!(slug("My-Sess.1"), "my_sess_1");
        assert_eq!(slug(""), "x");
        assert_eq!(slug(&"a".repeat(80)).len(), 64);
    }

    #[test]
    fn short_hash_is_deterministic_and_segment_safe() {
        let h = short_hash("term_abc");
        assert_eq!(h, short_hash("term_abc"));
        assert!(crate::path::valid_segment(&format!("w6_p1_{h}")));
    }
}
