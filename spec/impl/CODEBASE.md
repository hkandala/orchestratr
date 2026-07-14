# orchestratr codebase map (living)

A concise, cumulative map of the code as it exists **right now**, so implementers don't
have to re-read the whole source tree to get oriented. **Read this first** to understand
the current layout, then open only the specific files you need to touch (their exact
signatures live in the source, not here). **Every milestone's scribe updates this file**
to reflect what that milestone added/changed.

> This is a map, not a mirror — for exact function signatures, open the file. For the
> *why* behind decisions, read the per-milestone `notes.md` files (especially the herdr
> facts in `m0-foundations/notes.md`, which are load-bearing for the driver).

Current state: **through M2 (agent core).**

## Crate & binaries

- Crate `orchestratr` (lib at `src/lib.rs`), edition 2021, rust 1.89, `default-run = "orcr"`.
- Binaries: `orcr` (`src/bin/orcr.rs`) and `orcr-mock-agent` (`src/bin/orcr-mock-agent.rs`).
- `orcr` runs a **clap** CLI (`src/cli.rs`): M1 wires the `server` and `api` nouns; the
  hidden `__m0-selfcheck` subcommand is still routed before clap in `src/bin/orcr.rs`.
  agent/loop/top nouns land in later milestones (their socket methods are already
  registered in `src/api.rs`).
- Deps in use (through M1): `anyhow`, `thiserror` (v1), `serde`/`serde_json`, `rusqlite`
  (bundled, WAL), `uuid` (v4 + v7), `dirs`, `libc`, `chrono` (clock+std), `clap` (derive),
  `signal-hook`. dev: `tempfile`, `jsonschema` (schema validity test), `assert_cmd`,
  `predicates`. Add new deps as milestones need them (cron/chrono-tz for loops in M5,
  ratatui/crossterm for top in M6, etc.).
- **Server runtime is threaded/blocking, not tokio** (decided in M1 — see
  `m1-server-protocol/notes.md`): `Mutex<Store>` single writer + one thread per connection
  + one pump thread per subscription. orcr's own socket protocol version is
  `wire::ORCR_PROTOCOL` (currently 1), distinct from herdr's protocol 16.

## Modules (`src/`)

- `error.rs` — `OrcrError`, `ErrorCode` (the §13 error enum), `Result<T>`. Re-exported
  from the crate root. **All fallible code returns `Result`; map failures to the right
  `ErrorCode`.** This is the single source of truth for error codes + exit mapping.
- `home.rs` — `ORCR_HOME` resolution (`$ORCR_HOME` override → `~/.orcr`), the directory
  layout (store, `logs/`, `data/`, socket path, lock file), and the **safety check**
  (`unsafe_home` unless owned by current uid and not group/world-writable). Everything
  path-related about the orcr home lives here.
- `config.rs` — `~/.orcr/config.json` load + strict validation (§14): unknown keys warn
  with a nearest-name suggestion (Levenshtein), durations require units & must be
  positive, `concurrency.max ≥ 1`, per-provider caps clamped. Defaults built in.
- `duration.rs` — human duration parsing/formatting (`45s`, `20m`, `3h`); units required.
- `path.rs` — **the §5.1 grammar in one place**: segment/path validation, depth+reserved
  checks, `{rand}` expansion, `--name`/`--path` scope resolution (`resolve_create`), selector
  resolution, and the glob `Pattern` (`*` one segment, `**` any depth ≥1, anchored). Plus
  derived helpers (`name_of`, `scope_of_agent`, `home_workspace`, `tab_label`). Pure logic,
  heavily unit-tested; every surface derives from it (no ad-hoc string/LIKE matching).
- `wire.rs` — **orcr's own** socket wire protocol (§11.6): request/response/event envelopes,
  newline-delimited JSON framing (`read_frame`/`write_frame`, `MAX_FRAME` enforced),
  `ORCR_PROTOCOL`, `unsupported_version`. Transport-agnostic (server, client, tests share it).
- `api.rs` — the **method registry** (single source of the socket API): `methods()` lists
  every method (name, summary, params/result JSON-Schema fragments, `implemented`,
  `streaming`); `schema_document()` generates the versioned `api schema`. Live in M1:
  server.handshake/status/stop, api.schema/snapshot, events.subscribe, watch.open. All
  agent.*/loop.* registered as stubs. Also `EVENT_KINDS`, `ERROR_CODES`.
- `events.rs` — `EventBus` (mutex+condvar): wakeups for subscriber pumps + retention
  bookkeeping (`oldest_retained_seq` → `cursor_expired`, `is_expired`). The durable cursor
  is the `events` table; the bus is just the in-memory coordination layer.
- `lock.rs` — `InstanceLock`: exclusive `flock` guard on `orcr.lock` (single-instance;
  released on process exit, incl. `kill -9`).
- `cli.rs` — the clap CLI: `agent {run|send|kill|ls}` (M2), `server {…}`, `api {…}`, the
  `--json` envelope, §13 error→exit-code mapping, TTY detection, log tail/follow. Agent verbs
  forward the caller's `ORCR_ID`/`ORCR_PATH` (lineage+scope), resolve `-p -`/positional `-`
  from stdin, default `--cwd` to the caller's cwd, print `<path> <uuid>` + TTY hints, and do
  the kill TTY confirmation via a `preview` round-trip (`-y` skips).
- `server/` — the single-writer server process (§4, §11.6).
  - `mod.rs` — `run_foreground` (lock race → bind socket → open store → **reconcile** →
    **start queue worker** → serve), the threaded accept/dispatch loop, subscription pumps,
    `server.status`/`api.snapshot` builders (snapshot now carries real agents + queue),
    graceful stop, `emit_event`/`publish` (append + wake bus + trim), and `agent_row_json`
    (the flat §6.1 row). **Add new method handlers in `handle_request`** (M2 routes
    `agent.run/send/kill/ls` to `engine.rs`). `ServerInner` now also holds `home`, a cached
    owned-session `driver`, and a `spawn_lock`.
  - `engine.rs` — **the M2 agent engine**: the owned-session driver (`owned_driver`,
    lazy-connect+cache), the **queue worker** thread (FIFO promotion + stuck-start sweep,
    `QUEUE_TICK`), the **spawn pipeline** (`run_pipeline`: ensure workspace → `agent.start`
    → record location → capture `agent_session` → two-call first prompt → `working`, with
    `cancel_requested` checks between steps), start-up **reconciliation** (`reconcile_on_start`
    — confirm/repair/lost/orphan-close), and the `agent.run/send/kill/ls` handlers +
    resolution helpers (path-first then uuid-prefix). `LaunchPayload` = the `launch.json`
    shape (§12).
  - `client.rs` — `Client`: connect+handshake (version-checked), one-shot `request`,
    `open_stream`+`Subscription` for event streams, and `ensure_running` (auto-start: spawn
    a detached `server start --foreground`, wait for readiness). `StartOutcome`.
  - `log.rs` — `ServerLog`: JSON-per-line, size-capped rotation to `server.log.N`.
- `store/` — sqlite (WAL), single-writer.
  - `schema.rs` — the full §12 schema (`agents`, `turns`, `attaches`, `loops`,
    `loop_runs`, `events` + all indexes incl. the partial unique path index) and a `meta`
    table stamping `schema_version` (mismatch → `store_version_mismatch` refusal).
  - `mod.rs` — the typed data-access layer; **all writes go through `BEGIN IMMEDIATE`
    transaction helpers**. M1 events layer: `append_event`/`append_event_tx`, `events_since`,
    `latest/oldest_event_seq`, `trim_events`. **M2 agent DAL**: `enqueue_agent` (durable row +
    `queue_seq` + `deadline_at` from `--timeout` + `agent.created`, path-in-use →
    `state_conflict`), `promote_queued` (FIFO
    global+per-provider promotion in one txn), `agent_full`/`AgentFull`, resolution
    (`find_by_path` → `Resolution` active-first-else-latest-ended; `find_by_uuid_or_prefix` →
    `UuidLookup`), `record_location`/`record_agent_session`, `transition_status`,
    `request_cancel`/`is_cancel_requested`, `bump_input_seq` (+turn row), `list_agents`
    (`AgentFilter`; glob applied in Rust, never SQL LIKE), `queue_position`, `stuck_starting`,
    `active_managed_agents`. Store methods that write events append them in-txn and return the
    seq; the server calls `publish(seq)`.
- `driver/` — the herdr socket driver (see `m0-foundations/notes.md` for the verified
  wire facts; **the driver is the riskiest surface — trust the notes**).
  - `protocol.rs` — wire envelopes: request `{protocol,id,method,params}`; success
    `{id, result:{type:"<tag>", ...}}` (tagged union on `type`); error `{id, error:{code,
    message}}`. Newline-delimited JSON. Typed request params + result structs
    (AgentInfo/PaneInfo/WorkspaceInfo, agent_status enum idle|working|blocked|done|
    unknown, etc.).
  - `mod.rs` — the `Driver`: **synchronous/blocking**, **one request per connection**
    (herdr closes the socket after each response — open a fresh `UnixStream` per call,
    with read/write timeouts). Typed methods wrap each herdr op (ping/handshake,
    session.snapshot, workspace.create/list, agent.start/list, pane.get/list/move/close/
    send_text/send_keys, notification.show). **Version handshake is orcr-side**: ping,
    read `protocol` from `pong`, reject mismatch. NOTE: M1's socket *server* + event
    stream (§11.6) is a separate concern — the async story was deferred from M0; decide
    the server's runtime (tokio vs threaded) in M1 and record it.
  - `session.rs` — owned-session bootstrap: start the owned session's herdr server
    headless via the binary (`herdr --session <name> server`, spawned detached), discover
    its `socket_path` via `herdr session list --json`, connect the driver to *that*
    socket. **Sessions are per-socket** (major herdr fact — cross-session work fans out
    over each session's socket).
  - `integration.rs` — `IntegrationState` (herdr-layer parse) **plus** the orcr-side
    integration (M2): `launch_plan(provider, model, effort)` → argv (bypass flags +
    model/effort mapping) for claude/codex (and the test-only `mock` provider under
    `ORCR_ALLOW_MOCK_PROVIDER=1`), and `ensure_supported` (both-layers-required →
    `integration_missing` naming the missing layer + install command, §11.4).
  - `contract.rs` — the driver conformance table (§11.7) pinned to named herdr methods
    with fixed shapes; checked against a fixture generated from live `herdr api schema`
    (`tests/conformance_live.rs`), so herdr version drift fails CI.

## Binaries

- `src/bin/orcr.rs` — the entrypoint: routes `__m0-selfcheck` (hidden) else `cli::run()`.
- `src/bin/orcr-mock-agent.rs` — the **mock provider**: a scriptable fake agent TUI that
  self-discovers its herdr pane from injected env (`HERDR_SOCKET_PATH`, `HERDR_PANE_ID`)
  and reports its own state to herdr via `pane.report_agent` (state = idle|working|
  blocked|unknown, + optional transcript pointer). This is the workhorse for all e2e
  suites — use it instead of real providers in automated tests. Env knobs include
  `ORCR_MOCK_NO_REPORT` (suppress self-reporting when the test drives state elsewhere). M2:
  it also **dumps its `ORCR_*` env to `$ORCR_AGENT_DATA_DIR/mock_env.json`** (how e2e asserts
  the §5.3 env contract reached the pane) and defaults its reported `agent_session` id so the
  spawn pipeline's session-capture returns promptly. Spawned as provider `mock` via
  `$ORCR_MOCK_AGENT_BIN` when the server runs with `ORCR_ALLOW_MOCK_PROVIDER=1`.

## Tests & the e2e harness

- Unit + lightweight tests run by default (fast): `handshake.rs`, `home_config.rs`,
  `server_protocol.rs` (M1 acceptance — no herdr needed), in-crate `#[cfg(test)]` modules.
- `tests/server_protocol.rs` spawns the real `orcr` binary over a throwaway `ORCR_HOME` to
  prove: the auto-start race → one server; `kill -9` → clean restart + intact store;
  `api schema` is valid JSON Schema with 100% method coverage; subscription replay/live has
  no gaps/dups and `cursor_expired` → re-snapshot; `server logs --follow` streams live.
- **e2e tests are gated behind `ORCR_E2E=1`** (so `cargo test` stays fast). Run them
  with `ORCR_E2E=1 cargo test --test e2e` (driver/harness) and
  `ORCR_E2E=1 cargo test --test agent_e2e -- --test-threads=1` (M2 agent core). They
  exercise real behavior against **live herdr** using the mock provider.
- `tests/agent_e2e.rs` (M2) runs a real `orcr` server over a throwaway `ORCR_HOME` +
  disposable session (`TestServer` harness, drop-guard teardown) and proves the M2
  acceptance: run/send/kill lifecycle + env contract, 50-at-cap-5 FIFO/never-over-cap/drain,
  concurrent same-path one-winner, kill-during-starting, idle-held-at-working,
  integration-missing, crash-recovery (repair running + close orphan), and the path-model
  conformance table over socket **and** CLI.
- `tests/conformance_live.rs` diffs the pinned driver contract against live
  `herdr api schema` (guards herdr version drift).
- **e2e safety pattern (MANDATORY, reuse it):** each e2e test creates a throwaway
  `ORCR_HOME` tempdir and a **disposable** herdr session named `orcr_test_<rand>`
  (rand from UUIDv4 — UUIDv7's timestamp prefix collides across near-simultaneous
  tests), and tears it down in a **drop guard** (`herdr session stop <name>` +
  `herdr session delete <name>`). Never touch the user's `default` session; never use
  `~/.orcr`. Look at `tests/e2e.rs` for the existing harness helpers and copy the
  pattern.

## Conventions

- Match the spec verbatim where it is precise (grammar, status vocabulary, error/exit
  codes, JSON shapes, env contract, store schema). Where silent, choose the simplest
  correct behavior and record it in the milestone `notes.md`.
- Single writer: the server owns the store; all writes are `BEGIN IMMEDIATE`
  transactions; events (M1+) are written in the same transaction as the change.
- `cargo fmt` + `cargo clippy --all-targets -- -D warnings` must stay clean.
- Commit in small, focused commits on `main` (one module + tests, one verb, one fix).

## How to extend (quick pointers)

- New error condition → add/adjust `ErrorCode` in `error.rs` (keep the §13 enum small;
  detail goes in `details`).
- New store data → add schema in `store/schema.rs` (bump `schema_version` only if the
  on-disk schema changes incompatibly) + typed access in `store/mod.rs`.
- New herdr op → add typed params/result in `driver/protocol.rs`, a method in
  `driver/mod.rs`, and pin it in `driver/contract.rs` + the conformance fixture.
- New provider integration → `driver/integration.rs` (both-layers-required per §11.4).
- New socket method → register it in `src/api.rs` `methods()` (params/result schema,
  `implemented`) and add its live handler in `server/mod.rs` `handle_request` (flip the
  stub). The CLI verb in `src/cli.rs` calls it via `Client::request`/`open_stream`.
- New event kind → add to `EVENT_KINDS` in `src/api.rs`; producers write it with
  `store::append_event_tx` in the same txn as the change, then `Server::emit_event`
  publishes + trims (or call `emit_event` directly for out-of-txn cases).
- New e2e → copy the disposable-home + disposable-session harness in `tests/`.
