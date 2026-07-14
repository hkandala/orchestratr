# orchestratr codebase map (living)

A concise, cumulative map of the code as it exists **right now**, so implementers don't
have to re-read the whole source tree to get oriented. **Read this first** to understand
the current layout, then open only the specific files you need to touch (their exact
signatures live in the source, not here). **Every milestone's scribe updates this file**
to reflect what that milestone added/changed.

> This is a map, not a mirror — for exact function signatures, open the file. For the
> *why* behind decisions, read the per-milestone `notes.md` files (especially the herdr
> facts in `m0-foundations/notes.md`, which are load-bearing for the driver).

Current state: **through M7 (SDK & skill).**

## Crate & binaries

- Crate `orchestratr` (lib at `src/lib.rs`), edition 2021, rust 1.89, `default-run = "orcr"`.
- Binaries: `orcr` (`src/bin/orcr.rs`) and `orcr-mock-agent` (`src/bin/orcr-mock-agent.rs`).
- `orcr` runs a **clap** CLI (`src/cli.rs`): M1 wires the `server` and `api` nouns; the
  hidden `__m0-selfcheck` subcommand is still routed before clap in `src/bin/orcr.rs`.
  agent/loop/top nouns land in later milestones (their socket methods are already
  registered in `src/api.rs`).
- Deps in use (through M1): `anyhow`, `thiserror` (v1), `serde`/`serde_json`, `rusqlite`
  (bundled, WAL), `uuid` (v4 + v7), `dirs`, `libc`, `chrono` (clock+std), `clap` (derive),
  `signal-hook`. **M5** adds `chrono-tz` + `iana-time-zone` (DST-correct cron; cron is a
  hand-rolled evaluator in `cron.rs`, no cron crate). dev: `tempfile`, `jsonschema` (schema
  validity test), `assert_cmd`, `predicates`. Add new deps as milestones need them
  (ratatui/crossterm for top in M6, etc.).
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
  positive, `concurrency.max ≥ 1`, per-provider caps clamped. Defaults built in. **M3**:
  optional `integrations.<provider>.*` completion-tuning overrides (`IntegrationTuning`, ms).
  **M5** adds `timings.loop_tick` (scheduler tick, default 1s) + `timings.run_term_grace`
  (run TERM→KILL grace, default 10s).
- `duration.rs` — human duration parsing/formatting (`45s`, `20m`, `3h`); units required.
- `cron.rs` — **M5 own cron evaluator** (§6.2/§11.3): five-field `min hour dom month dow`
  (`*`/`a`/`a-b`/`a,b`/`*/n`/`a-b/n`, dow 0-6 with 0/7=Sun, dom+dow-restricted → OR).
  `Cron::parse`/`next_after(after, tz)` steps wall-clock minutes **in the creating tz** and
  converts each candidate to UTC → trivially DST-correct (spring-forward gaps skipped,
  fall-back folds take earliest), bounded to a 4-year search. Plus `local_tz_name`
  (iana-time-zone), `tz_from_name` (chrono-tz), and `describe`/`describe_next_fire` (cadence
  in words + human local+UTC next fire for the create echo). Pure logic, unit-tested incl. the
  DST acceptance over both 2026 US transitions.
- `service.rs` — **M5 `server enable/disable`** (§6.4), CLI-side (no server needed):
  `launchd_plist`/`systemd_unit` build the platform unit (macOS `dev.orchestratr.orcr`
  RunAtLoad+KeepAlive; Linux `orcr.service` Restart=on-failure) with the **absolute binary
  path** and propagated `ORCR_HOME`/`ORCR_HERDR_BIN` + redirected logs; `build_unit` picks the
  platform (`unsupported_platform` elsewhere). `enable`/`disable` write/remove the unit and
  best-effort `launchctl`/`systemctl` load (so headless CI still gets the durable file). Golden
  unit-file tests assert content.
- `scaffold.rs` — **the M7 `orcr scaffold`** (§6.6): generates exactly three files
  (`package.json` pinning `@orchestratr/sdk` to the CLI version + `tsx`/`typescript`,
  `tsconfig.json`, a ~15-line `workflow.ts`) into `<dir>` then runs `npm install`. Preflight
  Node ≥ 20 + npm (else `environment_error`, **nothing created**); never overwrites (any of the
  three present → `state_conflict`); purely local (no server). `ORCR_SDK_SPEC` overrides the dep
  spec for offline/local installs (else the pinned CLI version). Unit-tested; e2e in `recipe_e2e`.
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
  server.handshake/status/stop, api.schema/snapshot, events.subscribe, watch.open, all
  `agent.*` (M2–M4: run/ask/send/logs/wait/kill/ls + attach.prepare/heartbeat/release).
  `loop.*` (M5: create/pause/resume/rm/ls/logs + run.start/run.stop/run.ls, now
  `implemented`). Also `EVENT_KINDS` (M5 adds `loop.created/fired/coalesced/skipped/paused/
  resumed/removed/ended` + `loop_run.started/ended/stopping`), `ERROR_CODES`. `server.status`
  + `api.snapshot` schemas carry `loops_firing` + `loops`.
- `events.rs` — `EventBus` (mutex+condvar): wakeups for subscriber pumps + retention
  bookkeeping (`oldest_retained_seq` → `cursor_expired`, `is_expired`). The durable cursor
  is the `events` table; the bus is just the in-memory coordination layer.
- `lock.rs` — `InstanceLock`: exclusive `flock` guard on `orcr.lock` (single-instance;
  released on process exit, incl. `kill -9`).
- `cli.rs` — the clap CLI: `agent {run|send|kill|ls}` (M2) + `agent {ask|wait|logs}` (M3:
  `wait` computes the §6.1 exit code and `process::exit`s; `logs --follow` polls; `ask`
  prints the response on stdout) + `agent attach` (M4: prepare → background heartbeat thread →
  exec interactive `herdr agent attach` → release on exit), `server {…}`, `api {…}`, the
  `--json` envelope, §13 error→exit-code mapping, TTY detection, log tail/follow. Agent verbs
  forward the caller's `ORCR_ID`/`ORCR_PATH` (lineage+scope), resolve `-p -`/positional `-`
  from stdin, default `--cwd` to the caller's cwd, print `<path> <uuid>` + TTY hints, and do
  the kill TTY confirmation via a `preview` round-trip (`-y` skips). **M5** adds the `loop`
  noun (`create` — echoes parsed argv + cadence-in-words + local/UTC next fire + cancel cmd —
  `pause|resume|rm|ls|logs` + the `loop run {start|stop|ls}` sub-noun, with TTY confirms on
  `rm`/`run stop` and `logs --follow` re-poll) and `server enable|disable` (calls `service.rs`
  directly, no server).
- `server/` — the single-writer server process (§4, §11.6).
  - `mod.rs` — `run_foreground` (lock race → bind socket → open store → **reconcile** →
    **start queue worker** → **recover loops** → **start loop scheduler** → serve), the threaded
    accept/dispatch loop, subscription pumps, `server.status`/`api.snapshot` builders (snapshot
    carries real agents + queue + loops via `loops::loop_row_json`; `handle_request` routes the
    `loop.*` methods to `loops.rs`),
    graceful stop, `emit_event`/`publish` (append + wake bus + trim), and `agent_row_json`
    (the flat §6.1 row). **Add new method handlers in `handle_request`** (M2 routes
    `agent.run/send/kill/ls` to `engine.rs`). `ServerInner` now also holds `home`, a cached
    owned-session `driver`, and a `spawn_lock`.
  - `gc.rs` — **the M4 GC engine + reconciliation** (§5.4, §11.2, §11.5): one thread ticks
    every `timings.gc_tick` and (a) expires stale attach leases, (b) enforces explicit
    `--timeout` (`exit_reason: timeout`, all gc modes), (c) **parks** idle-past-`idle_after`
    `gc auto` agents (two-phase: `begin_move` CAS → `pane.move` to the `idle` workspace →
    `finish_park`), (d) **reaps** parked-past-`kill_after` agents (`exit_reason: reaped` +
    pane closed), (e) runs `periodic_reconcile` (recover half-done moves by `terminal_id`,
    resolve already-`lost` agents, mark newly-vanished panes `lost`, refresh drift counts).
    Also `unpark_for_send` (two-phase move back to the home workspace, called by `agent.send`
    before delivery), the attach-lease GC interlock (`lease_fresh`), the `DriftSnapshot`
    (surfaced in `server status`), and the test-only `ORCR_TEST_PARK_CRASH` fault hook.
  - `loops.rs` — **the M5 loop scheduler + `loop`/`loop run` handlers** (§6.2, §11.3):
    `start_loop_scheduler` (tick thread every `timings.loop_tick`: fire due loops, coalesce/skip
    per `--overlap`, honor pause, enforce per-run `--timeout`, promote oldest pending when a slot
    frees). `fire_loop` allocates a run row transactionally (uuid + `run_id` + `due_at`, pending
    at cap) and spawns via `setsid` (own process group) with the §5.3 env contract
    (`ORCR_ID`=run uuid, `ORCR_PATH`=`<loop>/<run_id>`, `ORCR_LOOP_DATA_DIR`), cwd = loop
    creation cwd, stdin `/dev/null`, stdout/stderr captured to a line-tagged rotated `run.log`
    JSONL (`{ts,stream,text}` + sidecar). Process-group **identity guard** (record pid/pgid +
    OS start time; signal/recover only a live pgid whose start time still matches — pid-reuse
    safe). `stop_run_process` = `enter_stop_barrier` (fast, sets `stopping`) + `finish_stop`
    (TERM → `run_term_grace` → KILL → `glob_kill_run_agents` `<loop>/<run_id>/**` until clean).
    `recover_loops_on_start` (per-loop txn: verify pgids by start time → close dead runs +
    glob-kill their agents → recompute active → honor paused/ended → decide pending fire once →
    recompute `next_fire`, cron missed fires skipped-and-logged, never replayed). Also
    `compute_next_fire` (via `cron`), `loop_data_dir`/`read_loop_payload` (the `loop.json`
    `LoopPayload`), the `handle_loop_*` verb handlers, and `loop_row_json` (shared with the
    snapshot builder). Namespace protection + run-scope live in `engine.rs`
    (`check_loop_namespace`, `caller_context`).
  - `discovery.rs` — **the M4 unmanaged-discovery poller** (§5.7): a 3s-tick thread that
    fans out over non-owned herdr sessions (per-socket), upserts supported-provider agents as
    read-only `unmanaged` rows keyed by (session, `terminal_id`) with path
    `unmanaged/<slug>/<slug>`, and ends rows whose terminal vanished. `ORCR_DISABLE_DISCOVERY=1`
    suppresses it (deterministic non-M4 tests).
  - `completion.rs` — **the M3 completion monitor**: a background thread (200ms tick) that
    polls the owned session's herdr `agent.list` and drives each monitorable agent's turn
    state machine — verified idle (working-after-delivery or fast-turn grace → stable idle →
    transcript settled → `working→idle`), external-turn detection (herdr `working` with no
    open turn → synthetic `source=external` turn), blocked tracking (a stale `blocked` from a
    prior turn never re-blocks a freshly re-armed turn — same working-seen/grace guard as idle),
    transcript capture, and
    `gc immediate` teardown (`working→ended(completed)` with no transient public idle). Also
    `agent_transcript` (locate via the provider adapter). Reads tuning from `driver::tuning_for`.
  - `engine.rs` — **the M2 agent engine + M3 wait/ask/logs**: the owned-session driver (`owned_driver`,
    lazy-connect+cache), the **queue worker** thread (FIFO promotion + stuck-start sweep,
    `QUEUE_TICK`), the **spawn pipeline** (`run_pipeline`: ensure workspace → `agent.start`
    → record location → capture `agent_session` → two-call first prompt → `working`, with
    `cancel_requested` checks between steps), start-up **reconciliation** (`reconcile_on_start`
    — confirm/repair/lost/orphan-close), and the `agent.run/send/kill/ls` handlers +
    resolution helpers (path-first then uuid-prefix). `LaunchPayload` = the `launch.json`
    shape (§12). **M3** adds `handle_agent_wait` (snapshot-then-subscribe on the event bus,
    `settle_of`/`next_hint`/`wait_result` → §6.1 reason tokens + structured `next` +
    `decision_seq`), `handle_agent_ask` (run --gc immediate → wait → last-response),
    `handle_agent_logs` (transcript entries / `--last-response`, both-layers gate); `send` now
    re-arms to `working` via `deliver_input`. **M4** adds the attach handlers
    (`handle_agent_attach_prepare`/`heartbeat`/`release`); `send` calls `unpark_for_send`
    (from `gc.rs`) before delivery for a parked/mid-move target; `reconcile_on_start` skips
    move-in-flight agents and calls `reconcile_moves_on_start`.
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
    `active_managed_agents`. **M3 turn/completion DAL**: `latest_turn`/`TurnRow`, `deliver_input`
    (bump input_seq + open turn + re-arm working), `open_external_turn`, `set_working_seen`,
    `set_idle_since`, `complete_turn` (→idle) / `complete_turn_row` (gc-immediate, no status flip),
    `mark_blocked`/`mark_working`, `record_capture`, `monitorable_agents`; `AgentFull` gained
    `idle_since`. **M4 GC/attach/reconcile/unmanaged DAL**: `park_candidates`/`reap_candidates`/
    `timed_out_agents`, the two-phase move CAS (`begin_move`/`finish_park`/`finish_unpark`/
    `rollback_move`/`agents_in_move`), `lost_agents`, attach leases
    (`prepare_attach`→`AttachInfo`/`heartbeat_lease`/`release_lease`/`has_fresh_lease`/
    `expire_leases`), unmanaged upsert (`find_unmanaged`/`insert_unmanaged`/`update_unmanaged`/
    `active_unmanaged`/`path_active`), and `rearm_idle_clocks_on_restart` (replaces M3's
    `clear_active_idle_since`: idle→park-clock-reset, working/blocked→clear). `AgentFull` gained
    `move_state`/`move_token`/`parked_at`. `debug_delete_agent` (test-only, behind the debug gate).
    **M5 loop/run DAL**: `create_loop` (`NewLoop`; unique among active/paused) + `loop.created`,
    `find_loop_by_name` (active-first-else-latest-ended)/`loop_by_uuid`/`list_loops`/`all_loops`,
    `active_loop_names` (namespace protection), `set_loop_status` (pause/resume/end with
    `removed`/`removed_by_run`/`fired` + events), `loops_due`/`set_next_fire`/`set_last_fire`;
    runs: `allocate_run` (→`RunAllocation::{Allocated{start_now},Coalesced,Skipped}`, fresh row
    always emits `loop.fired`, coalesce folds an existing pending scheduled run),
    `claim_pending_run(loop,max)` (atomic slot reservation — counts active + flips oldest
    pending→running in one `BEGIN IMMEDIATE`, concurrency-safe promotion), `record_run_start`
    (fills pid/pgid/start-time `WHERE status IN ('running','stopping')` — never clobbers the stop
    barrier), `finish_run`/`set_run_stopping`/`cancel_pending_run`, `run_by_id_or_uuid`/
    `run_by_uuid`/`runs_for_loop`/`active_runs`/`oldest_pending_run`/`runs_in_status`/
    `timed_out_runs` (`LoopRow`/`LoopRunRow`). Also `events_for_refs(&refs)` (index-scoped event
    fetch for `loop logs`, not a full `events` scan).
    Store methods that write events append them in-txn and return the seq; the server calls `publish(seq)`.
- `top/` — **the M6 monitoring TUI** (`orcr top`, §6.3, §7). A view-only dashboard; all
  acting stays in the CLI.
  - `model.rs` — the **pure, unit-tested core**: `Snapshot::from_json` parses an
    `api.snapshot`/`watch.open` doc into typed `SnapAgent`/`SnapLoop`/`SnapRun`; `TopFilter`
    (pattern/provider/status/managed/loops_only) whose `agent_matches` mirrors the `agent ls`
    store filter **byte-for-byte** (so the tree's agent node set == the equivalent `ls`);
    `build_tree` builds the §5.1 **path tree** (level-1 segments as top nodes, loops+active
    runs as subtrees, parked→synthetic `Idle` node, unmanaged grouped by session via their
    `unmanaged/<session>/<pane>` path) with cross-scope **lineage** shown as a `↖ <parent>`
    annotation (never a second placement/re-root — a parent that is a proper ancestor gets no
    annotation). `structure_lines` = deterministic time-independent render (golden diffs);
    `flatten(collapsed, now)` = UI rows (blocked floats upward, age column, glyphs). Helpers:
    `glyph_for_status`, `format_age`.
  - `app.rs` — the **ratatui/crossterm app** (`run_top`): `watch.open` → seed snapshot →
    background reader thread turns the event stream into a coalesced `Dirty`/`Disconnected`
    signal → render loop re-reads a fresh consistent `api.snapshot` **per frame** (event-driven,
    100ms budget, a burst = one redraw; by construction can't miss/double-apply). Reconnect +
    re-snapshot on `server_stopping`/EOF/`cursor_expired`. Keys: `/` filter (§5.1 grammar,
    resolved against the caller's `ORCR_PATH` scope), arrows collapse/expand + move, `q` quit.
  - `mod.rs` — re-exports (`run_top`, `build_tree`, `Snapshot`, `TopFilter`, `Tree`).
  - **Snapshot enrichment**: `server/mod.rs::agent_row_json` now also carries `model`,
    `move_state`, `herdr_session`, `last_status_change_at`, `starting_at`/`idle_since`/
    `parked_at`; `build_snapshot` adds each loop's active `runs` (run_id/uuid/status/due_at/
    started_at). Additive only — `ls`/`api snapshot`/`watch.open` shapes stay compatible.
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
    `integration_missing` naming the missing layer + install command, §11.4). **M3**:
    `TuningParams` + `tuning_for(provider, &config.integrations)` (completion tuning defaults
    per provider + config overrides).
  - `transcript.rs` — **M3 transcript adapters** (§11.4). **M7**: `locate_transcript` gained a
    `data_dir` param + a `mock`-provider branch that reads `<data_dir>/transcript.jsonl` directly
    (the mock writes a claude-format transcript into its own data dir — self-contained, never the
    user's `~/.claude`), so recipe/SDK e2e exercise `logs`/`ask`. Otherwise as M3: `locate_transcript` (claude:
    `~/.claude/projects/<cwd-slug>/<session_id>.jsonl`; codex:
    `~/.codex/sessions/**/rollout-*-<session_id>.jsonl`) with the **identity gate**
    (ambiguous candidates → `transcript_unavailable`), `TranscriptLocator::{read_entries,
    last_response,mtime_ms}` → common `TranscriptEntry` shape, and `transcript_fresh` (the
    freshness-gate helper). orcr keeps no response copies — reads always hit the native file.
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
  `$ORCR_MOCK_AGENT_BIN` when the server runs with `ORCR_ALLOW_MOCK_PROVIDER=1`. **M3**: it
  parses per-turn `@`-directives from the prompt (`@turn_ms`, `@tool_gaps`, `@gap_ms`,
  `@block`) so e2e can drive turn shape per agent (fast / tool-heavy idle gaps / blocked).
  **M7**: writes a claude-format `transcript.jsonl` into `$ORCR_AGENT_DATA_DIR` (read by the
  `mock` transcript adapter) so `logs`/`ask` resolve; new directives `@say=<word>` (exact
  response) + `@write=<relpath>` (file convention); `ORCR_MOCK_NO_TRANSCRIPT` opts out.

## SDK, recipes & skill (`sdk/ts/`, `skill/`) — M7

- `sdk/ts/` — **`@orchestratr/sdk`**, the TypeScript client of the socket API (§8). Built with
  `tsc` → `dist/`; unit tests via `node:test`/tsx.
  - `src/wire.ts` — the Unix-socket transport: newline-JSON framing, handshake/version check,
    one-request-per-connection `request`, `openStream`+`Subscription`, and **auto-start**
    (spawns `orcr server start --foreground` via `$ORCR_BIN`). `orcrHome`/`socketPath` mirror
    `home.rs`.
  - `src/path.ts` — a 1:1 TS port of `src/path.rs` (segment grammar, `resolveCreate`/
    `resolveSelector`, `Pattern`, `expandRand`, `loopNameFrom`). The SDK resolves scopes
    client-side and sends **absolute** selectors so the server never double-applies scope.
  - `src/scope.ts` — `orcr.scope()` over `AsyncLocalStorage` (not process-global); nests,
    `killOnThrow` barrier-kills `<scope>/**`. Base scope = `context.fromEnv().scope`.
  - `src/context.ts` — `fromEnv()` → `{kind:agent|loopRun|root, scope, dataDir, loop, …}`
    (agent vs loop-run distinguished by `ORCR_AGENT_DATA_DIR` presence, §5.3).
  - `src/errors.ts` — one class per §13 code + `errorFromWire`; `StateConflict.forceRequired`.
  - `src/generated.ts` — **generated** protocol client (every method 1:1) + `PROTOCOL_METHODS`/
    `EVENT_KINDS`/`ERROR_CODES`; produced by `scripts/codegen.ts` from `orcr api schema`.
    `npm run codegen:check` fails on drift (CI parity gate).
  - `src/client.ts` — the convenience layer (`orcr.*`): `agent.run`→`AgentHandle`
    (`wait/send/logs/followLogs/lastResponse/kill`, `dataDir`), collections
    (`agent.wait/ls/kill`), `ask`, `scope`, `watch`→`Watch`, `loop.*`+`loop.run.*`,
    `server.*`/`api.*`, `agent.prepareAttach`→`AttachHandle`. Ensures the server is running once,
    forwards `caller_id`/`caller_path` from env for lineage.
  - `recipes/` — the §9.1–9.7 fixtures (`_common.ts` stubs + provider selection via
    `ORCR_RECIPE_AGENT`/`_VERIFIER`/`_SCOPE`; `loop-until-done/` = kickoff+resume+file queue).
    Run against the mock in `recipe_e2e`; provider-literal copies live in `patterns.md`.
- `skill/SKILL.md` (≤ ~150 lines: decision ladder, hot path, specific roots, open-top rule,
  file convention, provider table, numeric discipline, guard rails, checklist) +
  `references/{cli,sdk,patterns,loops,files}.md` (loaded on demand). Doc-tested by
  `tests/skill_docs.rs` (no stale CLI flags vs `--help`; every `agent run`/`ask` sample carries
  `--name`/`--path`).
- `README.md` — the shipped quickstart (CLI + SDK + loops).

## Tests & the e2e harness

- Unit + lightweight tests run by default (fast): `handshake.rs`, `home_config.rs`,
  `server_protocol.rs` (M1 acceptance — no herdr needed), in-crate `#[cfg(test)]` modules.
- `tests/server_protocol.rs` spawns the real `orcr` binary over a throwaway `ORCR_HOME` to
  prove: the auto-start race → one server; `kill -9` → clean restart + intact store;
  `api schema` is valid JSON Schema with 100% method coverage; subscription replay/live has
  no gaps/dups and `cursor_expired` → re-snapshot; `server logs --follow` streams live.
- **e2e tests are gated behind `ORCR_E2E=1`** (so `cargo test` stays fast). Run them
  with `ORCR_E2E=1 cargo test --test e2e` (driver/harness) and
  `ORCR_E2E=1 cargo test --test agent_e2e -- --test-threads=1` (M2 agent core) and
  `ORCR_E2E=1 cargo test --test completion_e2e -- --test-threads=1` (M3) and
  `ORCR_E2E=1 cargo test --test gc_e2e -- --test-threads=1` (M4) and
  `ORCR_E2E=1 cargo test --test loop_e2e -- --test-threads=1` (M5) and
  `ORCR_E2E=1 cargo test --test top_e2e -- --test-threads=1` (M6) and
  `ORCR_E2E=1 cargo test --test recipe_e2e -- --test-threads=1` (M7). They
  exercise real behavior against **live herdr** using the mock provider. Non-M4 e2e
  harnesses set `ORCR_DISABLE_DISCOVERY=1` so unmanaged discovery doesn't pull the
  developer's real sessions into their stores.
- `tests/completion_e2e.rs` (M3) proves: fast turn, slow tool-heavy turn (idle gaps <
  settle window don't complete), blocked-then-send-clears, external input → synthetic turn,
  two consecutive sends (second wait never satisfied by the first idle), gc immediate →
  ended(completed), restart mid-turn re-arms, and logs → transcript_unavailable on the mock.
  Uses `integrations.mock.*` config to shorten completion windows.
- `tests/agent_e2e.rs` (M2) runs a real `orcr` server over a throwaway `ORCR_HOME` +
  disposable session (`TestServer` harness, drop-guard teardown) and proves the M2
  acceptance: run/send/kill lifecycle + env contract, 50-at-cap-5 FIFO/never-over-cap/drain,
  concurrent same-path one-winner, kill-during-starting, idle-held-at-working,
  integration-missing, crash-recovery (repair running + close orphan), and the path-model
  conformance table over socket **and** CLI.
- `tests/gc_e2e.rs` (M4) proves: park→send→un-park (home workspace + clocks reset), reap
  (`exit_reason: reaped` + pane closed), `gc never` exempt, explicit `--timeout` kill,
  crash-mid-park-move recovery (completes and rolls back, via the `ORCR_TEST_PARK_CRASH`
  fault hook), attach defers park/reap (incl. across a restart with a live lease) + resumes
  on release, unknown-marked-pane reported/never-closed, foreign-shell reported/never-touched,
  vanished-pane → lost → ended(lost), unmanaged discovery in a second disposable session
  (appears in `ls`, kill needs `--force`, terminal-gone → ended), and a scaled soak
  (`ORCR_SOAK_AGENTS`, default 20) asserting no leaked/wrongly-closed panes or workspaces.
- `tests/loop_e2e.rs` (M5) proves the loop acceptance: `--once-at` fire, capacity/FIFO
  promotion + coalesce (cap 1 slow runs → one pending, later fires fold), `loop run start` on a
  paused loop fires once (scheduled fires stay held), `loop run stop <run_id>` kills one of two
  concurrent runs + glob-kills its agents (the other survives), restart recovery (dead run
  closed + agents killed + pending decided once), `loop logs --run` isolation, missed-cron-fire
  skipped-and-logged (`e2e_missed_cron_fire_skipped`), and concurrency-safe promotion
  (`e2e_concurrent_promotion_no_double_spawn`: cap 2, 8 queued runs, no double-spawn). Drop
  guard kills each run's process group (via recorded `pgid`) **before** teardown so no lingering
  `orcr agent run` executes against a dead home. DST + cron next-fire are unit-tested in
  `src/cron.rs`; enable/disable unit files are golden-tested in `src/service.rs`.
- `tests/top_e2e.rs` (M6) proves the §7 acceptance against a live storm: the `watch.open`
  pinned snapshot renders a node set identical to `agent ls` (with the cross-scope
  `↖ fix_build/fixer` lineage placed once under `verify`); CLI/`/` filter node sets equal the
  equivalent `agent ls` queries (`review`, `review/*`, `review/**`, `reviewer/**`,
  `review/fanout/*`); the event stream delivers a post-snapshot change (seq > snapshot_seq)
  and the refreshed tree converges; a mid-storm `kill -9` restart re-opens `watch.open` and
  still matches the store; a 24-agent scale snapshot renders one consistent tree under the
  frame budget. Tree/filter/lineage + a synthetic 100-agent build-under-budget are unit-tested
  in `src/top/model/tests.rs`.
- `tests/recipe_e2e.rs` (M7) proves the SDK/recipe/scaffold acceptance against live herdr + the
  mock: every §9 recipe runs end-to-end (`e2e_recipes_run_against_mock`); a fan-out + a
  tournament run concurrently under distinct scopes (`e2e_concurrent_fanout_and_tournament` —
  scope isolation); the durable-handoff loop self-terminates; `orcr scaffold` (with an
  `ORCR_SDK_SPEC` tarball) + `npx tsx workflow.ts` runs green, re-run → `state_conflict`, pinned
  version == CLI version; SDK-composed paths equal the CLI's for the same nested scope
  (`e2e_sdk_scope_matches_cli`). `e2e_concurrent_burst_high` (4-way) is `#[ignore]`d — it
  surfaces a herdr concurrent-burst `agent.start` limitation (see `m7-sdk-skill/notes.md`).
- `tests/skill_docs.rs` (M7, **default suite**) doc-tests the skill: no stale CLI flags vs
  live `--help`; every `agent run`/`ask` sample carries `--name`/`--path`.
- `sdk/ts/test/*.test.ts` (M7, run via `npm test`): path-grammar parity with `path.rs`, the
  scope-composition property test, `context.fromEnv`, and codegen 100%-coverage/drift.
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
- New loop/scheduler behavior → `server/loops.rs` (tick + fire + stop/recover) + loop/run DAL
  in `store/mod.rs`; keep slot reservation atomic (`claim_pending_run` in one `BEGIN IMMEDIATE`).
- New OS service surface → `service.rs` (launchd/systemd unit builders + golden tests).
- New e2e → copy the disposable-home + disposable-session harness in `tests/`.
