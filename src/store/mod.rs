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

    /// Insert an agent row, enforcing path reservation in one immediate transaction.
    /// A collision with an active agent on the same path yields `state_conflict`
    /// (`reason: path_in_use`).
    pub fn insert_agent(&mut self, a: &NewAgent) -> Result<()> {
        let a = a.clone();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "INSERT INTO agents (
                     uuid, path, managed, origin, parent_id, agent, model, effort,
                     gc_mode, cwd, herdr_session, terminal_id, pane_id, launch_token,
                     status, created_at, last_status_change_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                     ?9, ?10, ?11, ?12, ?13, ?14,
                     ?15, ?16, ?16, ?16
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
                    a.status,
                    a.created_at,
                ],
            )
            .map_err(|e| map_insert_conflict(e, &a.path))?;
            Ok(())
        })
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

    /// Fetch a minimal view of an agent by uuid.
    pub fn get_agent(&self, uuid: &str) -> Result<Option<AgentRow>> {
        self.conn
            .query_row(
                "SELECT uuid, path, status, managed, agent FROM agents WHERE uuid = ?1",
                [uuid],
                |r| {
                    Ok(AgentRow {
                        uuid: r.get(0)?,
                        path: r.get(1)?,
                        status: r.get(2)?,
                        managed: r.get::<_, i64>(3)? != 0,
                        agent: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(map_sqlite)
    }

    /// Update an agent's status (M0 helper — later milestones own the full state machine).
    /// When moving to `ended`, `exit_reason` and `ended_at` are set.
    pub fn set_agent_status(
        &mut self,
        uuid: &str,
        status: &str,
        exit_reason: Option<&str>,
    ) -> Result<()> {
        let uuid = uuid.to_string();
        let status = status.to_string();
        let exit_reason = exit_reason.map(|s| s.to_string());
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            let ended_at = if status == "ended" { Some(now) } else { None };
            let n = tx
                .execute(
                    "UPDATE agents
                        SET status = ?2,
                            exit_reason = COALESCE(?3, exit_reason),
                            ended_at = COALESCE(?4, ended_at),
                            last_status_change_at = ?5,
                            updated_at = ?5
                      WHERE uuid = ?1",
                    rusqlite::params![uuid, status, exit_reason, ended_at, now],
                )
                .map_err(map_sqlite)?;
            if n == 0 {
                return Err(OrcrError::not_found(format!("no agent with uuid {uuid}")));
            }
            Ok(())
        })
    }

    /// Count agent rows (tests / diagnostics).
    pub fn count_agents(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))
            .map_err(map_sqlite)
    }

    /// Read the full row for an agent by uuid.
    pub fn agent_full(&self, uuid: &str) -> Result<Option<AgentFull>> {
        self.conn
            .query_row(AGENT_FULL_SELECT_ONE, [uuid], read_agent_full_row)
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

    /// Bump `input_seq` and record a turn row for a delivered input (spec §5.6, §12). The
    /// completed turn-tracking semantics land in M3; M2 writes the bookkeeping row so the
    /// epoch is durable. Returns the new `input_seq`.
    pub fn bump_input_seq(&mut self, uuid: &str, source: &str) -> Result<i64> {
        let (uuid, source) = (uuid.to_string(), source.to_string());
        let now = now_millis();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET input_seq = input_seq + 1, updated_at=?2 WHERE uuid=?1",
                rusqlite::params![uuid, now],
            )
            .map_err(map_sqlite)?;
            let seq: i64 = tx
                .query_row("SELECT input_seq FROM agents WHERE uuid=?1", [&uuid], |r| {
                    r.get(0)
                })
                .map_err(map_sqlite)?;
            tx.execute(
                "INSERT INTO turns (agent_uuid, input_seq, source, delivered_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![uuid, seq, source, now],
            )
            .map_err(map_sqlite)?;
            Ok(seq)
        })
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
    /// Emits `agent.status_changed`. Returns `(input_seq, event_seq)`.
    pub fn deliver_input(&mut self, uuid: &str, source: &str, at: i64) -> Result<(i64, i64)> {
        let (uuid, source) = (uuid.to_string(), source.to_string());
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE agents SET input_seq = input_seq + 1, status='working', \
                 blocked_kind=NULL, idle_since=NULL, \
                 last_status_change_at=?2, updated_at=?2 WHERE uuid=?1",
                rusqlite::params![uuid, at],
            )
            .map_err(map_sqlite)?;
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
            Ok((seq, ev))
        })
    }

    /// Open a synthetic **external** turn (spec §5.6): input orcr didn't deliver, observed as
    /// a `working` transition with no pending turn. Same effect as [`deliver_input`] with
    /// `source=external`, plus `working_seen_at` set (we saw the working that triggered it).
    pub fn open_external_turn(&mut self, uuid: &str, at: i64) -> Result<(i64, i64)> {
        let (seq, ev) = self.deliver_input(uuid, "external", at)?;
        let uuid = uuid.to_string();
        self.with_immediate_tx(|tx| {
            tx.execute(
                "UPDATE turns SET working_seen_at=?3 WHERE agent_uuid=?1 AND input_seq=?2",
                rusqlite::params![uuid, seq, at],
            )
            .map_err(map_sqlite)?;
            Ok(())
        })?;
        Ok((seq, ev))
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
                     WHERE uuid=?1 AND status='working'",
                    rusqlite::params![uuid, at],
                )
                .map_err(map_sqlite)?;
            if updated == 0 {
                // Turn marked complete but the public status was not `working` (e.g. already
                // idle/parked) — record the turn but emit nothing new.
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

    /// Direct read access for later milestones / tests.
    pub fn conn(&self) -> &Connection {
        &self.conn
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

/// One row from the events table.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub ts: i64,
    pub kind: String,
    pub ref_uuid: Option<String>,
    pub payload: serde_json::Value,
}

/// The live location read under the attach-prepare transaction (spec §6.1). `terminal_id` is
/// globally unique and stable across pane moves, so the exec command addresses it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachInfo {
    pub terminal_id: String,
    pub pane_id: Option<String>,
    pub herdr_session: Option<String>,
}

/// A minimal agent read view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub uuid: String,
    pub path: String,
    pub status: String,
    pub managed: bool,
    pub agent: Option<String>,
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

/// The same read scoped to one uuid.
const AGENT_FULL_SELECT_ONE: &str =
    "SELECT uuid, path, managed, origin, parent_id, agent, model, effort, \
     gc_mode, cwd, herdr_session, terminal_id, pane_id, launch_token, \
     agent_session_kind, agent_session_value, status, move_state, move_token, \
     blocked_kind, input_seq, \
     cancel_requested, exit_reason, queue_seq, deadline_at, created_at, starting_at, \
     idle_since, parked_at, last_status_change_at, ended_at FROM agents WHERE uuid = ?1";

/// Deserialize an `AgentFull` from a row selecting [`AGENT_FULL_COLUMNS`] in order.
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
    tx.query_row(AGENT_FULL_SELECT_ONE, [uuid], read_agent_full_row)
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

    #[test]
    fn partial_unique_index_reserves_active_paths() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_agent(&NewAgent::queued("u1", "review/worker", "claude"))
            .unwrap();
        // Active duplicate path fails.
        let e = s
            .insert_agent(&NewAgent::queued("u2", "review/worker", "claude"))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::StateConflict);
        assert_eq!(e.details["reason"], "path_in_use");

        // End the first, then the same path is reusable.
        s.set_agent_status("u1", "ended", Some("completed"))
            .unwrap();
        s.insert_agent(&NewAgent::queued("u3", "review/worker", "claude"))
            .unwrap();
        // u2's insert failed and rolled back, so only u1 (ended) + u3 (active) exist.
        assert_eq!(s.count_agents().unwrap(), 2);
    }

    #[test]
    fn two_ended_rows_same_path_allowed() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_agent(&NewAgent::queued("a", "p/x", "claude"))
            .unwrap();
        s.set_agent_status("a", "ended", Some("completed")).unwrap();
        s.insert_agent(&NewAgent::queued("b", "p/x", "claude"))
            .unwrap();
        s.set_agent_status("b", "ended", Some("completed")).unwrap();
        // both ended, same path — fine
        assert_eq!(s.count_agents().unwrap(), 2);
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
        assert_eq!(s.count_agents().unwrap(), 0);
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
        let seq = s.bump_input_seq("u1", "orcr").unwrap();
        assert_eq!(seq, 1);
        assert_eq!(s.bump_input_seq("u1", "orcr").unwrap(), 2);
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
        s.deliver_input(uuid, "orcr", now_millis()).unwrap();
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
    fn get_agent_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_agent(&NewAgent::queued("uuu", "a/b/c", "codex"))
            .unwrap();
        let row = s.get_agent("uuu").unwrap().unwrap();
        assert_eq!(row.path, "a/b/c");
        assert_eq!(row.status, "queued");
        assert!(row.managed);
        assert_eq!(row.agent.as_deref(), Some("codex"));
        assert!(s.get_agent("missing").unwrap().is_none());
    }
}
