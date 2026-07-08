use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

const USER_VERSION: i64 = 1;

#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRow {
    pub id: String,
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub kind: String,
    pub harness: String,
    pub model: String,
    pub effort: String,
    pub host: String,
    pub herdr_session: String,
    pub pane_id: Option<String>,
    pub terminal_id: Option<String>,
    pub cwd: String,
    pub worktree: Option<String>,
    pub status: String,
    pub exit_reason: Option<String>,
    pub keep: bool,
    pub timeout_s: i64,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub run_dir: String,
    pub agent_session_kind: Option<String>,
    pub agent_session_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    pub id: String,
    pub job_type: String,
    pub spec_json: String,
    pub status: String,
    pub tz: Option<String>,
    pub next_run_at: Option<String>,
    pub expires_at: Option<String>,
    pub runs_count: i64,
    pub created_at: String,
    pub ended_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnRow {
    pub agent_id: String,
    pub n: i64,
    pub prompt_paths: String,
    pub response_path: String,
    pub response_source: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    pub seq: i64,
    pub ts: String,
    pub kind: String,
    pub ref_id: Option<String>,
    pub payload_json: String,
}

impl AgentRow {
    pub fn new(
        name: Option<String>,
        kind: impl Into<String>,
        harness: impl Into<String>,
        created_at: impl Into<String>,
        run_dir: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            parent_id: None,
            kind: kind.into(),
            harness: harness.into(),
            model: String::new(),
            effort: String::new(),
            host: String::new(),
            herdr_session: "orcr".to_string(),
            pane_id: None,
            terminal_id: None,
            cwd: String::new(),
            worktree: None,
            status: "queued".to_string(),
            exit_reason: None,
            keep: false,
            timeout_s: 600,
            created_at: created_at.into(),
            ended_at: None,
            run_dir: run_dir.into(),
            agent_session_kind: None,
            agent_session_value: None,
        }
    }
}

impl JobRow {
    pub fn new(
        job_type: impl Into<String>,
        spec_json: impl Into<String>,
        status: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            job_type: job_type.into(),
            spec_json: spec_json.into(),
            status: status.into(),
            tz: None,
            next_run_at: None,
            expires_at: None,
            runs_count: 0,
            created_at: created_at.into(),
            ended_reason: None,
        }
    }
}

impl TurnRow {
    pub fn new(
        agent_id: impl Into<String>,
        n: i64,
        prompt_paths: impl Into<String>,
        response_path: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Self {
        Self {
            agent_id: agent_id.into(),
            n,
            prompt_paths: prompt_paths.into(),
            response_path: response_path.into(),
            response_source: None,
            started_at: started_at.into(),
            ended_at: None,
            tokens_in: None,
            tokens_out: None,
        }
    }
}

impl EventRow {
    pub fn new(
        ts: impl Into<String>,
        kind: impl Into<String>,
        ref_id: Option<String>,
        payload_json: impl Into<String>,
    ) -> Self {
        Self {
            seq: 0,
            ts: ts.into(),
            kind: kind.into(),
            ref_id,
            payload_json: payload_json.into(),
        }
    }
}

impl Store {
    pub fn open(store_root: &Path) -> Result<Self> {
        fs::create_dir_all(store_root)
            .with_context(|| format!("failed to create store root {}", store_root.display()))?;
        let db_path = store_root.join("orcr.db");
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open sqlite db {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;

        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != 0 && version != USER_VERSION {
            bail!("unsupported sqlite user_version {version}; expected {USER_VERSION}");
        }

        let store = Self { conn };
        store.init_schema()?;
        Ok(store)
    }

    pub fn create_agent(&self, agent: &AgentRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO agents (
                id, name, parent_id, kind, harness, model, effort, host, herdr_session,
                pane_id, terminal_id, cwd, worktree, status, exit_reason, keep, timeout_s,
                created_at, ended_at, run_dir, agent_session_kind, agent_session_value
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                agent.id,
                agent.name,
                agent.parent_id,
                agent.kind,
                agent.harness,
                agent.model,
                agent.effort,
                agent.host,
                agent.herdr_session,
                agent.pane_id,
                agent.terminal_id,
                agent.cwd,
                agent.worktree,
                agent.status,
                agent.exit_reason,
                bool_to_int(agent.keep),
                agent.timeout_s,
                agent.created_at,
                agent.ended_at,
                agent.run_dir,
                agent.agent_session_kind,
                agent.agent_session_value,
            ],
        )?;
        Ok(())
    }

    pub fn get_agent(&self, id: &str) -> Result<Option<AgentRow>> {
        let sql = AGENT_SELECT_SQL.to_string() + " WHERE id = ?1";
        self.conn
            .query_row(&sql, [id], map_agent)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_agent_status(
        &self,
        id: &str,
        status: &str,
        exit_reason: Option<&str>,
        ended_at: Option<&str>,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE agents SET status = ?1, exit_reason = ?2, ended_at = ?3 WHERE id = ?4",
            params![status, exit_reason, ended_at, id],
        )?;
        if changed == 0 {
            bail!("agent not found: {id}");
        }
        Ok(())
    }

    pub fn list_agents(&self) -> Result<Vec<AgentRow>> {
        let mut stmt = self
            .conn
            .prepare(&(AGENT_SELECT_SQL.to_string() + " ORDER BY created_at, id"))?;
        let rows = stmt.query_map([], map_agent)?;
        collect_rows(rows)
    }

    pub fn resolve_agent_id(&self, value: &str) -> Result<String> {
        let ids = self.agent_ids_by_prefix(value)?;
        match ids.len() {
            1 => return Ok(ids[0].clone()),
            n if n > 1 => bail!("ambiguous agent id prefix: {value}"),
            _ => {}
        }

        let ids = self.agent_ids_by_name(value)?;
        match ids.len() {
            1 => Ok(ids[0].clone()),
            n if n > 1 => bail!("ambiguous agent name: {value}"),
            _ => bail!("agent not found: {value}"),
        }
    }

    pub fn create_job(&self, job: &JobRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs (
                id, type, spec_json, status, tz, next_run_at, expires_at, runs_count,
                created_at, ended_reason
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                job.id,
                job.job_type,
                job.spec_json,
                job.status,
                job.tz,
                job.next_run_at,
                job.expires_at,
                job.runs_count,
                job.created_at,
                job.ended_reason,
            ],
        )?;
        Ok(())
    }

    pub fn get_job(&self, id: &str) -> Result<Option<JobRow>> {
        let sql = JOB_SELECT_SQL.to_string() + " WHERE id = ?1";
        self.conn
            .query_row(&sql, [id], map_job)
            .optional()
            .map_err(Into::into)
    }

    pub fn list_jobs(&self) -> Result<Vec<JobRow>> {
        let mut stmt = self
            .conn
            .prepare(&(JOB_SELECT_SQL.to_string() + " ORDER BY created_at, id"))?;
        let rows = stmt.query_map([], map_job)?;
        collect_rows(rows)
    }

    pub fn create_turn(&self, turn: &TurnRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO turns (
                agent_id, n, prompt_paths, response_path, response_source, started_at,
                ended_at, tokens_in, tokens_out
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                turn.agent_id,
                turn.n,
                turn.prompt_paths,
                turn.response_path,
                turn.response_source,
                turn.started_at,
                turn.ended_at,
                turn.tokens_in,
                turn.tokens_out,
            ],
        )?;
        Ok(())
    }

    pub fn list_turns_by_agent(&self, agent_id: &str) -> Result<Vec<TurnRow>> {
        let mut stmt = self
            .conn
            .prepare(&(TURN_SELECT_SQL.to_string() + " WHERE agent_id = ?1 ORDER BY n"))?;
        let rows = stmt.query_map([agent_id], map_turn)?;
        collect_rows(rows)
    }

    pub fn update_turn(&self, turn: &TurnRow) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE turns SET prompt_paths = ?1, response_path = ?2, response_source = ?3,
                started_at = ?4, ended_at = ?5, tokens_in = ?6, tokens_out = ?7
             WHERE agent_id = ?8 AND n = ?9",
            params![
                turn.prompt_paths,
                turn.response_path,
                turn.response_source,
                turn.started_at,
                turn.ended_at,
                turn.tokens_in,
                turn.tokens_out,
                turn.agent_id,
                turn.n,
            ],
        )?;
        if changed == 0 {
            bail!("turn not found: agent_id={} n={}", turn.agent_id, turn.n);
        }
        Ok(())
    }

    pub fn append_event(&self, event: &EventRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO events (ts, kind, ref_id, payload_json) VALUES (?1, ?2, ?3, ?4)",
            params![event.ts, event.kind, event.ref_id, event.payload_json],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn list_events(&self) -> Result<Vec<EventRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT seq, ts, kind, ref_id, payload_json FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], map_event)?;
        collect_rows(rows)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                name TEXT,
                parent_id TEXT,
                kind TEXT,
                harness TEXT,
                model TEXT,
                effort TEXT,
                host TEXT,
                herdr_session TEXT,
                pane_id TEXT,
                terminal_id TEXT,
                cwd TEXT,
                worktree TEXT,
                status TEXT,
                exit_reason TEXT,
                keep INT,
                timeout_s INT,
                created_at TEXT,
                ended_at TEXT,
                run_dir TEXT,
                agent_session_kind TEXT,
                agent_session_value TEXT
            );

            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                type TEXT,
                spec_json TEXT,
                status TEXT,
                tz TEXT,
                next_run_at TEXT,
                expires_at TEXT,
                runs_count INT,
                created_at TEXT,
                ended_reason TEXT
            );

            CREATE TABLE IF NOT EXISTS turns (
                agent_id TEXT,
                n INT,
                prompt_paths TEXT,
                response_path TEXT,
                response_source TEXT,
                started_at TEXT,
                ended_at TEXT,
                tokens_in INT,
                tokens_out INT,
                PRIMARY KEY (agent_id, n)
            );

            CREATE TABLE IF NOT EXISTS events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT,
                kind TEXT,
                ref_id TEXT,
                payload_json TEXT
            );

            PRAGMA user_version = 1;
            ",
        )?;
        Ok(())
    }

    fn agent_ids_by_prefix(&self, prefix: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM agents WHERE id LIKE ?1 ORDER BY id")?;
        let rows = stmt.query_map([format!("{prefix}%")], |row| row.get(0))?;
        collect_rows(rows)
    }

    fn agent_ids_by_name(&self, name: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM agents WHERE name = ?1 ORDER BY id")?;
        let rows = stmt.query_map([name], |row| row.get(0))?;
        collect_rows(rows)
    }
}

const AGENT_SELECT_SQL: &str = "SELECT id, name, parent_id, kind, harness, model, effort, host,
    herdr_session, pane_id, terminal_id, cwd, worktree, status, exit_reason, keep, timeout_s,
    created_at, ended_at, run_dir, agent_session_kind, agent_session_value FROM agents";

const JOB_SELECT_SQL: &str = "SELECT id, type, spec_json, status, tz, next_run_at, expires_at,
    runs_count, created_at, ended_reason FROM jobs";

const TURN_SELECT_SQL: &str = "SELECT agent_id, n, prompt_paths, response_path, response_source,
    started_at, ended_at, tokens_in, tokens_out FROM turns";

fn bool_to_int(value: bool) -> i64 {
    i64::from(value)
}

fn int_to_bool(value: i64) -> bool {
    value != 0
}

fn collect_rows<T>(rows: impl Iterator<Item = rusqlite::Result<T>>) -> Result<Vec<T>> {
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn map_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRow> {
    Ok(AgentRow {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        kind: row.get(3)?,
        harness: row.get(4)?,
        model: row.get(5)?,
        effort: row.get(6)?,
        host: row.get(7)?,
        herdr_session: row.get(8)?,
        pane_id: row.get(9)?,
        terminal_id: row.get(10)?,
        cwd: row.get(11)?,
        worktree: row.get(12)?,
        status: row.get(13)?,
        exit_reason: row.get(14)?,
        keep: int_to_bool(row.get(15)?),
        timeout_s: row.get(16)?,
        created_at: row.get(17)?,
        ended_at: row.get(18)?,
        run_dir: row.get(19)?,
        agent_session_kind: row.get(20)?,
        agent_session_value: row.get(21)?,
    })
}

fn map_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok(JobRow {
        id: row.get(0)?,
        job_type: row.get(1)?,
        spec_json: row.get(2)?,
        status: row.get(3)?,
        tz: row.get(4)?,
        next_run_at: row.get(5)?,
        expires_at: row.get(6)?,
        runs_count: row.get(7)?,
        created_at: row.get(8)?,
        ended_reason: row.get(9)?,
    })
}

fn map_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRow> {
    Ok(TurnRow {
        agent_id: row.get(0)?,
        n: row.get(1)?,
        prompt_paths: row.get(2)?,
        response_path: row.get(3)?,
        response_source: row.get(4)?,
        started_at: row.get(5)?,
        ended_at: row.get(6)?,
        tokens_in: row.get(7)?,
        tokens_out: row.get(8)?,
    })
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        seq: row.get(0)?,
        ts: row.get(1)?,
        kind: row.get(2)?,
        ref_id: row.get(3)?,
        payload_json: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn agent_crud_and_resolution() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();
        let mut agent = AgentRow::new(
            Some("worker".to_string()),
            "tui",
            "claude",
            "2026-01-01T00:00:00Z",
            "/tmp/run",
        );
        agent.cwd = "/tmp".to_string();
        store.create_agent(&agent).unwrap();

        assert_eq!(store.get_agent(&agent.id).unwrap(), Some(agent.clone()));
        store
            .update_agent_status(
                &agent.id,
                "done",
                Some("completed"),
                Some("2026-01-01T00:00:01Z"),
            )
            .unwrap();

        let updated = store.get_agent(&agent.id).unwrap().unwrap();
        assert_eq!(updated.status, "done");
        assert_eq!(updated.exit_reason.as_deref(), Some("completed"));
        assert_eq!(store.list_agents().unwrap().len(), 1);
        assert_eq!(store.resolve_agent_id(&agent.id[..8]).unwrap(), agent.id);
        assert_eq!(store.resolve_agent_id("worker").unwrap(), agent.id);
    }

    #[test]
    fn jobs_turns_and_events_crud() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();

        let job = JobRow::new("loop", r#"{"prompt":"hi"}"#, "queued", "now");
        store.create_job(&job).unwrap();
        assert_eq!(store.get_job(&job.id).unwrap(), Some(job.clone()));
        assert_eq!(store.list_jobs().unwrap(), vec![job]);

        let mut turn = TurnRow::new("agent-1", 1, r#"["p"]"#, "/r", "start");
        store.create_turn(&turn).unwrap();
        turn.response_source = Some("file".to_string());
        turn.ended_at = Some("end".to_string());
        turn.tokens_in = Some(10);
        store.update_turn(&turn).unwrap();
        assert_eq!(store.list_turns_by_agent("agent-1").unwrap(), vec![turn]);

        let event = EventRow::new("ts", "kind", Some("ref".to_string()), "{}");
        let seq = store.append_event(&event).unwrap();
        let events = store.list_events().unwrap();
        assert_eq!(seq, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].seq, 1);
        assert_eq!(events[0].payload_json, "{}");
    }

    #[test]
    fn user_version_is_enforced() {
        let temp = tempdir().unwrap();
        let db_path = temp.path().join("orcr.db");
        let conn = Connection::open(&db_path).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        drop(conn);

        let error = Store::open(temp.path()).unwrap_err();
        assert!(error
            .to_string()
            .contains("unsupported sqlite user_version 2"));
    }

    #[test]
    fn id_prefix_ambiguous_and_not_found() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();

        let mut first = AgentRow::new(None, "tui", "mock", "now", "/r1");
        first.id = "aaaaaaaa-0000-4000-8000-000000000001".to_string();
        let mut second = AgentRow::new(None, "tui", "mock", "now", "/r2");
        second.id = "aaaaaaaa-0000-4000-8000-000000000002".to_string();
        store.create_agent(&first).unwrap();
        store.create_agent(&second).unwrap();

        let ambiguous = store.resolve_agent_id("aaaaaaaa").unwrap_err();
        assert!(ambiguous.to_string().contains("ambiguous agent id prefix"));

        let missing = store.resolve_agent_id("missing").unwrap_err();
        assert!(missing.to_string().contains("agent not found"));
    }

    #[test]
    fn name_resolution_can_be_ambiguous() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path()).unwrap();

        let first = AgentRow::new(Some("same".to_string()), "tui", "mock", "now", "/r1");
        let second = AgentRow::new(Some("same".to_string()), "tui", "mock", "now", "/r2");
        store.create_agent(&first).unwrap();
        store.create_agent(&second).unwrap();

        let error = store.resolve_agent_id("same").unwrap_err();
        assert!(error.to_string().contains("ambiguous agent name"));
    }
}
