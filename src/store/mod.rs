//! The sqlite store (spec §12): WAL-mode, owned exclusively by the server (single
//! writer). All writes go through `BEGIN IMMEDIATE` transactions.
//!
//! M0 ships the full schema and the transaction plumbing, plus a minimal typed agent
//! data-access layer sufficient to exercise the partial unique path-reservation index.
//! Later milestones grow the DAL (queue, turns, loops, events) on top.

mod schema;

pub use schema::{SCHEMA_SQL, SCHEMA_VERSION};

use crate::error::{ErrorCode, OrcrError, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::json;
use std::path::Path;

/// The single-writer store handle.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the store at `path`, configure WAL, install the schema,
    /// and stamp/verify the schema version.
    pub fn open(path: impl AsRef<Path>) -> Result<Store> {
        let conn = Connection::open(path.as_ref()).map_err(|e| {
            OrcrError::environment(
                "store_open_failed",
                format!("cannot open store {}: {e}", path.as_ref().display()),
            )
        })?;
        Store::init(conn)
    }

    /// Open an in-memory store (tests).
    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()
            .map_err(|e| OrcrError::environment("store_open_failed", e.to_string()))?;
        Store::init(conn)
    }

    fn init(conn: Connection) -> Result<Store> {
        // WAL for concurrent readers alongside the single writer; enforce FKs off (the
        // schema uses uuids as soft references, mirroring §12's "sqlite coordinates").
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(map_sqlite)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(map_sqlite)?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(map_sqlite)?;
        conn.execute_batch(SCHEMA_SQL).map_err(map_sqlite)?;

        let mut store = Store { conn };
        store.stamp_or_check_version()?;
        Ok(store)
    }

    /// Stamp the schema version on a fresh store, or refuse to open a store written by a
    /// different schema version (spec §12/§15).
    fn stamp_or_check_version(&mut self) -> Result<()> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        match existing {
            None => {
                self.conn
                    .execute(
                        "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)",
                        [SCHEMA_VERSION.to_string()],
                    )
                    .map_err(map_sqlite)?;
                Ok(())
            }
            Some(v) => {
                let found: i64 = v.parse().unwrap_or(-1);
                if found != SCHEMA_VERSION {
                    Err(OrcrError::environment(
                        "store_version_mismatch",
                        format!(
                            "store schema version {found} does not match this orcr's \
                             version {SCHEMA_VERSION}; the store was written by a \
                             different orcr version — do not share one store across versions"
                        ),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// The stamped schema version.
    pub fn schema_version(&self) -> Result<i64> {
        let v: String = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .map_err(map_sqlite)?;
        Ok(v.parse().unwrap_or(-1))
    }

    /// Run `f` inside a `BEGIN IMMEDIATE` transaction — commit on `Ok`, roll back on
    /// `Err`. This is the single-writer write path (spec §12).
    pub fn with_immediate_tx<T>(
        &mut self,
        f: impl FnOnce(&rusqlite::Transaction) -> Result<T>,
    ) -> Result<T> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(map_sqlite)?;
        let out = f(&tx)?;
        tx.commit().map_err(map_sqlite)?;
        Ok(out)
    }

    /// Enqueue a new managed agent (spec §5.5, §11.1): allocate `queue_seq` and insert the
    /// full launch payload with status `queued`, all in **one** `BEGIN IMMEDIATE`
    /// transaction so concurrent same-path spawns can never both win. Also appends an
    /// `agent.created` event in the same transaction. Returns the allocated `queue_seq` and
    /// the event seq (0 if none).
    ///
    /// A path collision with an active agent → `state_conflict` (`reason: path_in_use`) with
    /// the occupying `{uuid, path, status}`.
    pub fn enqueue_agent(&mut self, a: &NewAgent) -> Result<(i64, i64)> {
        let a = a.clone();
        self.with_immediate_tx(|tx| {
            // Pre-check the active-path reservation so we can return the occupying row's
            // identity in details (the partial unique index is the hard guarantee).
            if let Some((uuid, status)) = tx
                .query_row(
                    "SELECT uuid, status FROM agents \
                     WHERE path = ?1 AND status NOT IN ('ended') LIMIT 1",
                    [&a.path],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?
            {
                return Err(OrcrError::state_conflict(format!(
                    "path `{}` is in use by an active agent",
                    a.path
                ))
                .with_details(json!({
                    "reason": "path_in_use",
                    "current_status": status,
                    "occupant": { "uuid": uuid, "path": a.path, "status": status },
                })));
            }

            let queue_seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(queue_seq), 0) + 1 FROM agents",
                    [],
                    |r| r.get(0),
                )
                .map_err(map_sqlite)?;

            tx.execute(
                "INSERT INTO agents (
                     uuid, path, managed, origin, parent_id, agent, model, effort,
                     gc_mode, cwd, herdr_session, terminal_id, pane_id, launch_token,
                     status, queue_seq, deadline_at, enqueued_at, created_at,
                     last_status_change_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                     ?9, ?10, ?11, ?12, ?13, ?14,
                     'queued', ?15, ?16, ?17, ?17, ?17, ?17
                 )",
                rusqlite::params![
                    a.uuid,
                    a.path,
                    a.managed as i64,
                    a.origin,
                    a.parent_id,
                    a.agent,
                    a.model,
                    a.effort,
                    a.gc_mode,
                    a.cwd,
                    a.herdr_session,
                    a.terminal_id,
                    a.pane_id,
                    a.launch_token,
                    queue_seq,
                    a.deadline_at,
                    a.created_at,
                ],
            )
            .map_err(|e| map_insert_conflict(e, &a.path))?;

            let ev = append_event_tx(
                tx,
                "agent.created",
                Some(&a.uuid),
                &json!({ "uuid": a.uuid, "path": a.path, "status": "queued", "agent": a.agent }),
            )?;
            Ok((queue_seq, ev))
        })
    }

    /// Promote queued agents to `starting` in strict `queue_seq` FIFO order, up to the
    /// available global and per-provider capacity, in one transaction (spec §5.5). A
    /// provider that is at its cap is skipped (its later siblings wait) while agents of
    /// other providers may still promote. Emits `queue.promoted` + `agent.status_changed`
    /// per promotion. Returns the promoted rows and the highest event seq written.
    pub fn promote_queued(
        &mut self,
        global_max: u32,
        per_provider: &std::collections::BTreeMap<String, u32>,
        now: i64,
    ) -> Result<(Vec<AgentFull>, i64)> {
        let per_provider = per_provider.clone();
        self.with_immediate_tx(|tx| {
            let mut global_used: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM agents \
                     WHERE managed = 1 AND status NOT IN ('queued','ended','lost')",
                    [],
                    |r| r.get(0),
                )
                .map_err(map_sqlite)?;

            // Per-provider used counts.
            let mut used: std::collections::BTreeMap<String, i64> =
                std::collections::BTreeMap::new();
            {
                let mut stmt = tx
                    .prepare(
                        "SELECT COALESCE(agent,''), COUNT(*) FROM agents \
                         WHERE managed = 1 AND status NOT IN ('queued','ended','lost') \
                         GROUP BY agent",
                    )
                    .map_err(map_sqlite)?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
                    .map_err(map_sqlite)?;
                for row in rows {
                    let (p, c) = row.map_err(map_sqlite)?;
                    used.insert(p, c);
                }
            }

            // Candidate queued rows, FIFO.
            let candidates: Vec<(String, String)> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT uuid, COALESCE(agent,'') FROM agents \
                         WHERE status = 'queued' ORDER BY queue_seq ASC",
                    )
                    .map_err(map_sqlite)?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                    .map_err(map_sqlite)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            };

            let mut promoted_uuids = Vec::new();
            let mut last_ev = 0i64;
            for (uuid, provider) in candidates {
                if global_used >= global_max as i64 {
                    break;
                }
                let cap = per_provider.get(&provider).copied().unwrap_or(global_max) as i64;
                let used_p = *used.get(&provider).unwrap_or(&0);
                if used_p >= cap {
                    continue; // provider at cap — skip, its FIFO siblings wait
                }
                tx.execute(
                    "UPDATE agents SET status='starting', starting_at=?2, \
                     last_status_change_at=?2, updated_at=?2 WHERE uuid=?1 AND status='queued'",
                    rusqlite::params![uuid, now],
                )
                .map_err(map_sqlite)?;
                append_event_tx(tx, "queue.promoted", Some(&uuid), &json!({ "uuid": uuid }))?;
                last_ev = append_event_tx(
                    tx,
                    "agent.status_changed",
                    Some(&uuid),
                    &json!({ "uuid": uuid, "status": "starting" }),
                )?;
                global_used += 1;
                *used.entry(provider).or_insert(0) += 1;
                promoted_uuids.push(uuid);
            }

            let mut out = Vec::new();
            for uuid in &promoted_uuids {
                if let Some(a) = read_agent_full_tx(tx, uuid)? {
                    out.push(a);
                }
            }
            Ok((out, last_ev))
        })
    }

    /// Read the full row for an agent by uuid.
    pub fn agent_full(&self, uuid: &str) -> Result<Option<AgentFull>> {
        self.conn
            .query_row(
                &format!("{AGENT_FULL_SELECT} WHERE uuid = ?1"),
                [uuid],
                read_agent_full_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// Resolve a bare **path** (no wildcard) to a row: the active agent at that path if any,
    /// else the most recent ended agent with that path (spec §5.1 "path-first" resolution).
    pub fn find_by_path(&self, path: &str) -> Result<Option<Resolution>> {
        if let Some(a) = self
            .conn
            .query_row(
                &format!("{AGENT_FULL_SELECT} WHERE path=?1 AND status NOT IN ('ended') LIMIT 1"),
                [path],
                read_agent_full_row,
            )
            .optional()
            .map_err(map_sqlite)?
        {
            return Ok(Some(Resolution::Active(a)));
        }
        let ended = self
            .conn
            .query_row(
                &format!(
                    "{AGENT_FULL_SELECT} WHERE path=?1 AND status='ended' \
                     ORDER BY created_at DESC LIMIT 1"
                ),
                [path],
                read_agent_full_row,
            )
            .optional()
            .map_err(map_sqlite)?;
        Ok(ended.map(Resolution::LatestEnded))
    }

    /// Resolve a uuid or unambiguous uuid prefix (≥ 8 hex, git-style, spec §5.1).
    pub fn find_by_uuid_or_prefix(&self, s: &str) -> Result<UuidLookup> {
        // Exact match first.
        if let Some(a) = self.agent_full(s)? {
            return Ok(UuidLookup::Found(Box::new(a)));
        }
        let matches: Vec<AgentFull> = {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "{AGENT_FULL_SELECT} WHERE uuid LIKE ?1 ESCAPE '\\' ORDER BY created_at DESC \
                     LIMIT 16"
                ))
                .map_err(map_sqlite)?;
            let like = format!("{}%", escape_like(s));
            let rows = stmt
                .query_map([like], read_agent_full_row)
                .map_err(map_sqlite)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(map_sqlite)?
        };
        match matches.len() {
            0 => Ok(UuidLookup::NotFound),
            1 => Ok(UuidLookup::Found(Box::new(
                matches.into_iter().next().unwrap(),
            ))),
            _ => Ok(UuidLookup::Ambiguous(
                matches.into_iter().map(|a| a.uuid).collect(),
            )),
        }
    }

    /// Record the herdr location of an agent after a spawn step (spec §11.1). Emits
    /// `agent.location_changed`. Returns the event seq.
    pub fn record_location(
        &mut self,
        uuid: &str,
        herdr_session: &str,
        terminal_id: &str,
        pane_id: &str,
    ) -> Result<i64> {
        let uuid = uuid.to_string();
        let (hs, tid, pid) = (
            herdr_session.to_string(),
            terminal_id.to_string(),
            pane_id.to_string(),
        );
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET herdr_session=?2, terminal_id=?3, pane_id=?4, updated_at=?5 \
                 WHERE uuid=?1",
                rusqlite::params![uuid, hs, tid, pid, now],
            )
            .map_err(map_sqlite)?;
            append_event_tx(
                tx,
                "agent.location_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "pane_id": pid, "terminal_id": tid }),
            )
        })
    }

    /// Capture the provider's transcript pointer once herdr reports it (spec §11.1).
    pub fn record_agent_session(&mut self, uuid: &str, kind: &str, value: &str) -> Result<()> {
        let (uuid, kind, value) = (uuid.to_string(), kind.to_string(), value.to_string());
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET agent_session_kind=?2, agent_session_value=?3, updated_at=?4 \
                 WHERE uuid=?1",
                rusqlite::params![uuid, kind, value, now],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// Transition an agent's status, appending an `agent.status_changed` (and, for `ended`,
    /// `agent.ended`) event in the same transaction. `exit_reason` and `ended_at` are set
    /// when moving to `ended`. Returns the highest event seq written.
    pub fn transition_status(
        &mut self,
        uuid: &str,
        status: &str,
        exit_reason: Option<&str>,
    ) -> Result<i64> {
        let (uuid, status) = (uuid.to_string(), status.to_string());
        let exit_reason = exit_reason.map(|s| s.to_string());
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let ended_at = if status == "ended" { Some(now) } else { None };
            let n = tx
                .execute(
                    "UPDATE agents SET status=?2, \
                     exit_reason=COALESCE(?3, exit_reason), \
                     ended_at=COALESCE(?4, ended_at), \
                     last_status_change_at=?5, updated_at=?5 WHERE uuid=?1",
                    rusqlite::params![uuid, status, exit_reason, ended_at, now],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Err(OrcrError::not_found(format!("no agent with uuid {uuid}")));
            }
            let mut ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": status, "exit_reason": exit_reason }),
            )?;
            if status == "ended" {
                ev = append_event_tx(
                    tx,
                    "agent.ended",
                    Some(&uuid),
                    &json!({ "uuid": uuid, "exit_reason": exit_reason }),
                )?;
            }
            Ok(ev)
        })
    }

    /// Status-guarded transition to `ended` (the reap interlock, spec §5.4): only ends the row
    /// if it is still at `from_status`, so a concurrent un-park (which moves the status away
    /// from `parked`) wins the race. Writes `agent.status_changed` + `agent.ended` in the same
    /// transaction. Returns the event seq if this call ended the row, or `None` if the guard
    /// failed (the row was no longer at `from_status`).
    pub fn end_if_status(
        &mut self,
        uuid: &str,
        from_status: &str,
        exit_reason: &str,
    ) -> Result<Option<i64>> {
        let (uuid, from_status, exit_reason) = (
            uuid.to_string(),
            from_status.to_string(),
            exit_reason.to_string(),
        );
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='ended', exit_reason=?3, ended_at=?4, \
                     move_state='none', move_token=NULL, \
                     last_status_change_at=?4, updated_at=?4 WHERE uuid=?1 AND status=?2",
                    rusqlite::params![uuid, from_status, exit_reason, now],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(None);
            }
            append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "ended", "exit_reason": exit_reason }),
            )?;
            let ev = append_event_tx(
                tx,
                "agent.ended",
                Some(&uuid),
                &json!({ "uuid": uuid, "exit_reason": exit_reason }),
            )?;
            Ok(Some(ev))
        })
    }

    /// Set `cancel_requested` on an agent (the §5.5 interlock, checked between pipeline
    /// steps). Returns true if the row existed.
    pub fn request_cancel(&mut self, uuid: &str) -> Result<bool> {
        let uuid = uuid.to_string();
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET cancel_requested=1, updated_at=?2 WHERE uuid=?1",
                    rusqlite::params![uuid, now],
                )
                .map_err(map_sqlite)?;
            Ok(n > 0)
        })
    }

    /// Whether cancellation has been requested for an agent.
    pub fn is_cancel_requested(&self, uuid: &str) -> Result<bool> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT cancel_requested FROM agents WHERE uuid=?1",
                [uuid],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sqlite)?;
        Ok(v.unwrap_or(0) != 0)
    }

    // --- Turn completion (spec §5.6, §12) ---

    /// The latest turn row for an agent (highest `input_seq`), or `None` if it has none.
    pub fn latest_turn(&self, uuid: &str) -> Result<Option<TurnRow>> {
        self.conn
            .query_row(
                "SELECT agent_uuid, input_seq, source, delivered_at, working_seen_at, \
                 completed_at, blocked_kind, transcript_cursor FROM turns \
                 WHERE agent_uuid=?1 ORDER BY input_seq DESC LIMIT 1",
                [uuid],
                read_turn_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// Record that `working` was observed for a turn (idempotent: only sets `working_seen_at`
    /// if still null). Also clears `idle_since` since the agent is actively working.
    pub fn set_working_seen(&mut self, uuid: &str, input_seq: i64, at: i64) -> Result<()> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE turns SET working_seen_at=?3 \
                 WHERE agent_uuid=?1 AND input_seq=?2 AND working_seen_at IS NULL",
                rusqlite::params![uuid, input_seq, at],
            )
            .map_err(map_sqlite)?;
            tx.execute(
                "UPDATE agents SET idle_since=NULL, updated_at=?2 WHERE uuid=?1",
                rusqlite::params![uuid, at],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// Set (or clear) `idle_since` — the start of the current idle streak used by the
    /// stable-idle completion check (§5.6).
    pub fn set_idle_since(&mut self, uuid: &str, at: Option<i64>) -> Result<()> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET idle_since=?2, updated_at=?3 WHERE uuid=?1",
                rusqlite::params![uuid, at, now_millis()],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// Deliver an input (spec §5.6): bump `input_seq`, open a turn row, and **re-arm** the
    /// agent to `working` (clearing `idle_since`/`blocked_kind`) so a `wait` issued after
    /// this input cannot be satisfied by a stale idle. `source` is `orcr` or `external`.
    /// Emits `agent.status_changed`. Returns `Some((input_seq, event_seq))`, or **`None`** if
    /// the row is already in a terminal state (`ended`/`lost`): the UPDATE is guarded so a
    /// concurrent `kill`/reconcile/discovery that just ended the agent can never be silently
    /// revived (ended→working) by a racing spawn/send delivery.
    pub fn deliver_input(
        &mut self,
        uuid: &str,
        source: &str,
        at: i64,
    ) -> Result<Option<(i64, i64)>> {
        let (uuid, source) = (uuid.to_string(), source.to_string());
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET input_seq = input_seq + 1, status='working', \
                     blocked_kind=NULL, idle_since=NULL, \
                     last_status_change_at=?2, updated_at=?2 \
                     WHERE uuid=?1 AND status NOT IN ('ended','lost')",
                    rusqlite::params![uuid, at],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(None);
            }
            let seq: i64 = tx
                .query_row("SELECT input_seq FROM agents WHERE uuid=?1", [&uuid], |r| {
                    r.get(0)
                })
                .map_err(map_sqlite)?;
            tx.execute(
                "INSERT INTO turns (agent_uuid, input_seq, source, delivered_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![uuid, seq, source, at],
            )
            .map_err(map_sqlite)?;
            let ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "working", "input_seq": seq }),
            )?;
            Ok(Some((seq, ev)))
        })
    }

    /// Settle a **primed, prompt-less** agent from `starting` → `idle` at the end of the spawn
    /// pipeline (§5.6), stamping the idle clock in the same transaction. Guarded on
    /// `status='starting'` so a concurrent `kill` that already ended the row (ended/lost/canceled)
    /// is not silently revived to `idle`. Returns the `agent.status_changed` event seq, or `None`
    /// if the row was no longer `starting`.
    pub fn settle_primed_idle(&mut self, uuid: &str, at: i64) -> Result<Option<i64>> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='idle', idle_since=?2, \
                     last_status_change_at=?2, updated_at=?2 \
                     WHERE uuid=?1 AND status='starting'",
                    rusqlite::params![uuid, at],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(None);
            }
            let ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "idle" }),
            )?;
            Ok(Some(ev))
        })
    }

    /// Open a synthetic **external** turn (spec §5.6): input orcr didn't deliver, observed as
    /// a `working` transition with no pending turn. Same effect as [`deliver_input`] with
    /// `source=external`, plus `working_seen_at` set (we saw the working that triggered it).
    /// Returns `None` if the row is already terminal (same guard as [`deliver_input`]).
    pub fn open_external_turn(&mut self, uuid: &str, at: i64) -> Result<Option<(i64, i64)>> {
        let Some((seq, ev)) = self.deliver_input(uuid, "external", at)? else {
            return Ok(None);
        };
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE turns SET working_seen_at=?3 WHERE agent_uuid=?1 AND input_seq=?2",
                rusqlite::params![uuid, seq, at],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })?;
        Ok(Some((seq, ev)))
    }

    /// Complete a turn (spec §5.6): mark `completed_at`, flip public status `working → idle`,
    /// and emit `agent.turn_completed` + `agent.status_changed`. No-op (returns 0) if the
    /// turn is already completed or the agent is no longer `working`. Returns the event seq.
    pub fn complete_turn(
        &mut self,
        uuid: &str,
        input_seq: i64,
        at: i64,
        cursor: Option<&str>,
    ) -> Result<i64> {
        let uuid = uuid.to_string();
        let cursor = cursor.map(|s| s.to_string());
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE turns SET completed_at=?3, transcript_cursor=COALESCE(?4, transcript_cursor) \
                     WHERE agent_uuid=?1 AND input_seq=?2 AND completed_at IS NULL",
                    rusqlite::params![uuid, input_seq, at, cursor],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            let updated = tx
                .execute(
                    "UPDATE agents SET status='idle', last_status_change_at=?2, updated_at=?2 \
                     WHERE uuid=?1 AND status='working' AND input_seq=?3",
                    rusqlite::params![uuid, at, input_seq],
                )
                .map_err(map_sqlite)?;
            if updated == 0 {
                // Turn marked complete but the public status is no longer this turn's `working`
                // (already idle/parked, OR a racing `send` bumped input_seq and opened a newer
                // turn — §5.6: an old idle can never satisfy a newer send). Record the turn but
                // emit no working→idle flip; the completion monitor re-arms the newer turn next
                // tick.
                return Ok(0);
            }
            append_event_tx(
                tx,
                "agent.turn_completed",
                Some(&uuid),
                &json!({ "uuid": uuid, "input_seq": input_seq }),
            )?;
            let ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "idle" }),
            )?;
            Ok(ev)
        })
    }

    /// Mark a turn completed **without** flipping the public status (used by `gc immediate`,
    /// which goes `working → ended (completed)` with no transient public `idle`, §11.2). Emits
    /// `agent.turn_completed`. Returns the event seq (0 if the turn was already completed).
    pub fn complete_turn_row(
        &mut self,
        uuid: &str,
        input_seq: i64,
        at: i64,
        cursor: Option<&str>,
    ) -> Result<i64> {
        let uuid = uuid.to_string();
        let cursor = cursor.map(|s| s.to_string());
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE turns SET completed_at=?3, transcript_cursor=COALESCE(?4, transcript_cursor) \
                     WHERE agent_uuid=?1 AND input_seq=?2 AND completed_at IS NULL",
                    rusqlite::params![uuid, input_seq, at, cursor],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            append_event_tx(
                tx,
                "agent.turn_completed",
                Some(&uuid),
                &json!({ "uuid": uuid, "input_seq": input_seq }),
            )
        })
    }

    /// Mark an agent `blocked` (turn-scoped, §5.6): set the public status + `blocked_kind` on
    /// both the agent and its latest turn. Returns the event seq (0 if already blocked).
    pub fn mark_blocked(&mut self, uuid: &str, input_seq: i64, kind: &str) -> Result<i64> {
        let (uuid, kind) = (uuid.to_string(), kind.to_string());
        let at = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='blocked', blocked_kind=?2, idle_since=NULL, \
                     last_status_change_at=?3, updated_at=?3 WHERE uuid=?1 AND status != 'blocked'",
                    rusqlite::params![uuid, kind, at],
                )
                .map_err(map_sqlite)?;
            tx.execute(
                "UPDATE turns SET blocked_kind=?3 WHERE agent_uuid=?1 AND input_seq=?2",
                rusqlite::params![uuid, input_seq, kind],
            )
            .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            let ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "blocked", "blocked_kind": kind }),
            )?;
            Ok(ev)
        })
    }

    /// Force the public status back to `working` (clearing `idle_since`) when the agent is
    /// observed working again (un-settles an idle/blocked). Returns event seq (0 if no change).
    pub fn mark_working(&mut self, uuid: &str) -> Result<i64> {
        let uuid = uuid.to_string();
        let at = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='working', blocked_kind=NULL, idle_since=NULL, \
                     last_status_change_at=?2, updated_at=?2 \
                     WHERE uuid=?1 AND status IN ('idle','blocked','parked')",
                    rusqlite::params![uuid, at],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            let ev = append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "working" }),
            )?;
            Ok(ev)
        })
    }

    /// Record the captured transcript locator/cursor for the final response (spec §11.2,
    /// §12). No response copy is stored. Emits `agent.response_captured`. Returns event seq.
    pub fn record_capture(&mut self, uuid: &str, locator: &str, cursor: &str) -> Result<i64> {
        let (uuid, locator, cursor) = (uuid.to_string(), locator.to_string(), cursor.to_string());
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET transcript_locator=?2, transcript_cursor=?3, updated_at=?4 \
                 WHERE uuid=?1",
                rusqlite::params![uuid, locator, cursor, now_millis()],
            )
            .map_err(map_sqlite)?;
            append_event_tx(
                tx,
                "agent.response_captured",
                Some(&uuid),
                &json!({ "uuid": uuid, "transcript_locator": locator }),
            )
        })
    }

    /// All active managed agents that have a live pane (for the completion monitor). Status in
    /// working/idle/blocked/parked with a `pane_id` recorded.
    pub fn monitorable_agents(&self) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND pane_id IS NOT NULL \
                 AND status IN ('working','idle','blocked','parked')"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// All agents matching a listing filter (spec §6.1 `ls`). Ordered by path then
    /// `created_at`. `include_ended` adds history (`--all`).
    pub fn list_agents(&self, filter: &AgentFilter) -> Result<Vec<AgentFull>> {
        let mut sql = format!("{AGENT_FULL_SELECT} WHERE 1=1");
        if !filter.include_ended {
            sql.push_str(" AND status NOT IN ('ended')");
        }
        match filter.managed {
            Some(true) => sql.push_str(" AND managed = 1"),
            Some(false) => sql.push_str(" AND managed = 0"),
            None => {}
        }
        sql.push_str(" ORDER BY path ASC, created_at ASC");
        let mut stmt = self.conn.prepare(&sql).map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        // Provider / status / pattern filters applied in Rust (glob semantics must not be
        // SQL LIKE — `_` is a legal name char, §5.1).
        let pattern = match &filter.pattern {
            Some(p) => Some(crate::path::Pattern::compile(p)?),
            None => None,
        };
        Ok(rows
            .into_iter()
            .filter(|a| {
                filter
                    .provider
                    .as_deref()
                    .map(|p| a.agent.as_deref() == Some(p))
                    .unwrap_or(true)
            })
            .filter(|a| {
                filter
                    .status
                    .as_deref()
                    .map(|s| a.status == s)
                    .unwrap_or(true)
            })
            .filter(|a| pattern.as_ref().map(|p| p.matches(&a.path)).unwrap_or(true))
            .collect())
    }

    /// The queue position (1-based rank by `queue_seq` among `queued` rows) of an agent, or
    /// `None` if it is not queued (spec §12 "derived, never stored").
    pub fn queue_position(&self, uuid: &str) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM agents b WHERE b.status='queued' \
                         AND b.queue_seq <= a.queue_seq) \
                 FROM agents a WHERE a.uuid=?1 AND a.status='queued'",
                [uuid],
                |r| r.get(0),
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// Agents stuck in `starting` past `cutoff_ms` with no pane recorded — the stuck-start
    /// guard's targets (spec §5.5). A pane recorded (`pane_id` set) counts as progress.
    pub fn stuck_starting(&self, cutoff_ms: i64) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE status='starting' AND pane_id IS NULL \
                 AND starting_at IS NOT NULL AND starting_at < ?1"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([cutoff_ms], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Conservative restart re-arm (spec §5.6, §5.4): for agents mid-turn (`working`/`blocked`)
    /// clear `idle_since` so the stable-idle completion timer re-measures from a **fresh** herdr
    /// transition, never trusting a pre-crash idle streak. For already-`idle` (turn-complete)
    /// agents, **restart the park clock** from now — otherwise a turn-complete agent that
    /// survived a restart would never become a park candidate again (its idle streak has no open
    /// turn for the monitor to re-set). `parked` agents keep their `parked_at` reap clock.
    pub fn rearm_idle_clocks_on_restart(&mut self) -> Result<()> {
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET idle_since=NULL WHERE managed=1 \
                 AND status IN ('working','blocked')",
                [],
            )
            .map_err(map_sqlite)?;
            tx.execute(
                "UPDATE agents SET idle_since=?1 WHERE managed=1 AND status='idle'",
                [now],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// All active managed agents (for reconciliation on server start, spec §11.5).
    pub fn active_managed_agents(&self) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND status NOT IN ('ended','lost')"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    // --- GC: park / reap / timeout (spec §5.4, §11.2) ---

    /// Managed `gc auto` agents that are turn-complete and have been idle since at or before
    /// `idle_cutoff`, with no move in flight — the park candidates (spec §5.4). The attach-lease
    /// guard is applied by the caller (leases live in a separate table).
    pub fn park_candidates(&self, idle_cutoff: i64) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND gc_mode='auto' AND status='idle' \
                 AND move_state='none' AND idle_since IS NOT NULL AND idle_since <= ?1"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([idle_cutoff], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Managed agents parked since at or before `kill_cutoff` — the reap candidates (§5.4).
    pub fn reap_candidates(&self, kill_cutoff: i64) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND status='parked' \
                 AND parked_at IS NOT NULL AND parked_at <= ?1"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([kill_cutoff], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Managed agents whose explicit `--timeout` deadline has passed (spec §5.4) — kill with
    /// `exit_reason: timeout`. Applies in every gc mode (there is no *default* timeout, but an
    /// explicit one is enforced even under `gc never`).
    pub fn timed_out_agents(&self, now: i64) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND deadline_at IS NOT NULL \
                 AND deadline_at <= ?1 AND status NOT IN ('ended','lost')"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([now], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Begin a two-phase pane move (spec §5.4): CAS the exclusive move lease on. Sets
    /// `move_state`/`move_token` only if the row is still at `from_status` with no move in
    /// flight. Returns true if this call won the lease.
    pub fn begin_move(
        &mut self,
        uuid: &str,
        from_status: &str,
        move_state: &str,
        token: &str,
    ) -> Result<bool> {
        let (uuid, from_status, move_state, token) = (
            uuid.to_string(),
            from_status.to_string(),
            move_state.to_string(),
            token.to_string(),
        );
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET move_state=?3, move_token=?4, updated_at=?5 \
                     WHERE uuid=?1 AND status=?2 AND move_state='none'",
                    rusqlite::params![uuid, from_status, move_state, token, now_millis()],
                )
                .map_err(map_sqlite)?;
            Ok(n > 0)
        })
    }

    /// Complete a park (spec §5.4): only if `move_token` still matches (the move we own). Sets
    /// `status='parked'`, clears the lease, stamps `parked_at`, and records the new location.
    /// Emits `agent.location_changed` + `agent.status_changed`. Returns the event seq (0 if the
    /// token no longer matches — someone else resolved the move).
    pub fn finish_park(
        &mut self,
        uuid: &str,
        token: &str,
        session: &str,
        terminal_id: &str,
        pane_id: &str,
    ) -> Result<i64> {
        let (uuid, token, session, terminal_id, pane_id) = (
            uuid.to_string(),
            token.to_string(),
            session.to_string(),
            terminal_id.to_string(),
            pane_id.to_string(),
        );
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='parked', move_state='none', move_token=NULL, \
                     parked_at=?5, herdr_session=?2, terminal_id=?3, pane_id=?4, \
                     last_status_change_at=?5, updated_at=?5 WHERE uuid=?1 AND move_token=?6",
                    rusqlite::params![uuid, session, terminal_id, pane_id, now, token],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            append_event_tx(
                tx,
                "agent.location_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "pane_id": pane_id, "terminal_id": terminal_id }),
            )?;
            append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "parked" }),
            )
        })
    }

    /// Complete an un-park (spec §5.4): only if `move_token` matches. Sets `status='idle'`,
    /// clears the lease + `parked_at`, resets the idle clock, and records the new location.
    /// Emits `agent.location_changed` + `agent.status_changed`. Returns the event seq (0 if the
    /// token no longer matches).
    pub fn finish_unpark(
        &mut self,
        uuid: &str,
        token: &str,
        session: &str,
        terminal_id: &str,
        pane_id: &str,
    ) -> Result<i64> {
        let (uuid, token, session, terminal_id, pane_id) = (
            uuid.to_string(),
            token.to_string(),
            session.to_string(),
            terminal_id.to_string(),
            pane_id.to_string(),
        );
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET status='idle', move_state='none', move_token=NULL, \
                     parked_at=NULL, idle_since=?5, herdr_session=?2, terminal_id=?3, \
                     pane_id=?4, last_status_change_at=?5, updated_at=?5 \
                     WHERE uuid=?1 AND move_token=?6",
                    rusqlite::params![uuid, session, terminal_id, pane_id, now, token],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            append_event_tx(
                tx,
                "agent.location_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "pane_id": pane_id, "terminal_id": terminal_id }),
            )?;
            append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": "idle" }),
            )
        })
    }

    /// Roll back a half-done move (spec §5.4, §11.5): clear the lease, leaving the public
    /// status where it was (idle for a failed park, parked for a failed un-park). Only affects
    /// the row if `move_token` matches. Returns true if a move was rolled back.
    pub fn rollback_move(&mut self, uuid: &str, token: &str) -> Result<bool> {
        let (uuid, token) = (uuid.to_string(), token.to_string());
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE agents SET move_state='none', move_token=NULL, updated_at=?3 \
                     WHERE uuid=?1 AND move_token=?2",
                    rusqlite::params![uuid, token, now_millis()],
                )
                .map_err(map_sqlite)?;
            Ok(n > 0)
        })
    }

    /// Managed agents with an in-flight move (`move_state != 'none'`) — half-done park/un-park
    /// moves the reconciler completes or rolls back after a crash (spec §11.5).
    pub fn agents_in_move(&self) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND move_state != 'none' \
                 AND status NOT IN ('ended','lost')"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// All managed agents currently `lost` (spec §11.5): their panes vanished and the path
    /// stays reserved until reconciliation confirms the terminal is really gone.
    pub fn lost_agents(&self) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=1 AND status='lost'"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    // --- attach leases (spec §5.4, §6.1, §12) ---

    /// Prepare an attach (spec §6.1, §11.2): in ONE transaction, validate the target is
    /// attachable, insert the lease, and read the current location — so GC can never move/reap
    /// between resolution and the lease landing. Returns the location + `attach.started` seq.
    /// Queued/starting/ended/lost targets → `state_conflict`.
    pub fn prepare_attach(
        &mut self,
        uuid: &str,
        lease_id: &str,
        mode: &str,
        connection: &str,
        client_pid: i64,
        ttl_ms: i64,
    ) -> Result<(AttachInfo, i64)> {
        let (uuid, lease_id, mode, connection) = (
            uuid.to_string(),
            lease_id.to_string(),
            mode.to_string(),
            connection.to_string(),
        );
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let row = read_agent_full_tx(tx, &uuid)?
                .ok_or_else(|| OrcrError::not_found(format!("no agent with uuid {uuid}")))?;
            if matches!(
                row.status.as_str(),
                "queued" | "starting" | "ended" | "lost"
            ) {
                return Err(OrcrError::state_conflict(format!(
                    "agent `{}` is {} — cannot attach",
                    row.path, row.status
                ))
                .with_details(json!({ "current_status": row.status })));
            }
            let terminal_id = row.terminal_id.clone().ok_or_else(|| {
                OrcrError::state_conflict(format!("agent `{}` has no live pane", row.path))
                    .with_details(json!({ "current_status": row.status }))
            })?;
            tx.execute(
                "INSERT INTO attaches \
                 (agent_uuid, lease_id, mode, connection, client_pid, started_at, \
                  heartbeat_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
                rusqlite::params![
                    uuid,
                    lease_id,
                    mode,
                    connection,
                    client_pid,
                    now,
                    now + ttl_ms
                ],
            )
            .map_err(map_sqlite)?;
            let ev = append_event_tx(
                tx,
                "attach.started",
                Some(&uuid),
                &json!({ "uuid": uuid, "lease_id": lease_id, "mode": mode }),
            )?;
            Ok((
                AttachInfo {
                    terminal_id,
                    pane_id: row.pane_id.clone(),
                    herdr_session: row.herdr_session.clone(),
                },
                ev,
            ))
        })
    }

    /// Heartbeat an attach lease (spec §5.4): refresh `heartbeat_at`/`expires_at`. Returns
    /// true if the lease still existed.
    pub fn heartbeat_lease(&mut self, lease_id: &str, ttl_ms: i64) -> Result<bool> {
        let lease_id = lease_id.to_string();
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE attaches SET heartbeat_at=?2, expires_at=?3 WHERE lease_id=?1",
                    rusqlite::params![lease_id, now, now + ttl_ms],
                )
                .map_err(map_sqlite)?;
            Ok(n > 0)
        })
    }

    /// Release an attach lease (spec §5.4): drop it and emit `attach.ended`. Returns the event
    /// seq (0 if the lease was already gone).
    pub fn release_lease(&mut self, lease_id: &str) -> Result<i64> {
        let lease_id = lease_id.to_string();
        self.with_immediate_tx(|tx| {
            let agent: Option<String> = tx
                .query_row(
                    "SELECT agent_uuid FROM attaches WHERE lease_id=?1",
                    [&lease_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_sqlite)?;
            let Some(agent) = agent else {
                return Ok(0);
            };
            tx.execute("DELETE FROM attaches WHERE lease_id=?1", [&lease_id])
                .map_err(map_sqlite)?;
            append_event_tx(
                tx,
                "attach.ended",
                Some(&agent),
                &json!({ "uuid": agent, "lease_id": lease_id }),
            )
        })
    }

    /// Whether the agent has a *fresh* attach lease (not expired) — the GC interlock that
    /// survives restarts (spec §5.4).
    pub fn has_fresh_lease(&self, uuid: &str, now: i64) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM attaches WHERE agent_uuid=?1 AND expires_at > ?2",
                rusqlite::params![uuid, now],
                |r| r.get(0),
            )
            .map_err(map_sqlite)?;
        Ok(n > 0)
    }

    /// Delete every attach lease whose heartbeat expired (spec §5.4, cleanup). Emits
    /// `attach.ended` per lease. Returns the highest event seq written (0 if none).
    pub fn expire_leases(&mut self, now: i64) -> Result<i64> {
        self.with_immediate_tx(|tx| {
            let expired: Vec<(String, String)> = {
                let mut stmt = tx
                    .prepare("SELECT lease_id, agent_uuid FROM attaches WHERE expires_at <= ?1")
                    .map_err(map_sqlite)?;
                let rows = stmt
                    .query_map([now], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .map_err(map_sqlite)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            };
            let mut last = 0;
            for (lease_id, agent) in expired {
                tx.execute("DELETE FROM attaches WHERE lease_id=?1", [&lease_id])
                    .map_err(map_sqlite)?;
                last = append_event_tx(
                    tx,
                    "attach.ended",
                    Some(&agent),
                    &json!({ "uuid": agent, "lease_id": lease_id, "reason": "expired" }),
                )?;
            }
            Ok(last)
        })
    }

    // --- unmanaged discovery (spec §5.7, §11.5) ---

    /// The active unmanaged row keyed by (herdr session, terminal_id), if any (§5.7).
    pub fn find_unmanaged(&self, session: &str, terminal_id: &str) -> Result<Option<AgentFull>> {
        self.conn
            .query_row(
                &format!(
                    "{AGENT_FULL_SELECT} WHERE managed=0 AND herdr_session=?1 \
                     AND terminal_id=?2 AND status != 'ended' LIMIT 1"
                ),
                rusqlite::params![session, terminal_id],
                read_agent_full_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// All active unmanaged rows in a session (to detect vanished terminals, §5.7).
    pub fn active_unmanaged(&self, session: &str) -> Result<Vec<AgentFull>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "{AGENT_FULL_SELECT} WHERE managed=0 AND herdr_session=?1 AND status != 'ended'"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([session], read_agent_full_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Whether any *active* agent (managed or unmanaged) already holds `path` — used to make an
    /// unmanaged path unique with a deterministic suffix (§5.7).
    pub fn path_active(&self, path: &str) -> Result<bool> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM agents WHERE path=?1 AND status != 'ended'",
                [path],
                |r| r.get(0),
            )
            .map_err(map_sqlite)?;
        Ok(n > 0)
    }

    /// Insert a discovered unmanaged agent row (spec §5.7). The path must already be made
    /// unique by the caller. Emits `agent.created`. Returns the event seq.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_unmanaged(
        &mut self,
        uuid: &str,
        path: &str,
        session: &str,
        terminal_id: &str,
        pane_id: &str,
        provider: Option<&str>,
        status: &str,
        agent_session: Option<(&str, &str)>,
    ) -> Result<i64> {
        let (uuid, path, session, terminal_id, pane_id, status) = (
            uuid.to_string(),
            path.to_string(),
            session.to_string(),
            terminal_id.to_string(),
            pane_id.to_string(),
            status.to_string(),
        );
        let provider = provider.map(|s| s.to_string());
        let (askind, asval) = match agent_session {
            Some((k, v)) => (Some(k.to_string()), Some(v.to_string())),
            None => (None, None),
        };
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "INSERT INTO agents (uuid, path, managed, origin, agent, herdr_session, \
                 terminal_id, pane_id, agent_session_kind, agent_session_value, status, \
                 created_at, last_status_change_at, updated_at) \
                 VALUES (?1, ?2, 0, 'detected', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)",
                rusqlite::params![
                    uuid,
                    path,
                    provider,
                    session,
                    terminal_id,
                    pane_id,
                    askind,
                    asval,
                    status,
                    now
                ],
            )
            .map_err(|e| map_insert_conflict(e, &path))?;
            append_event_tx(
                tx,
                "agent.created",
                Some(&uuid),
                &json!({ "uuid": uuid, "path": path, "status": status, "agent": provider,
                         "managed": false }),
            )
        })
    }

    /// Update a discovered unmanaged agent's status/location (spec §5.7). Only emits + flips
    /// when something actually changed. Returns the event seq (0 if unchanged).
    pub fn update_unmanaged(
        &mut self,
        uuid: &str,
        status: &str,
        pane_id: &str,
        agent_session: Option<(&str, &str)>,
    ) -> Result<i64> {
        let (uuid, status, pane_id) = (uuid.to_string(), status.to_string(), pane_id.to_string());
        let (askind, asval) = match agent_session {
            Some((k, v)) => (Some(k.to_string()), Some(v.to_string())),
            None => (None, None),
        };
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let cur: Option<String> = tx
                .query_row("SELECT status FROM agents WHERE uuid=?1", [&uuid], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(map_sqlite)?;
            let Some(cur) = cur else { return Ok(0) };
            // Always refresh location + late transcript pointer.
            tx.execute(
                "UPDATE agents SET pane_id=?2, \
                 agent_session_kind=COALESCE(?3, agent_session_kind), \
                 agent_session_value=COALESCE(?4, agent_session_value), updated_at=?5 \
                 WHERE uuid=?1",
                rusqlite::params![uuid, pane_id, askind, asval, now],
            )
            .map_err(map_sqlite)?;
            if cur == status {
                return Ok(0);
            }
            tx.execute(
                "UPDATE agents SET status=?2, last_status_change_at=?3, updated_at=?3 \
                 WHERE uuid=?1",
                rusqlite::params![uuid, status, now],
            )
            .map_err(map_sqlite)?;
            append_event_tx(
                tx,
                "agent.status_changed",
                Some(&uuid),
                &json!({ "uuid": uuid, "status": status }),
            )
        })
    }

    // --- Events (the subscription cursor, §11.6, §12) ---

    /// Append an event row and return its monotonic `seq`. This opens its own
    /// `BEGIN IMMEDIATE` transaction; producers that must write an event in the *same*
    /// transaction as the change they describe use [`append_event_tx`] instead.
    pub fn append_event(
        &mut self,
        kind: &str,
        ref_uuid: Option<&str>,
        payload: &serde_json::Value,
    ) -> Result<i64> {
        let kind = kind.to_string();
        let ref_uuid = ref_uuid.map(|s| s.to_string());
        let payload = payload.clone();
        self.with_immediate_tx(|tx| append_event_tx(tx, &kind, ref_uuid.as_deref(), &payload))
    }

    /// Read event rows with `seq > since_seq`, oldest first, up to `limit` rows.
    pub fn events_since(&self, since_seq: i64, limit: i64) -> Result<Vec<EventRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, ts, kind, ref_uuid, payload_json FROM events
                  WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
            )
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map(rusqlite::params![since_seq, limit], |r| {
                let payload: String = r.get(4)?;
                Ok(EventRow {
                    seq: r.get(0)?,
                    ts: r.get(1)?,
                    kind: r.get(2)?,
                    ref_uuid: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                })
            })
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Read every event whose `ref_uuid` is in `refs`, oldest first. Uses the
    /// `events(ref_uuid, seq)` index instead of a full-table scan — the targeted read `loop
    /// logs` needs to interleave one loop's + its runs' scheduler lines (spec §6.2). Note:
    /// events aged out by retention trimming are not returned (the retention-driven limit on
    /// old orcr-source lines, documented for `loop logs --source orcr`).
    pub fn events_for_refs(&self, refs: &[&str]) -> Result<Vec<EventRow>> {
        if refs.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", refs.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT seq, ts, kind, ref_uuid, payload_json FROM events \
                 WHERE ref_uuid IN ({placeholders}) ORDER BY seq ASC"
            ))
            .map_err(map_sqlite)?;
        let params = rusqlite::params_from_iter(refs.iter());
        let rows = stmt
            .query_map(params, |r| {
                let payload: String = r.get(4)?;
                Ok(EventRow {
                    seq: r.get(0)?,
                    ts: r.get(1)?,
                    kind: r.get(2)?,
                    ref_uuid: r.get(3)?,
                    payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
                })
            })
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// The highest event `seq` written so far (0 when the table is empty).
    pub fn latest_event_seq(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |r| r.get(0))
            .map_err(map_sqlite)
    }

    /// The lowest event `seq` still present (None when the table is empty).
    pub fn oldest_event_seq(&self) -> Result<Option<i64>> {
        self.conn
            .query_row("SELECT MIN(seq) FROM events", [], |r| r.get(0))
            .map_err(map_sqlite)
    }

    /// Trim the events table to at most `keep_last` most-recent rows (bounded replay
    /// retention, §11.6). Returns the lowest `seq` still retained afterward (0 if empty).
    pub fn trim_events(&mut self, keep_last: i64) -> Result<i64> {
        let keep_last = keep_last.max(0);
        self.with_immediate_tx(|tx| {
            tx.execute(
                "DELETE FROM events WHERE seq <= (
                     SELECT COALESCE(MAX(seq), 0) - ?1 FROM events
                 )",
                [keep_last],
            )
            .map_err(map_sqlite)?;
            let oldest: Option<i64> = tx
                .query_row("SELECT MIN(seq) FROM events", [], |r| r.get(0))
                .map_err(map_sqlite)?;
            Ok(oldest.unwrap_or(0))
        })
    }

    /// The fleet status counts surfaced in `server status` (§6.4): managed live/queued/blocked
    /// plus active unmanaged. Typed so `server status` doesn't reach past the DAL into raw SQL.
    pub fn status_counts(&self) -> Result<StatusCounts> {
        let count = |sql: &str| -> Result<i64> {
            self.conn
                .query_row(sql, [], |r| r.get(0))
                .map_err(map_sqlite)
        };
        Ok(StatusCounts {
            live: count(
                "SELECT COUNT(*) FROM agents WHERE managed = 1 AND status NOT IN ('ended','lost')",
            )?,
            queued: count("SELECT COUNT(*) FROM agents WHERE managed = 1 AND status = 'queued'")?,
            blocked: count("SELECT COUNT(*) FROM agents WHERE managed = 1 AND status = 'blocked'")?,
            unmanaged: count(
                "SELECT COUNT(*) FROM agents WHERE managed = 0 AND status != 'ended'",
            )?,
        })
    }

    /// Delete an agent row and its turn/attach bookkeeping (test-only, behind the debug
    /// method gate). Simulates the store being reset under a live pane — the
    /// unknown-marked-pane reconciliation drill (spec §11.5).
    pub fn debug_delete_agent(&mut self, uuid: &str) -> Result<()> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute("DELETE FROM turns WHERE agent_uuid=?1", [&uuid])
                .map_err(map_sqlite)?;
            tx.execute("DELETE FROM attaches WHERE agent_uuid=?1", [&uuid])
                .map_err(map_sqlite)?;
            tx.execute("DELETE FROM agents WHERE uuid=?1", [&uuid])
                .map_err(map_sqlite)?;
            Ok(())
        })
    }

    // --- Loops (spec §6.2, §11.3, §12) ---

    /// Create a durable loop definition (spec §6.2). The name must be unique among **active
    /// or paused** loops (a removed name is reusable — histories never collide because each
    /// definition has its own uuid). Writes a `loop.created` event in the same txn; returns
    /// the event seq.
    pub fn create_loop(&mut self, l: &NewLoop) -> Result<i64> {
        let l = l.clone();
        self.with_immediate_tx(|tx| {
            if let Some(uuid) = tx
                .query_row(
                    "SELECT uuid FROM loops WHERE name=?1 AND status IN ('active','paused') LIMIT 1",
                    [&l.name],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(map_sqlite)?
            {
                return Err(OrcrError::state_conflict(format!(
                    "loop `{}` already exists",
                    l.name
                ))
                .with_details(json!({
                    "reason": "loop_name_in_use",
                    "occupant": { "uuid": uuid, "name": l.name },
                })));
            }
            tx.execute(
                "INSERT INTO loops (
                     uuid, name, cadence_kind, cadence_value, tz, cwd, max_concurrency,
                     overlap, timeout_s, status, next_fire_at, created_at, updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'active',?10,?11,?11)",
                rusqlite::params![
                    l.uuid,
                    l.name,
                    l.cadence_kind,
                    l.cadence_value,
                    l.tz,
                    l.cwd,
                    l.max_concurrency,
                    l.overlap,
                    l.timeout_s,
                    l.next_fire_at,
                    l.created_at,
                ],
            )
            .map_err(|e| {
                // The partial unique index is the hard guarantee behind the pre-check.
                if let rusqlite::Error::SqliteFailure(f, _) = &e {
                    if f.code == rusqlite::ErrorCode::ConstraintViolation {
                        return OrcrError::state_conflict(format!(
                            "loop `{}` already exists",
                            l.name
                        ))
                        .with_details(json!({ "reason": "loop_name_in_use" }));
                    }
                }
                map_sqlite(e)
            })?;
            append_event_tx(
                tx,
                "loop.created",
                Some(&l.uuid),
                &json!({ "uuid": l.uuid, "name": l.name, "cadence": l.cadence_value }),
            )
        })
    }

    /// Resolve a loop by name: the active/paused definition first, else the most recent ended
    /// one (spec §6.2 name resolution).
    pub fn find_loop_by_name(&self, name: &str) -> Result<Option<LoopRow>> {
        if let Some(l) = self
            .conn
            .query_row(
                &format!(
                    "SELECT {LOOP_COLS} FROM loops \
                     WHERE name=?1 AND status IN ('active','paused') LIMIT 1"
                ),
                [name],
                read_loop_row,
            )
            .optional()
            .map_err(map_sqlite)?
        {
            return Ok(Some(l));
        }
        self.conn
            .query_row(
                &format!(
                    "SELECT {LOOP_COLS} FROM loops \
                     WHERE name=?1 ORDER BY created_at DESC LIMIT 1"
                ),
                [name],
                read_loop_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// A loop by its uuid (any status).
    pub fn loop_by_uuid(&self, uuid: &str) -> Result<Option<LoopRow>> {
        self.conn
            .query_row(
                &format!("SELECT {LOOP_COLS} FROM loops WHERE uuid=?1"),
                [uuid],
                read_loop_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// List loops (spec §6.2 `loop ls`). `names` filters by name (any status); `status`
    /// filters by status; without `include_ended`, only active/paused are returned.
    pub fn list_loops(
        &self,
        names: &[String],
        status: Option<&str>,
        include_ended: bool,
    ) -> Result<Vec<LoopRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {LOOP_COLS} FROM loops ORDER BY created_at ASC"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], read_loop_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows
            .into_iter()
            .filter(|l| names.is_empty() || names.iter().any(|n| n == &l.name))
            .filter(|l| status.map(|s| l.status == s).unwrap_or(true))
            .filter(|l| include_ended || status.is_some() || l.status != "ended")
            .collect())
    }

    /// The names of loops that are currently active or paused (namespace protection, §5.1).
    pub fn active_loop_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM loops WHERE status IN ('active','paused')")
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Set a loop's status (pause/resume/end), optionally recording an `ended_reason`, and
    /// emit an event. Returns the event seq.
    pub fn set_loop_status(
        &mut self,
        uuid: &str,
        status: &str,
        ended_reason: Option<&str>,
        event_kind: &str,
    ) -> Result<i64> {
        let (uuid, status, ended_reason, event_kind) = (
            uuid.to_string(),
            status.to_string(),
            ended_reason.map(|s| s.to_string()),
            event_kind.to_string(),
        );
        self.with_immediate_tx(|tx| {
            let now = now_millis();
            tx.execute(
                "UPDATE loops SET status=?2, ended_reason=COALESCE(?3, ended_reason), \
                 next_fire_at=CASE WHEN ?2='ended' THEN NULL ELSE next_fire_at END, \
                 updated_at=?4 WHERE uuid=?1",
                rusqlite::params![uuid, status, ended_reason, now],
            )
            .map_err(map_sqlite)?;
            append_event_tx(
                tx,
                &event_kind,
                Some(&uuid),
                &json!({ "uuid": uuid, "status": status, "ended_reason": ended_reason }),
            )
        })
    }

    /// Record the next scheduled fire time (UTC ms) for a loop.
    pub fn set_next_fire(&mut self, uuid: &str, next_fire_at: Option<i64>) -> Result<()> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE loops SET next_fire_at=?2, updated_at=?3 WHERE uuid=?1",
                rusqlite::params![uuid, next_fire_at, now_millis()],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// Record the last fire time.
    pub fn set_last_fire(&mut self, uuid: &str, ts: i64) -> Result<()> {
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE loops SET last_fire_at=?2, updated_at=?3 WHERE uuid=?1",
                rusqlite::params![uuid, ts, now_millis()],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })
    }

    /// Active loops whose `next_fire_at` has come due (`<= now`).
    pub fn loops_due(&self, now: i64) -> Result<Vec<LoopRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {LOOP_COLS} FROM loops \
                 WHERE status='active' AND next_fire_at IS NOT NULL AND next_fire_at <= ?1"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([now], read_loop_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// All loops (any status) — for restart recovery (spec §11.3).
    pub fn all_loops(&self) -> Result<Vec<LoopRow>> {
        self.list_loops(&[], None, true)
    }

    // --- Loop runs (spec §6.2, §11.3, §12) ---

    /// Allocate a run row transactionally (spec §11.3): every run is durable from the moment
    /// it is asked for. Scheduled fires at capacity coalesce into at most one pending
    /// scheduled run under `overlap=queue` (or drop under `skip`); manual runs always
    /// allocate. Returns the allocation outcome + the event seq written.
    pub fn allocate_run(
        &mut self,
        loop_uuid: &str,
        kind: &str,
        due_at: i64,
        max_concurrency: i64,
        overlap: &str,
        advance: Option<ScheduleAdvance>,
    ) -> Result<(RunAllocation, i64)> {
        let (loop_uuid, kind, overlap) =
            (loop_uuid.to_string(), kind.to_string(), overlap.to_string());
        self.with_immediate_tx(|tx| {
            let now = now_millis();
            // Advance the loop's schedule *inside this same txn* as the run allocation, so a
            // crash can't leave a fired occurrence without its schedule advanced — which would
            // make restart recovery log a spurious `missed_while_down` for an occurrence that
            // actually fired (spec §11.3). Applied on every scheduled-fire outcome (allocated /
            // coalesced / skipped), mirroring the pre-atomic advance_schedule call. Returns the
            // highest event seq so far (loop.ended for a `once` end, else the allocation's ev).
            let apply_advance = |tx: &rusqlite::Transaction, base_ev: i64| -> Result<i64> {
                let Some(adv) = advance.as_ref() else {
                    return Ok(base_ev);
                };
                tx.execute(
                    "UPDATE loops SET last_fire_at=?2, updated_at=?2 WHERE uuid=?1",
                    rusqlite::params![loop_uuid, now],
                )
                .map_err(map_sqlite)?;
                match adv {
                    ScheduleAdvance::EndOnce => {
                        tx.execute(
                            "UPDATE loops SET status='ended', ended_reason='fired', \
                             next_fire_at=NULL, updated_at=?2 WHERE uuid=?1",
                            rusqlite::params![loop_uuid, now],
                        )
                        .map_err(map_sqlite)?;
                        append_event_tx(
                            tx,
                            "loop.ended",
                            Some(&loop_uuid),
                            &json!({ "uuid": loop_uuid, "status": "ended", "ended_reason": "fired" }),
                        )
                    }
                    ScheduleAdvance::Next(next) => {
                        tx.execute(
                            "UPDATE loops SET next_fire_at=?2, updated_at=?3 WHERE uuid=?1",
                            rusqlite::params![loop_uuid, next, now],
                        )
                        .map_err(map_sqlite)?;
                        Ok(base_ev)
                    }
                }
            };

            let active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM loop_runs \
                     WHERE loop_uuid=?1 AND status IN ('running','stopping')",
                    [&loop_uuid],
                    |r| r.get(0),
                )
                .map_err(map_sqlite)?;
            let at_capacity = active >= max_concurrency;

            if kind == "scheduled" && at_capacity {
                if overlap == "skip" {
                    let ev = append_event_tx(
                        tx,
                        "loop.skipped",
                        Some(&loop_uuid),
                        &json!({ "loop_uuid": loop_uuid, "due_at": due_at }),
                    )?;
                    let ev = apply_advance(tx, ev)?;
                    return Ok((RunAllocation::Skipped, ev));
                }
                // overlap == queue: coalesce into the single pending scheduled run.
                if let Some(existing_uuid) = tx
                    .query_row(
                        "SELECT uuid FROM loop_runs \
                         WHERE loop_uuid=?1 AND status='pending' AND kind='scheduled' LIMIT 1",
                        [&loop_uuid],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(map_sqlite)?
                {
                    // Keep the earliest missed fire as the coalesced run's due_at.
                    tx.execute(
                        "UPDATE loop_runs SET due_at=MIN(due_at, ?2), updated_at=?3 WHERE uuid=?1",
                        rusqlite::params![existing_uuid, due_at, now],
                    )
                    .map_err(map_sqlite)?;
                    let run = read_run_row_tx(tx, &existing_uuid)?
                        .ok_or_else(|| OrcrError::server_error("loop", "coalesced run vanished"))?;
                    let ev = append_event_tx(
                        tx,
                        "loop.coalesced",
                        Some(&loop_uuid),
                        &json!({ "loop_uuid": loop_uuid, "run_id": run.run_id, "due_at": due_at }),
                    )?;
                    let ev = apply_advance(tx, ev)?;
                    return Ok((RunAllocation::Coalesced { run }, ev));
                }
            }

            // Allocate a fresh run row (unique run_id per loop; retry on the rare collision).
            // When a slot is free we reserve it atomically *inside this same txn* by inserting
            // the row already `running` (start_now); at capacity it goes in `pending`. Because
            // `BEGIN IMMEDIATE` serializes writers and `active` counts running/stopping rows,
            // no concurrent allocation/promotion can hand the same slot out twice (spec §11.3).
            let uuid = uuid::Uuid::now_v7().to_string();
            let mut run_id = new_run_id();
            for _ in 0..8 {
                let taken: bool = tx
                    .query_row(
                        "SELECT 1 FROM loop_runs WHERE loop_uuid=?1 AND run_id=?2 LIMIT 1",
                        rusqlite::params![loop_uuid, run_id],
                        |_| Ok(true),
                    )
                    .optional()
                    .map_err(map_sqlite)?
                    .unwrap_or(false);
                if !taken {
                    break;
                }
                run_id = new_run_id();
            }
            let start_now = !at_capacity;
            let status = if start_now { "running" } else { "pending" };
            tx.execute(
                "INSERT INTO loop_runs (
                     uuid, loop_uuid, run_id, kind, due_at, status, created_at, updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?7)",
                rusqlite::params![uuid, loop_uuid, run_id, kind, due_at, status, now],
            )
            .map_err(map_sqlite)?;
            let run = read_run_row_tx(tx, &uuid)?
                .ok_or_else(|| OrcrError::server_error("loop", "allocated run vanished"))?;
            // Always `loop.fired` for a fresh allocation (`pending:true` when queued); the true
            // fold path above is the only place that emits `loop.coalesced` (spec §11.3).
            let ev = append_event_tx(
                tx,
                "loop.fired",
                Some(&loop_uuid),
                &json!({
                    "loop_uuid": loop_uuid, "run_id": run.run_id, "kind": kind,
                    "due_at": due_at, "pending": !start_now,
                }),
            )?;
            let ev = apply_advance(tx, ev)?;
            Ok((RunAllocation::Allocated { run, start_now }, ev))
        })
    }

    /// Record a started run's process identity (pid/pgid + OS start time — pgid alone is not
    /// proof of identity, §11.3) and its optional timeout deadline. The slot was already
    /// reserved (`running`) by [`Store::allocate_run`] / [`Store::claim_pending_run`], so this
    /// only fills in the identity; it never re-creates the `running` state and leaves a
    /// concurrently-entered `stopping` barrier intact. Emits `loop_run.started`; returns the
    /// event seq (0 if the run is already terminal).
    pub fn record_run_start(
        &mut self,
        run_uuid: &str,
        pid: i64,
        pgid: i64,
        pgid_start_time: Option<i64>,
        started_at: i64,
        timeout_at: Option<i64>,
    ) -> Result<i64> {
        let run_uuid = run_uuid.to_string();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE loop_runs SET pid=?2, pgid=?3, pgid_start_time=?4, started_at=?5, \
                     timeout_at=?6, updated_at=?5 \
                     WHERE uuid=?1 AND status IN ('running','stopping')",
                    rusqlite::params![run_uuid, pid, pgid, pgid_start_time, started_at, timeout_at],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            let run = read_run_row_tx(tx, &run_uuid)?;
            append_event_tx(
                tx,
                "loop_run.started",
                Some(&run_uuid),
                &json!({
                    "run_uuid": run_uuid,
                    "run_id": run.as_ref().map(|r| r.run_id.clone()),
                    "pid": pid, "pgid": pgid,
                }),
            )
        })
    }

    /// Atomically reserve a free slot for the oldest pending run of a loop (spec §11.3). In one
    /// `BEGIN IMMEDIATE` transaction: count active (running/stopping) runs and, only if below
    /// `max_concurrency`, transition the oldest pending run pending→running and return it —
    /// returning `None` otherwise. Because `BEGIN IMMEDIATE` serializes writers, two concurrent
    /// callers can never claim the same slot (and thus never spawn the same run twice). The
    /// process identity is filled in later by [`Store::record_run_start`].
    pub fn claim_pending_run(
        &mut self,
        loop_uuid: &str,
        max_concurrency: i64,
    ) -> Result<Option<LoopRunRow>> {
        let loop_uuid = loop_uuid.to_string();
        self.with_immediate_tx(|tx| {
            let active: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM loop_runs \
                     WHERE loop_uuid=?1 AND status IN ('running','stopping')",
                    [&loop_uuid],
                    |r| r.get(0),
                )
                .map_err(map_sqlite)?;
            if active >= max_concurrency {
                return Ok(None);
            }
            let pending_uuid: Option<String> = tx
                .query_row(
                    "SELECT uuid FROM loop_runs \
                     WHERE loop_uuid=?1 AND status='pending' \
                     ORDER BY due_at ASC, created_at ASC LIMIT 1",
                    [&loop_uuid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map_sqlite)?;
            let Some(pending_uuid) = pending_uuid else {
                return Ok(None);
            };
            // Guarded pending→running: the LIMIT-1 select + this update run in the same
            // serialized txn, so exactly one caller flips the row.
            let n = tx
                .execute(
                    "UPDATE loop_runs SET status='running', updated_at=?2 \
                     WHERE uuid=?1 AND status='pending'",
                    rusqlite::params![pending_uuid, now_millis()],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(None);
            }
            read_run_row_tx(tx, &pending_uuid)
        })
    }

    /// Finish a run: record its terminal status + exit code/signal. Emits `loop_run.ended`.
    /// Returns the event seq.
    pub fn finish_run(
        &mut self,
        run_uuid: &str,
        status: &str,
        exit_code: Option<i64>,
        signal: Option<i64>,
    ) -> Result<i64> {
        let (run_uuid, status) = (run_uuid.to_string(), status.to_string());
        self.with_immediate_tx(|tx| {
            let now = now_millis();
            // Only finalize a run that is still running/stopping — makes concurrent finalizers
            // (the exit monitor vs the stop/timeout path) idempotent (spec §11.3).
            let n = tx
                .execute(
                    "UPDATE loop_runs SET status=?2, exit_code=?3, signal=?4, ended_at=?5, \
                     updated_at=?5 WHERE uuid=?1 AND status IN ('running','stopping')",
                    rusqlite::params![run_uuid, status, exit_code, signal, now],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            let run = read_run_row_tx(tx, &run_uuid)?;
            append_event_tx(
                tx,
                "loop_run.ended",
                Some(&run_uuid),
                &json!({
                    "run_uuid": run_uuid,
                    "run_id": run.as_ref().map(|r| r.run_id.clone()),
                    "status": status, "exit_code": exit_code, "signal": signal,
                }),
            )
        })
    }

    /// Move a running run into the `stopping` admission barrier (spec §6.2, §11.3). Emits an
    /// event; returns the seq (0 if the run was not running).
    pub fn set_run_stopping(&mut self, run_uuid: &str) -> Result<i64> {
        let run_uuid = run_uuid.to_string();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE loop_runs SET status='stopping', updated_at=?2 \
                     WHERE uuid=?1 AND status='running'",
                    rusqlite::params![run_uuid, now_millis()],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            append_event_tx(
                tx,
                "loop_run.stopping",
                Some(&run_uuid),
                &json!({ "run_uuid": run_uuid }),
            )
        })
    }

    /// Cancel a still-pending run (spec §6.2: `loop run stop` before it starts → `canceled`).
    /// Emits `loop_run.ended`; returns the seq (0 if the run was not pending).
    pub fn cancel_pending_run(&mut self, run_uuid: &str) -> Result<i64> {
        let run_uuid = run_uuid.to_string();
        self.with_immediate_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE loop_runs SET status='canceled', ended_at=?2, updated_at=?2 \
                     WHERE uuid=?1 AND status='pending'",
                    rusqlite::params![run_uuid, now_millis()],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Ok(0);
            }
            append_event_tx(
                tx,
                "loop_run.ended",
                Some(&run_uuid),
                &json!({ "run_uuid": run_uuid, "status": "canceled" }),
            )
        })
    }

    /// A run by run_id or run uuid within a loop.
    pub fn run_by_id_or_uuid(&self, loop_uuid: &str, sel: &str) -> Result<Option<LoopRunRow>> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLS} FROM loop_runs WHERE loop_uuid=?1 AND (run_id=?2 OR uuid=?2)"
                ),
                rusqlite::params![loop_uuid, sel],
                read_run_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// A run by uuid (any loop).
    pub fn run_by_uuid(&self, uuid: &str) -> Result<Option<LoopRunRow>> {
        self.conn
            .query_row(
                &format!("SELECT {RUN_COLS} FROM loop_runs WHERE uuid=?1"),
                [uuid],
                read_run_row,
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// Runs of a loop, filtered. Without `include_history`, only active + pending are returned.
    pub fn runs_for_loop(
        &self,
        loop_uuid: &str,
        status: Option<&str>,
        include_history: bool,
    ) -> Result<Vec<LoopRunRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {RUN_COLS} FROM loop_runs WHERE loop_uuid=?1 ORDER BY created_at ASC"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([loop_uuid], read_run_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows
            .into_iter()
            .filter(|r| status.map(|s| r.status == s).unwrap_or(true))
            .filter(|r| {
                include_history
                    || status.is_some()
                    || matches!(r.status.as_str(), "pending" | "running" | "stopping")
            })
            .collect())
    }

    /// The active (running/stopping) runs of a loop.
    pub fn active_runs(&self, loop_uuid: &str) -> Result<Vec<LoopRunRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {RUN_COLS} FROM loop_runs \
                 WHERE loop_uuid=?1 AND status IN ('running','stopping') ORDER BY created_at ASC"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([loop_uuid], read_run_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }

    /// Runs whose per-run timeout has expired (`timeout_at <= now`), still running.
    pub fn timed_out_runs(&self, now: i64) -> Result<Vec<LoopRunRow>> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT {RUN_COLS} FROM loop_runs \
                 WHERE status='running' AND timeout_at IS NOT NULL AND timeout_at <= ?1"
            ))
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([now], read_run_row)
            .map_err(map_sqlite)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        Ok(rows)
    }
}

/// Append an event within an existing transaction and return its `seq`. Producers use this
/// so the event lands in the **same** transaction as the change it describes (§11.6).
pub fn append_event_tx(
    tx: &rusqlite::Transaction,
    kind: &str,
    ref_uuid: Option<&str>,
    payload: &serde_json::Value,
) -> Result<i64> {
    tx.execute(
        "INSERT INTO events (ts, kind, ref_uuid, payload_json) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![now_millis(), kind, ref_uuid, payload.to_string()],
    )
    .map_err(map_sqlite)?;
    Ok(tx.last_insert_rowid())
}

/// One row from the `turns` table — the per-input completion bookkeeping (spec §5.6, §12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub agent_uuid: String,
    pub input_seq: i64,
    pub source: String,
    pub delivered_at: Option<i64>,
    pub working_seen_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub blocked_kind: Option<String>,
    pub transcript_cursor: Option<String>,
}

impl TurnRow {
    /// Whether this turn is still open (not yet completed).
    pub fn is_open(&self) -> bool {
        self.completed_at.is_none()
    }
}

fn read_turn_row(r: &rusqlite::Row) -> rusqlite::Result<TurnRow> {
    Ok(TurnRow {
        agent_uuid: r.get(0)?,
        input_seq: r.get(1)?,
        source: r.get(2)?,
        delivered_at: r.get(3)?,
        working_seen_at: r.get(4)?,
        completed_at: r.get(5)?,
        blocked_kind: r.get(6)?,
        transcript_cursor: r.get(7)?,
    })
}

/// The column list for a `loops` row read (mirrors [`read_loop_row`]).
const LOOP_COLS: &str = "uuid, name, cadence_kind, cadence_value, tz, cwd, max_concurrency, \
     overlap, timeout_s, status, next_fire_at, last_fire_at, ended_reason, created_at, updated_at";

/// The column list for a `loop_runs` row read (mirrors [`read_run_row`]).
const RUN_COLS: &str = "uuid, loop_uuid, run_id, kind, due_at, status, pid, pgid, \
     pgid_start_time, exit_code, signal, timeout_at, started_at, ended_at, created_at, updated_at";

/// The columns to insert a new loop definition (spec §12).
#[derive(Debug, Clone)]
pub struct NewLoop {
    pub uuid: String,
    pub name: String,
    pub cadence_kind: String, // cron|once
    pub cadence_value: String,
    pub tz: String,
    pub cwd: String,
    pub max_concurrency: i64,
    pub overlap: String, // queue|skip
    pub timeout_s: Option<i64>,
    pub next_fire_at: Option<i64>,
    pub created_at: i64,
}

/// A loop definition row (spec §12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRow {
    pub uuid: String,
    pub name: String,
    pub cadence_kind: String,
    pub cadence_value: String,
    pub tz: String,
    pub cwd: String,
    pub max_concurrency: i64,
    pub overlap: String,
    pub timeout_s: Option<i64>,
    pub status: String, // active|paused|ended
    pub next_fire_at: Option<i64>,
    pub last_fire_at: Option<i64>,
    pub ended_reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A loop run row (spec §12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRunRow {
    pub uuid: String,
    pub loop_uuid: String,
    pub run_id: String,
    pub kind: String, // scheduled|manual
    pub due_at: Option<i64>,
    pub status: String, // pending|running|stopping|ok|failed|timeout|stopped|canceled
    pub pid: Option<i64>,
    pub pgid: Option<i64>,
    pub pgid_start_time: Option<i64>,
    pub exit_code: Option<i64>,
    pub signal: Option<i64>,
    pub timeout_at: Option<i64>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The outcome of [`Store::allocate_run`] (spec §11.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAllocation {
    /// A fresh run row was created; `start_now` = a slot was free (else it sits pending).
    Allocated { run: LoopRunRow, start_now: bool },
    /// A scheduled fire folded into the existing pending scheduled run.
    Coalesced { run: LoopRunRow },
    /// An `overlap=skip` fire at capacity — dropped (logged).
    Skipped,
}

/// A scheduled fire's schedule advance, applied **inside** [`Store::allocate_run`]'s transaction
/// so the run insert (loop.fired) and the schedule advance commit atomically (spec §11.3). A
/// crash then either leaves nothing done (recovery skips) or leaves both done (recovery sees the
/// advanced schedule and emits no spurious `missed_while_down`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleAdvance {
    /// A `once` loop: end it after this fire (status=ended, reason "fired", loop.ended).
    EndOnce,
    /// A cron loop: set the next fire (None → no further fires; loop stays active).
    Next(Option<i64>),
}

/// A run id: `r` + 5 lowercase alphanumeric chars (spec §5.1). Uniqueness per loop is
/// enforced by the caller's retry loop against the unique index.
fn new_run_id() -> String {
    format!("r{}", crate::path::random_token())
}

fn read_loop_row(r: &rusqlite::Row) -> rusqlite::Result<LoopRow> {
    Ok(LoopRow {
        uuid: r.get(0)?,
        name: r.get(1)?,
        cadence_kind: r.get(2)?,
        cadence_value: r.get(3)?,
        tz: r.get(4)?,
        cwd: r.get(5)?,
        max_concurrency: r.get(6)?,
        overlap: r.get(7)?,
        timeout_s: r.get(8)?,
        status: r.get(9)?,
        next_fire_at: r.get(10)?,
        last_fire_at: r.get(11)?,
        ended_reason: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
    })
}

fn read_run_row(r: &rusqlite::Row) -> rusqlite::Result<LoopRunRow> {
    Ok(LoopRunRow {
        uuid: r.get(0)?,
        loop_uuid: r.get(1)?,
        run_id: r.get(2)?,
        kind: r.get(3)?,
        due_at: r.get(4)?,
        status: r.get(5)?,
        pid: r.get(6)?,
        pgid: r.get(7)?,
        pgid_start_time: r.get(8)?,
        exit_code: r.get(9)?,
        signal: r.get(10)?,
        timeout_at: r.get(11)?,
        started_at: r.get(12)?,
        ended_at: r.get(13)?,
        created_at: r.get(14)?,
        updated_at: r.get(15)?,
    })
}

fn read_run_row_tx(tx: &rusqlite::Transaction, uuid: &str) -> Result<Option<LoopRunRow>> {
    tx.query_row(
        &format!("SELECT {RUN_COLS} FROM loop_runs WHERE uuid=?1"),
        [uuid],
        read_run_row,
    )
    .optional()
    .map_err(map_sqlite)
}

/// One row from the events table.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub ts: i64,
    pub kind: String,
    pub ref_uuid: Option<String>,
    pub payload: serde_json::Value,
}

/// The managed/unmanaged fleet counts for `server status` (spec §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusCounts {
    pub live: i64,
    pub queued: i64,
    pub blocked: i64,
    pub unmanaged: i64,
}

/// The live location read under the attach-prepare transaction (spec §6.1). `terminal_id` is
/// globally unique and stable across pane moves, so the exec command addresses it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachInfo {
    pub terminal_id: String,
    pub pane_id: Option<String>,
    pub herdr_session: Option<String>,
}

/// The columns needed to insert an agent for path-reservation purposes. Later milestones
/// extend this with the full launch payload.
#[derive(Debug, Clone)]
pub struct NewAgent {
    pub uuid: String,
    pub path: String,
    pub managed: bool,
    pub origin: String, // run|detected
    pub parent_id: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub gc_mode: Option<String>,
    pub cwd: Option<String>,
    pub herdr_session: Option<String>,
    pub terminal_id: Option<String>,
    pub pane_id: Option<String>,
    pub launch_token: Option<String>,
    pub status: String,
    /// Kill deadline (`created_at + --timeout`), only when `--timeout` was passed (§5.4).
    pub deadline_at: Option<i64>,
    pub created_at: i64,
}

/// The full agent row (spec §12), the read model for the pipeline, resolution, `ls`, and
/// `api snapshot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFull {
    pub uuid: String,
    pub path: String,
    pub managed: bool,
    pub origin: String,
    pub parent_id: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub gc_mode: Option<String>,
    pub cwd: Option<String>,
    pub herdr_session: Option<String>,
    pub terminal_id: Option<String>,
    pub pane_id: Option<String>,
    pub launch_token: Option<String>,
    pub agent_session_kind: Option<String>,
    pub agent_session_value: Option<String>,
    pub status: String,
    /// Exclusive move lease state (`none|parking|unparking`, §5.4).
    pub move_state: String,
    pub move_token: Option<String>,
    pub blocked_kind: Option<String>,
    pub input_seq: i64,
    pub cancel_requested: bool,
    pub exit_reason: Option<String>,
    pub queue_seq: Option<i64>,
    pub deadline_at: Option<i64>,
    pub created_at: i64,
    pub starting_at: Option<i64>,
    pub idle_since: Option<i64>,
    /// When the agent entered `parked` (basis for the reap clock, §5.4).
    pub parked_at: Option<i64>,
    pub last_status_change_at: Option<i64>,
    pub ended_at: Option<i64>,
}

/// The path-resolution outcome (spec §5.1): active agents resolve to `Active`, otherwise
/// the most recent ended agent with that path resolves to `LatestEnded`.
#[derive(Debug, Clone)]
pub enum Resolution {
    Active(AgentFull),
    LatestEnded(AgentFull),
}

impl Resolution {
    pub fn row(&self) -> &AgentFull {
        match self {
            Resolution::Active(a) | Resolution::LatestEnded(a) => a,
        }
    }
    pub fn tag(&self) -> &'static str {
        match self {
            Resolution::Active(_) => "active",
            Resolution::LatestEnded(_) => "latest_ended",
        }
    }
}

/// The uuid / uuid-prefix lookup outcome (spec §5.1).
#[derive(Debug, Clone)]
pub enum UuidLookup {
    Found(Box<AgentFull>),
    /// Ambiguous prefix — the candidate uuids (caller can suggest disambiguating prefixes).
    Ambiguous(Vec<String>),
    NotFound,
}

/// A listing filter for `agent ls` (spec §6.1).
#[derive(Debug, Clone, Default)]
pub struct AgentFilter {
    pub pattern: Option<String>,
    pub provider: Option<String>,
    pub status: Option<String>,
    pub managed: Option<bool>,
    pub include_ended: bool,
}

/// `SELECT <cols> FROM agents` (column order matches [`read_agent_full_row`]) — append a
/// `WHERE`/`ORDER BY` clause.
const AGENT_FULL_SELECT: &str =
    "SELECT uuid, path, managed, origin, parent_id, agent, model, effort, \
     gc_mode, cwd, herdr_session, terminal_id, pane_id, launch_token, \
     agent_session_kind, agent_session_value, status, move_state, move_token, \
     blocked_kind, input_seq, \
     cancel_requested, exit_reason, queue_seq, deadline_at, created_at, starting_at, \
     idle_since, parked_at, last_status_change_at, ended_at FROM agents";

/// Deserialize an `AgentFull` from a row selecting [`AGENT_FULL_SELECT`]'s columns in order.
fn read_agent_full_row(r: &rusqlite::Row) -> rusqlite::Result<AgentFull> {
    Ok(AgentFull {
        uuid: r.get(0)?,
        path: r.get(1)?,
        managed: r.get::<_, i64>(2)? != 0,
        origin: r.get(3)?,
        parent_id: r.get(4)?,
        agent: r.get(5)?,
        model: r.get(6)?,
        effort: r.get(7)?,
        gc_mode: r.get(8)?,
        cwd: r.get(9)?,
        herdr_session: r.get(10)?,
        terminal_id: r.get(11)?,
        pane_id: r.get(12)?,
        launch_token: r.get(13)?,
        agent_session_kind: r.get(14)?,
        agent_session_value: r.get(15)?,
        status: r.get(16)?,
        move_state: r.get(17)?,
        move_token: r.get(18)?,
        blocked_kind: r.get(19)?,
        input_seq: r.get(20)?,
        cancel_requested: r.get::<_, i64>(21)? != 0,
        exit_reason: r.get(22)?,
        queue_seq: r.get(23)?,
        deadline_at: r.get(24)?,
        created_at: r.get(25)?,
        starting_at: r.get(26)?,
        idle_since: r.get(27)?,
        parked_at: r.get(28)?,
        last_status_change_at: r.get(29)?,
        ended_at: r.get(30)?,
    })
}

/// Read one `AgentFull` inside a transaction (used by promotion).
fn read_agent_full_tx(tx: &rusqlite::Transaction, uuid: &str) -> Result<Option<AgentFull>> {
    tx.query_row(
        &format!("{AGENT_FULL_SELECT} WHERE uuid = ?1"),
        [uuid],
        read_agent_full_row,
    )
    .optional()
    .map_err(map_sqlite)
}

/// Escape SQL LIKE metacharacters in a uuid-prefix literal (only `%`/`_`/`\`).
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl NewAgent {
    /// A managed, queued agent with the given uuid/path (test/convenience constructor).
    pub fn queued(uuid: impl Into<String>, path: impl Into<String>, provider: &str) -> NewAgent {
        NewAgent {
            uuid: uuid.into(),
            path: path.into(),
            managed: true,
            origin: "run".to_string(),
            parent_id: None,
            agent: Some(provider.to_string()),
            model: None,
            effort: None,
            gc_mode: Some("auto".to_string()),
            cwd: None,
            herdr_session: None,
            terminal_id: None,
            pane_id: None,
            launch_token: None,
            status: "queued".to_string(),
            deadline_at: None,
            created_at: now_millis(),
        }
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn map_sqlite(e: rusqlite::Error) -> OrcrError {
    OrcrError::new(ErrorCode::ServerError, format!("store error: {e}"))
        .with_details(json!({ "cause": "sqlite" }))
}

/// Map a UNIQUE-constraint violation on the active-path index to `state_conflict`.
fn map_insert_conflict(e: rusqlite::Error, path: &str) -> OrcrError {
    if let rusqlite::Error::SqliteFailure(err, _) = &e {
        if err.code == rusqlite::ErrorCode::ConstraintViolation {
            return OrcrError::state_conflict(format!(
                "path `{path}` is in use by an active agent"
            ))
            .with_details(json!({ "reason": "path_in_use", "path": path }));
        }
    }
    map_sqlite(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_idempotent_and_stamps_version() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("orcr.db");
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        }
        // reopen — no error, same version
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn version_mismatch_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("orcr.db");
        {
            let _s = Store::open(&path).unwrap();
        }
        // Corrupt the stamped version to simulate a store from another orcr version.
        {
            let c = Connection::open(&path).unwrap();
            c.execute(
                "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                [],
            )
            .unwrap();
        }
        let e = match Store::open(&path) {
            Ok(_) => panic!("expected version mismatch to be refused"),
            Err(e) => e,
        };
        assert_eq!(e.details["cause"], "store_version_mismatch");
    }

    fn agent_count(s: &Store) -> usize {
        s.list_agents(&AgentFilter {
            include_ended: true,
            ..Default::default()
        })
        .unwrap()
        .len()
    }

    #[test]
    fn partial_unique_index_reserves_active_paths() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "review/worker", "claude"))
            .unwrap();
        // Active duplicate path fails.
        let e = s
            .enqueue_agent(&NewAgent::queued("u2", "review/worker", "claude"))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::StateConflict);
        assert_eq!(e.details["reason"], "path_in_use");

        // End the first, then the same path is reusable.
        s.transition_status("u1", "ended", Some("completed"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("u3", "review/worker", "claude"))
            .unwrap();
        // u2's insert failed and rolled back, so only u1 (ended) + u3 (active) exist.
        assert_eq!(agent_count(&s), 2);
    }

    #[test]
    fn two_ended_rows_same_path_allowed() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("a", "p/x", "claude"))
            .unwrap();
        s.transition_status("a", "ended", Some("completed"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("b", "p/x", "claude"))
            .unwrap();
        s.transition_status("b", "ended", Some("completed"))
            .unwrap();
        // both ended, same path — fine
        assert_eq!(agent_count(&s), 2);
    }

    #[test]
    fn tx_rolls_back_on_error() {
        let mut s = Store::open_in_memory().unwrap();
        let r: Result<()> = s.with_immediate_tx(|tx| {
            tx.execute(
                "INSERT INTO agents (uuid, path, managed, origin, status, created_at, \
                 last_status_change_at, updated_at) VALUES ('x','p',1,'run','queued',0,0,0)",
                [],
            )
            .map_err(map_sqlite)?;
            Err(OrcrError::server_error("test", "boom"))
        });
        assert!(r.is_err());
        // rolled back — no row
        assert_eq!(agent_count(&s), 0);
    }

    #[test]
    fn events_are_monotonic_and_readable() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s
            .append_event("agent.created", Some("u1"), &json!({"x":1}))
            .unwrap();
        let b = s
            .append_event("agent.ended", Some("u1"), &json!({"x":2}))
            .unwrap();
        assert!(b > a);
        assert_eq!(s.latest_event_seq().unwrap(), b);
        let rows = s.events_since(0, 100).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].seq, a);
        assert_eq!(rows[0].kind, "agent.created");
        assert_eq!(rows[0].payload["x"], 1);
        // since_seq filters strictly greater.
        let after = s.events_since(a, 100).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].seq, b);
    }

    #[test]
    fn trim_events_bounds_retention() {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..10 {
            s.append_event("k", None, &json!({ "i": i })).unwrap();
        }
        assert_eq!(s.latest_event_seq().unwrap(), 10);
        // Keep only the last 3.
        let oldest = s.trim_events(3).unwrap();
        assert_eq!(oldest, 8);
        assert_eq!(s.oldest_event_seq().unwrap(), Some(8));
        assert_eq!(s.events_since(0, 100).unwrap().len(), 3);
    }

    /// Empty per-provider caps (promotion falls back to the global max).
    fn caps(_max: u32) -> std::collections::BTreeMap<String, u32> {
        std::collections::BTreeMap::new()
    }

    #[test]
    fn enqueue_allocates_fifo_queue_seq_and_created_event() {
        let mut s = Store::open_in_memory().unwrap();
        let (q1, ev1) = s
            .enqueue_agent(&NewAgent::queued("u1", "review/a", "claude"))
            .unwrap();
        let (q2, _) = s
            .enqueue_agent(&NewAgent::queued("u2", "review/b", "claude"))
            .unwrap();
        assert_eq!(q1, 1);
        assert_eq!(q2, 2);
        assert!(ev1 > 0);
        assert_eq!(s.queue_position("u1").unwrap(), Some(1));
        assert_eq!(s.queue_position("u2").unwrap(), Some(2));
    }

    #[test]
    fn enqueue_path_in_use_reports_occupant() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "review/a", "claude"))
            .unwrap();
        let e = s
            .enqueue_agent(&NewAgent::queued("u2", "review/a", "claude"))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::StateConflict);
        assert_eq!(e.details["reason"], "path_in_use");
        assert_eq!(e.details["occupant"]["uuid"], "u1");
        assert_eq!(e.details["occupant"]["status"], "queued");
    }

    #[test]
    fn promotion_respects_global_cap_and_fifo() {
        let mut s = Store::open_in_memory().unwrap();
        for i in 0..5 {
            s.enqueue_agent(&NewAgent::queued(
                format!("u{i}"),
                format!("w/a{i}"),
                "claude",
            ))
            .unwrap();
        }
        // Cap 2 → exactly the first two (FIFO) promote.
        let (promoted, _) = s.promote_queued(2, &caps(2), now_millis()).unwrap();
        assert_eq!(promoted.len(), 2);
        assert_eq!(promoted[0].uuid, "u0");
        assert_eq!(promoted[1].uuid, "u1");
        for p in &promoted {
            assert_eq!(p.status, "starting");
        }
        // No more capacity until one ends.
        let (again, _) = s.promote_queued(2, &caps(2), now_millis()).unwrap();
        assert!(again.is_empty());
        // End one → the next FIFO agent promotes.
        s.transition_status("u0", "ended", Some("completed"))
            .unwrap();
        let (more, _) = s.promote_queued(2, &caps(2), now_millis()).unwrap();
        assert_eq!(more.len(), 1);
        assert_eq!(more[0].uuid, "u2");
    }

    #[test]
    fn promotion_respects_per_provider_cap() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("c1", "w/c1", "claude"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("c2", "w/c2", "claude"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("x1", "w/x1", "codex"))
            .unwrap();
        // Global 10, but claude capped at 1 → only c1 (claude) + x1 (codex) promote.
        let mut per = std::collections::BTreeMap::new();
        per.insert("claude".to_string(), 1);
        let (promoted, _) = s.promote_queued(10, &per, now_millis()).unwrap();
        let ids: Vec<&str> = promoted.iter().map(|a| a.uuid.as_str()).collect();
        assert!(ids.contains(&"c1"));
        assert!(ids.contains(&"x1"));
        assert!(!ids.contains(&"c2"), "claude at cap 1 — c2 waits");
    }

    #[test]
    fn resolution_path_first_then_uuid() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued(
            "11111111-2222-3333-4444-555555555555",
            "review/worker",
            "claude",
        ))
        .unwrap();
        // Active path resolves.
        let r = s.find_by_path("review/worker").unwrap().unwrap();
        assert_eq!(r.tag(), "active");
        assert_eq!(r.row().uuid, "11111111-2222-3333-4444-555555555555");
        // End it, insert a new one at the same path.
        s.transition_status(
            "11111111-2222-3333-4444-555555555555",
            "ended",
            Some("completed"),
        )
        .unwrap();
        s.enqueue_agent(&NewAgent::queued(
            "99999999-2222-3333-4444-555555555555",
            "review/worker",
            "claude",
        ))
        .unwrap();
        let r = s.find_by_path("review/worker").unwrap().unwrap();
        assert_eq!(r.tag(), "active");
        assert_eq!(r.row().uuid, "99999999-2222-3333-4444-555555555555");
        // uuid prefix (≥8) resolves the exact historical row.
        match s.find_by_uuid_or_prefix("11111111").unwrap() {
            UuidLookup::Found(a) => assert_eq!(a.status, "ended"),
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_uuid_prefix() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued(
            "abcd0000-0000-0000-0000-000000000001",
            "w/a",
            "claude",
        ))
        .unwrap();
        s.enqueue_agent(&NewAgent::queued(
            "abcd0000-0000-0000-0000-000000000002",
            "w/b",
            "claude",
        ))
        .unwrap();
        match s.find_by_uuid_or_prefix("abcd0000").unwrap() {
            UuidLookup::Ambiguous(cands) => assert_eq!(cands.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn location_session_cancel_and_turns() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "review/worker", "claude"))
            .unwrap();
        s.record_location("u1", "orcr", "term_x", "w1:p1").unwrap();
        s.record_agent_session("u1", "id", "sess-1").unwrap();
        let a = s.agent_full("u1").unwrap().unwrap();
        assert_eq!(a.pane_id.as_deref(), Some("w1:p1"));
        assert_eq!(a.terminal_id.as_deref(), Some("term_x"));
        assert_eq!(a.agent_session_value.as_deref(), Some("sess-1"));
        assert!(!a.cancel_requested);
        assert!(s.request_cancel("u1").unwrap());
        assert!(s.is_cancel_requested("u1").unwrap());
        let (seq, _) = s
            .deliver_input("u1", "orcr", now_millis())
            .unwrap()
            .unwrap();
        assert_eq!(seq, 1);
        assert_eq!(
            s.deliver_input("u1", "orcr", now_millis())
                .unwrap()
                .unwrap()
                .0,
            2
        );
        // A terminal row is never revived: deliver_input refuses (returns None) rather than
        // flipping ended→working (§5.6 concurrent-kill guard).
        s.transition_status("u1", "ended", Some("killed")).unwrap();
        assert!(s
            .deliver_input("u1", "orcr", now_millis())
            .unwrap()
            .is_none());
        assert_eq!(s.agent_full("u1").unwrap().unwrap().status, "ended");
    }

    #[test]
    fn settle_primed_idle_is_guarded_on_starting() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "w/a", "mock"))
            .unwrap();
        s.promote_queued(10, &caps(10), now_millis()).unwrap(); // → starting
        assert_eq!(s.agent_full("u1").unwrap().unwrap().status, "starting");
        // From `starting`, settle succeeds and stamps the idle clock.
        assert!(s.settle_primed_idle("u1", now_millis()).unwrap().is_some());
        let a = s.agent_full("u1").unwrap().unwrap();
        assert_eq!(a.status, "idle");
        assert!(a.idle_since.is_some());
        // Not `starting` any more → a second settle is a no-op (returns None).
        assert!(s.settle_primed_idle("u1", now_millis()).unwrap().is_none());
        // A concurrently-ended row is never revived to `idle` by the spawn pipeline's settle.
        s.enqueue_agent(&NewAgent::queued("u2", "w/b", "mock"))
            .unwrap();
        s.promote_queued(10, &caps(10), now_millis()).unwrap();
        s.transition_status("u2", "ended", Some("canceled"))
            .unwrap();
        assert!(s.settle_primed_idle("u2", now_millis()).unwrap().is_none());
        assert_eq!(s.agent_full("u2").unwrap().unwrap().status, "ended");
    }

    #[test]
    fn status_counts_reports_managed_and_unmanaged() {
        let mut s = Store::open_in_memory().unwrap();
        // Two managed: one queued, one working.
        s.enqueue_agent(&NewAgent::queued("m1", "w/q", "mock"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("m2", "w/w", "mock"))
            .unwrap();
        s.record_location("m2", "orcr", "t2", "w1:p2").unwrap();
        s.deliver_input("m2", "orcr", now_millis())
            .unwrap()
            .unwrap(); // → working
                       // One unmanaged active + one ended (excluded from every count).
        s.insert_unmanaged(
            "un1",
            "unmanaged/x/y",
            "default",
            "termu",
            "wU:pU",
            Some("claude"),
            "idle",
            None,
        )
        .unwrap();
        s.enqueue_agent(&NewAgent::queued("m3", "w/e", "mock"))
            .unwrap();
        s.transition_status("m3", "ended", Some("completed"))
            .unwrap();
        let c = s.status_counts().unwrap();
        assert_eq!(
            c.live, 2,
            "m1(queued)+m2(working) are managed-live; m3 ended excluded"
        );
        assert_eq!(c.queued, 1, "only m1");
        assert_eq!(c.unmanaged, 1, "un1 active; ended excluded");
    }

    #[test]
    fn stuck_starting_only_targets_paneless() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "w/a", "claude"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("u2", "w/b", "claude"))
            .unwrap();
        s.promote_queued(10, &caps(10), 1_000).unwrap(); // starting_at = 1000
                                                         // u2 recorded a pane → progress, exempt.
        s.record_location("u2", "orcr", "t2", "w1:p2").unwrap();
        let stuck = s.stuck_starting(2_000).unwrap();
        let ids: Vec<&str> = stuck.iter().map(|a| a.uuid.as_str()).collect();
        assert_eq!(ids, vec!["u1"]);
    }

    #[test]
    fn ls_filters() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "review/a", "claude"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("u2", "review/b", "codex"))
            .unwrap();
        s.enqueue_agent(&NewAgent::queued("u3", "verify/c", "claude"))
            .unwrap();
        s.transition_status("u3", "ended", Some("completed"))
            .unwrap();
        // Default excludes ended.
        let active = s.list_agents(&AgentFilter::default()).unwrap();
        assert_eq!(active.len(), 2);
        // Pattern review/* → u1, u2.
        let f = AgentFilter {
            pattern: Some("review/*".into()),
            ..Default::default()
        };
        assert_eq!(s.list_agents(&f).unwrap().len(), 2);
        // Provider filter.
        let f = AgentFilter {
            provider: Some("codex".into()),
            ..Default::default()
        };
        assert_eq!(s.list_agents(&f).unwrap().len(), 1);
        // include_ended surfaces u3.
        let f = AgentFilter {
            include_ended: true,
            ..Default::default()
        };
        assert_eq!(s.list_agents(&f).unwrap().len(), 3);
    }

    /// Move an agent into a live `idle` state with a recorded pane (park test scaffolding).
    fn make_idle(s: &mut Store, uuid: &str, path: &str) {
        s.enqueue_agent(&NewAgent::queued(uuid, path, "mock"))
            .unwrap();
        s.record_location(uuid, "orcr", &format!("term_{uuid}"), &format!("w1:{uuid}"))
            .unwrap();
        s.deliver_input(uuid, "orcr", now_millis())
            .unwrap()
            .unwrap();
        s.complete_turn(uuid, 1, now_millis(), None).unwrap();
        s.set_idle_since(uuid, Some(now_millis())).unwrap();
    }

    #[test]
    fn park_candidates_respect_gc_mode_and_idle_clock() {
        let mut s = Store::open_in_memory().unwrap();
        make_idle(&mut s, "u1", "w/a");
        // A `gc never` agent is never a park candidate.
        make_idle(&mut s, "u2", "w/b");
        s.conn
            .execute("UPDATE agents SET gc_mode='never' WHERE uuid='u2'", [])
            .unwrap();
        let cands = s.park_candidates(now_millis() + 1).unwrap();
        let ids: Vec<&str> = cands.iter().map(|a| a.uuid.as_str()).collect();
        assert_eq!(ids, vec!["u1"]);
        // Idle-clock gate: cutoff before idle_since → no candidates.
        assert!(s.park_candidates(0).unwrap().is_empty());
    }

    #[test]
    fn two_phase_park_and_reap() {
        let mut s = Store::open_in_memory().unwrap();
        make_idle(&mut s, "u1", "w/a");
        // Begin the move (CAS from idle).
        assert!(s.begin_move("u1", "idle", "parking", "tok1").unwrap());
        // A second begin on the same row loses (lease already held).
        assert!(!s.begin_move("u1", "idle", "parking", "tok2").unwrap());
        // Finishing with the wrong token is a no-op.
        assert_eq!(s.finish_park("u1", "wrong", "orcr", "t", "p").unwrap(), 0);
        // Finish with the right token → parked.
        assert!(s.finish_park("u1", "tok1", "orcr", "t2", "w9:p2").unwrap() > 0);
        let a = s.agent_full("u1").unwrap().unwrap();
        assert_eq!(a.status, "parked");
        assert_eq!(a.move_state, "none");
        assert!(a.move_token.is_none());
        assert!(a.parked_at.is_some());
        assert_eq!(a.pane_id.as_deref(), Some("w9:p2"));
        // Reap candidate once parked_at is in the past.
        let reap = s.reap_candidates(now_millis() + 1).unwrap();
        assert_eq!(reap.len(), 1);
        assert!(s.reap_candidates(0).unwrap().is_empty());
    }

    #[test]
    fn unpark_resets_clock_and_rollback_restores() {
        let mut s = Store::open_in_memory().unwrap();
        make_idle(&mut s, "u1", "w/a");
        s.begin_move("u1", "idle", "parking", "t").unwrap();
        s.finish_park("u1", "t", "orcr", "t2", "p2").unwrap();
        // Un-park two-phase.
        assert!(s.begin_move("u1", "parked", "unparking", "u").unwrap());
        assert!(s.finish_unpark("u1", "u", "orcr", "t3", "p3").unwrap() > 0);
        let a = s.agent_full("u1").unwrap().unwrap();
        assert_eq!(a.status, "idle");
        assert!(a.parked_at.is_none());
        assert!(a.idle_since.is_some());
        // Rollback path: begin a move, roll it back — status stays put, lease cleared.
        assert!(s.begin_move("u1", "idle", "parking", "r").unwrap());
        assert_eq!(s.agents_in_move().unwrap().len(), 1);
        assert!(s.rollback_move("u1", "r").unwrap());
        assert_eq!(s.agent_full("u1").unwrap().unwrap().status, "idle");
        assert!(s.agents_in_move().unwrap().is_empty());
    }

    #[test]
    fn timed_out_agents_selects_deadline_passed() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent {
            deadline_at: Some(now_millis() - 1),
            ..NewAgent::queued("u1", "w/a", "mock")
        })
        .unwrap();
        s.enqueue_agent(&NewAgent {
            deadline_at: Some(now_millis() + 60_000),
            ..NewAgent::queued("u2", "w/b", "mock")
        })
        .unwrap();
        let out = s.timed_out_agents(now_millis()).unwrap();
        let ids: Vec<&str> = out.iter().map(|a| a.uuid.as_str()).collect();
        assert_eq!(ids, vec!["u1"]);
    }

    #[test]
    fn attach_lease_guards_gc_and_expires() {
        let mut s = Store::open_in_memory().unwrap();
        make_idle(&mut s, "u1", "w/a");
        let (info, ev) = s
            .prepare_attach("u1", "lease1", "observe", "cli", 42, 30_000)
            .unwrap();
        assert!(ev > 0);
        assert_eq!(info.terminal_id, "term_u1");
        assert!(s.has_fresh_lease("u1", now_millis()).unwrap());
        // Heartbeat keeps it fresh.
        assert!(s.heartbeat_lease("lease1", 30_000).unwrap());
        // A far-future `now` sees it expired.
        assert!(!s.has_fresh_lease("u1", now_millis() + 60_000).unwrap());
        // expire_leases cleans it up.
        assert!(s.expire_leases(now_millis() + 60_000).unwrap() > 0);
        assert!(!s.has_fresh_lease("u1", now_millis()).unwrap());
        // Release is a no-op once gone.
        assert_eq!(s.release_lease("lease1").unwrap(), 0);
    }

    #[test]
    fn prepare_attach_rejects_queued() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("u1", "w/a", "mock"))
            .unwrap();
        let e = s
            .prepare_attach("u1", "l", "observe", "cli", 1, 1000)
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::StateConflict);
    }

    #[test]
    fn unmanaged_insert_update_and_lookup() {
        let mut s = Store::open_in_memory().unwrap();
        assert!(s.find_unmanaged("main", "term_x").unwrap().is_none());
        let ev = s
            .insert_unmanaged(
                "uu",
                "unmanaged/main/w6_p1",
                "main",
                "term_x",
                "w6:p1",
                Some("claude"),
                "working",
                Some(("id", "sess")),
            )
            .unwrap();
        assert!(ev > 0);
        let found = s.find_unmanaged("main", "term_x").unwrap().unwrap();
        assert!(!found.managed);
        assert_eq!(found.status, "working");
        assert_eq!(found.agent.as_deref(), Some("claude"));
        // Status change emits; an unchanged status does not.
        assert!(s.update_unmanaged("uu", "idle", "w6:p1", None).unwrap() > 0);
        assert_eq!(s.update_unmanaged("uu", "idle", "w6:p1", None).unwrap(), 0);
        assert_eq!(s.active_unmanaged("main").unwrap().len(), 1);
        // Terminal gone → ended → drops out of active set.
        s.transition_status("uu", "ended", None).unwrap();
        assert!(s.active_unmanaged("main").unwrap().is_empty());
        assert!(!s.path_active("unmanaged/main/w6_p1").unwrap());
    }

    #[test]
    fn agent_full_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        s.enqueue_agent(&NewAgent::queued("uuu", "a/b/c", "codex"))
            .unwrap();
        let row = s.agent_full("uuu").unwrap().unwrap();
        assert_eq!(row.path, "a/b/c");
        assert_eq!(row.status, "queued");
        assert!(row.managed);
        assert_eq!(row.agent.as_deref(), Some("codex"));
        assert!(s.agent_full("missing").unwrap().is_none());
    }

    fn new_loop(uuid: &str, name: &str, max: i64, overlap: &str) -> NewLoop {
        NewLoop {
            uuid: uuid.into(),
            name: name.into(),
            cadence_kind: "cron".into(),
            cadence_value: "*/5 * * * *".into(),
            tz: "UTC".into(),
            cwd: "/tmp".into(),
            max_concurrency: max,
            overlap: overlap.into(),
            timeout_s: None,
            next_fire_at: Some(1000),
            created_at: now_millis(),
        }
    }

    #[test]
    fn loop_name_unique_among_active_reusable_after_end() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 1, "queue"))
            .unwrap();
        // A second active loop with the same name is rejected.
        let e = s
            .create_loop(&new_loop("l2", "nightly", 1, "queue"))
            .unwrap_err();
        assert_eq!(e.details["reason"], "loop_name_in_use");
        // End the first; the name is reusable.
        s.set_loop_status("l1", "ended", Some("removed"), "loop.removed")
            .unwrap();
        assert!(s
            .create_loop(&new_loop("l3", "nightly", 1, "queue"))
            .is_ok());
        // find_loop_by_name resolves the active one, not the ended.
        let found = s.find_loop_by_name("nightly").unwrap().unwrap();
        assert_eq!(found.uuid, "l3");
        // Names of active loops for namespace protection.
        assert_eq!(s.active_loop_names().unwrap(), vec!["nightly".to_string()]);
    }

    #[test]
    fn run_allocation_capacity_and_coalesce() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 1, "queue"))
            .unwrap();
        // First scheduled fire: a slot is free → start_now.
        let (a1, _) = s
            .allocate_run("l1", "scheduled", 1000, 1, "queue", None)
            .unwrap();
        let run1 = match a1 {
            RunAllocation::Allocated { run, start_now } => {
                assert!(start_now);
                run
            }
            _ => panic!("expected Allocated"),
        };
        assert!(run1.run_id.starts_with('r') && run1.run_id.len() == 6);
        // Mark it running (occupies the only slot).
        s.record_run_start(&run1.uuid, 111, 111, Some(5), now_millis(), None)
            .unwrap();
        // Second scheduled fire at capacity → allocated pending (not start_now).
        let (a2, _) = s
            .allocate_run("l1", "scheduled", 2000, 1, "queue", None)
            .unwrap();
        let run2 = match a2 {
            RunAllocation::Allocated { run, start_now } => {
                assert!(!start_now);
                run
            }
            _ => panic!("expected Allocated pending"),
        };
        // Third scheduled fire coalesces into the single pending run, keeping earliest due_at.
        let (a3, _) = s
            .allocate_run("l1", "scheduled", 1500, 1, "queue", None)
            .unwrap();
        match a3 {
            RunAllocation::Coalesced { run } => {
                assert_eq!(run.uuid, run2.uuid);
                assert_eq!(run.due_at, Some(1500)); // earliest of 2000 and 1500
            }
            _ => panic!("expected Coalesced"),
        }
        // A manual fire at capacity always allocates its own run (pending).
        let (a4, _) = s
            .allocate_run("l1", "manual", 3000, 1, "queue", None)
            .unwrap();
        assert!(matches!(
            a4,
            RunAllocation::Allocated {
                start_now: false,
                ..
            }
        ));
    }

    #[test]
    fn fresh_allocation_reserves_slot_and_emits_fired() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 1, "queue"))
            .unwrap();
        // A free-slot allocation reserves the slot *in the same txn* by inserting `running`,
        // so it counts toward capacity immediately — before record_run_start runs.
        let (a1, ev1) = s
            .allocate_run("l1", "scheduled", 1000, 1, "queue", None)
            .unwrap();
        let run1 = match a1 {
            RunAllocation::Allocated { run, start_now } => {
                assert!(start_now);
                run
            }
            _ => panic!("expected Allocated"),
        };
        assert_eq!(
            s.run_by_uuid(&run1.uuid).unwrap().unwrap().status,
            "running"
        );
        assert_eq!(s.active_runs("l1").unwrap().len(), 1);
        // A freshly-queued run emits loop.fired(pending:true), never loop.coalesced.
        let fired = s.events_since(ev1 - 1, 10).unwrap();
        let (a2, ev2) = s
            .allocate_run("l1", "manual", 2000, 1, "queue", None)
            .unwrap();
        assert!(matches!(
            a2,
            RunAllocation::Allocated {
                start_now: false,
                ..
            }
        ));
        assert!(fired.iter().any(|e| e.kind == "loop.fired"));
        let queued = s.events_since(ev2 - 1, 10).unwrap();
        let qev = queued.iter().find(|e| e.seq == ev2).unwrap();
        assert_eq!(qev.kind, "loop.fired");
        assert_eq!(qev.payload["pending"], serde_json::json!(true));
    }

    #[test]
    fn claim_pending_run_never_exceeds_capacity() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 2, "queue"))
            .unwrap();
        // Fill both slots (fresh allocations reserve them as running).
        let r1 = match s
            .allocate_run("l1", "manual", 1, 2, "queue", None)
            .unwrap()
            .0
        {
            RunAllocation::Allocated { run, .. } => run,
            _ => panic!(),
        };
        s.allocate_run("l1", "manual", 2, 2, "queue", None).unwrap();
        // Two queued runs behind them.
        s.allocate_run("l1", "manual", 3, 2, "queue", None).unwrap();
        s.allocate_run("l1", "manual", 4, 2, "queue", None).unwrap();
        assert_eq!(s.active_runs("l1").unwrap().len(), 2);
        // At capacity → claim reserves nothing.
        assert!(s.claim_pending_run("l1", 2).unwrap().is_none());
        // Free one slot → exactly one claim succeeds, then no more.
        s.finish_run(&r1.uuid, "ok", Some(0), None).unwrap();
        let claimed = s.claim_pending_run("l1", 2).unwrap();
        assert!(claimed.is_some());
        assert_eq!(
            s.run_by_uuid(&claimed.unwrap().uuid)
                .unwrap()
                .unwrap()
                .status,
            "running"
        );
        assert_eq!(s.active_runs("l1").unwrap().len(), 2);
        assert!(s.claim_pending_run("l1", 2).unwrap().is_none());
    }

    #[test]
    fn record_run_start_preserves_stopping_barrier() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 1, "queue"))
            .unwrap();
        let run = match s
            .allocate_run("l1", "manual", 1, 1, "queue", None)
            .unwrap()
            .0
        {
            RunAllocation::Allocated { run, .. } => run,
            _ => panic!(),
        };
        // A stop lands during the spawn window (before record_run_start).
        s.set_run_stopping(&run.uuid).unwrap();
        // record_run_start fills the pid so the killer can find the pgid, but must NOT flip the
        // run back to `running` and clobber the stopping barrier.
        s.record_run_start(&run.uuid, 42, 42, Some(9), now_millis(), None)
            .unwrap();
        let cur = s.run_by_uuid(&run.uuid).unwrap().unwrap();
        assert_eq!(cur.status, "stopping");
        assert_eq!(cur.pid, Some(42));
    }

    #[test]
    fn run_allocation_skip_drops_at_capacity() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 1, "skip"))
            .unwrap();
        let (a1, _) = s
            .allocate_run("l1", "scheduled", 1000, 1, "skip", None)
            .unwrap();
        let run1 = match a1 {
            RunAllocation::Allocated { run, .. } => run,
            _ => panic!(),
        };
        s.record_run_start(&run1.uuid, 1, 1, Some(1), now_millis(), None)
            .unwrap();
        let (a2, _) = s
            .allocate_run("l1", "scheduled", 2000, 1, "skip", None)
            .unwrap();
        assert!(matches!(a2, RunAllocation::Skipped));
        // No pending run was created.
        assert!(s
            .runs_for_loop("l1", Some("pending"), false)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn run_lifecycle_and_lookup() {
        let mut s = Store::open_in_memory().unwrap();
        s.create_loop(&new_loop("l1", "nightly", 2, "queue"))
            .unwrap();
        let (a, _) = s
            .allocate_run("l1", "manual", 1000, 2, "queue", None)
            .unwrap();
        let run = match a {
            RunAllocation::Allocated { run, .. } => run,
            _ => panic!(),
        };
        // Lookup by run_id and uuid.
        assert_eq!(
            s.run_by_id_or_uuid("l1", &run.run_id)
                .unwrap()
                .unwrap()
                .uuid,
            run.uuid
        );
        assert_eq!(
            s.run_by_id_or_uuid("l1", &run.uuid)
                .unwrap()
                .unwrap()
                .run_id,
            run.run_id
        );
        s.record_run_start(&run.uuid, 42, 42, Some(9), now_millis(), None)
            .unwrap();
        assert_eq!(s.active_runs("l1").unwrap().len(), 1);
        s.finish_run(&run.uuid, "ok", Some(0), None).unwrap();
        let done = s.run_by_uuid(&run.uuid).unwrap().unwrap();
        assert_eq!(done.status, "ok");
        assert_eq!(done.exit_code, Some(0));
        // Default runs_for_loop excludes history; --all includes it.
        assert!(s.runs_for_loop("l1", None, false).unwrap().is_empty());
        assert_eq!(s.runs_for_loop("l1", None, true).unwrap().len(), 1);
    }
}
