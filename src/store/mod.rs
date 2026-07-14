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

    /// Direct read access for later milestones / tests.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
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
    pub created_at: i64,
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
