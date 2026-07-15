//! The sqlite schema, verbatim. Nothing derivable is stored; payloads live
//! as files in the data dirs — sqlite coordinates, files carry content.

/// The current store schema version. Two orcr versions sharing one store must agree on
/// this; a mismatch is refused with a message on version skew.
pub const SCHEMA_VERSION: i64 = 1;

/// Full DDL. Every statement is idempotent (`IF NOT EXISTS`) so init is safe to re-run.
pub const SCHEMA_SQL: &str = r#"
-- meta: schema version stamp and other single-row bookkeeping.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- agents: uuid is the permanent identity; path is the address (last segment = name).
CREATE TABLE IF NOT EXISTS agents (
    uuid                  TEXT PRIMARY KEY,          -- UUIDv7
    path                  TEXT NOT NULL,             -- absolute; last segment = name
    managed               INTEGER NOT NULL,          -- 0|1
    origin                TEXT NOT NULL,             -- run|detected
    parent_id             TEXT,                      -- uuid of spawning context
    agent                 TEXT,                      -- provider (claude|codex|...)
    model                 TEXT,
    effort                TEXT,
    gc_mode               TEXT,                      -- auto|immediate|never
    cwd                   TEXT,
    herdr_session         TEXT,
    terminal_id           TEXT,                      -- current location, not identity
    pane_id               TEXT,
    launch_token          TEXT,                      -- crash-recovery idempotency marker
    agent_session_kind    TEXT,                      -- id|path (transcript identity gate)
    agent_session_value   TEXT,
    status                TEXT NOT NULL,             -- lifecycle status vocabulary
    move_state            TEXT NOT NULL DEFAULT 'none', -- none|parking|unparking
    move_token            TEXT,
    blocked_kind          TEXT,                      -- question|limit|login|unknown
    input_seq             INTEGER NOT NULL DEFAULT 0,
    cancel_requested      INTEGER NOT NULL DEFAULT 0,
    exit_reason           TEXT,                      -- completed|killed|canceled|reaped|timeout|failed|lost
    transcript_locator    TEXT,
    transcript_cursor     TEXT,
    queue_seq             INTEGER,
    enqueued_at           INTEGER,
    starting_at           INTEGER,
    deadline_at           INTEGER,                   -- only if --timeout
    idle_since            INTEGER,
    parked_at             INTEGER,
    last_status_change_at INTEGER,
    created_at            INTEGER NOT NULL,
    ended_at             INTEGER,
    updated_at            INTEGER NOT NULL
);

-- path reservation: a path is unique among ACTIVE agents (any non-ended status).
-- ended paths are reusable; the uuid is what stays unique forever.
CREATE UNIQUE INDEX IF NOT EXISTS agents_active_path
    ON agents(path) WHERE status NOT IN ('ended');

CREATE INDEX IF NOT EXISTS agents_status_queue    ON agents(status, queue_seq);
CREATE INDEX IF NOT EXISTS agents_provider_status ON agents(agent, status);
CREATE INDEX IF NOT EXISTS agents_path            ON agents(path);
CREATE INDEX IF NOT EXISTS agents_parent          ON agents(parent_id);
CREATE INDEX IF NOT EXISTS agents_pane            ON agents(pane_id);
CREATE INDEX IF NOT EXISTS agents_session_term    ON agents(herdr_session, terminal_id);
CREATE INDEX IF NOT EXISTS agents_agent_session
    ON agents(agent_session_kind, agent_session_value);

-- turns: one row per delivered input; the completion bookkeeping.
CREATE TABLE IF NOT EXISTS turns (
    agent_uuid        TEXT NOT NULL,
    input_seq         INTEGER NOT NULL,
    source            TEXT NOT NULL,            -- orcr|external
    delivered_at      INTEGER,
    working_seen_at   INTEGER,
    completed_at      INTEGER,
    blocked_kind      TEXT,
    transcript_cursor TEXT,
    PRIMARY KEY (agent_uuid, input_seq)
);

-- attaches: attach leases; the GC interlock that survives restarts.
CREATE TABLE IF NOT EXISTS attaches (
    agent_uuid   TEXT NOT NULL,
    lease_id     TEXT PRIMARY KEY,
    mode         TEXT NOT NULL,                 -- observe|takeover
    connection   TEXT,
    client_pid   INTEGER,
    started_at   INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attaches_agent ON attaches(agent_uuid);

-- loops: durable cron definitions. uuid is permanent; name is unique among active/paused.
CREATE TABLE IF NOT EXISTS loops (
    uuid            TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    cadence_kind    TEXT NOT NULL,             -- cron|once
    cadence_value   TEXT NOT NULL,
    tz              TEXT NOT NULL,
    cwd             TEXT NOT NULL,
    max_concurrency INTEGER NOT NULL,
    overlap         TEXT NOT NULL,             -- queue|skip
    timeout_s       INTEGER,
    status          TEXT NOT NULL,             -- active|paused|ended
    next_fire_at    INTEGER,
    last_fire_at    INTEGER,
    ended_reason    TEXT,                      -- removed|removed_by_run|fired
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS loops_active_name
    ON loops(name) WHERE status IN ('active','paused');
CREATE INDEX IF NOT EXISTS loops_status_fire ON loops(status, next_fire_at);

-- loop_runs: every run is a durable row from the moment it is asked for.
CREATE TABLE IF NOT EXISTS loop_runs (
    uuid            TEXT PRIMARY KEY,
    loop_uuid       TEXT NOT NULL,
    run_id          TEXT NOT NULL,            -- r + 5 [a-z0-9]; unique per loop
    kind            TEXT NOT NULL,            -- scheduled|manual
    due_at          INTEGER,
    status          TEXT NOT NULL,            -- pending|running|stopping|ok|failed|timeout|stopped|canceled
    pid             INTEGER,
    pgid            INTEGER,
    pgid_start_time INTEGER,                  -- signal only on start-time match
    exit_code       INTEGER,
    signal          INTEGER,
    timeout_at      INTEGER,
    started_at      INTEGER,
    ended_at        INTEGER,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS loop_runs_run_id ON loop_runs(loop_uuid, run_id);
CREATE INDEX IF NOT EXISTS loop_runs_loop_status ON loop_runs(loop_uuid, status);

-- events: the subscription cursor; written in the same txn as the change it describes.
CREATE TABLE IF NOT EXISTS events (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    ref_uuid     TEXT,
    payload_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_ref ON events(ref_uuid, seq);
"#;
