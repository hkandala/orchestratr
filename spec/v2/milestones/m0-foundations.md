# M0 · Foundations

Everything later milestones stand on: the home layout, config, store, the herdr socket
driver, the owned session, and the test harness. No user-facing agent features ship
here — M0 is done when the plumbing is provably correct against a live herdr.

## Scope

### Repo & home layout
- Project scaffold, CI (fmt/lint/test), release build.
- `ORCR_HOME` (default `~/.orcr`) created with safety checks (spec §11.6): owned by
  the current uid, not group/world-writable, else `unsafe_home`. Subpaths: store,
  `logs/`, `data/`, socket path, lock file.
- `ORCR_HOME` env override honored everywhere (tests run in throwaway homes).

### Config (spec §14)
- `~/.orcr/config.json`, strict JSON; every key optional with built-in defaults.
- Validation with precise errors: unknown keys rejected (suggest nearest valid name),
  durations require units and must be positive, `concurrency.max ≥ 1`, per-provider
  caps clamped to max with a warning.
- Precedence rules: CLI flag → config → default (mechanism only; consumers land in
  later milestones).

### Store (spec §12)
- sqlite (WAL) init with the full §12 schema: `agents`, `turns`, `attaches`, `loops`,
  `loop_runs`, `events` + all indexes, including the partial unique fqn index
  (`UNIQUE (group_path, name) WHERE status NOT IN ('ended')`).
- Typed data-access layer; all writes through `BEGIN IMMEDIATE` transaction helpers.
- Schema version stamp + refusal-with-message on mismatch (two orcr versions sharing
  one store).

### herdr socket driver (spec §4, §2)
- Binary discovery: config `herdr.bin` → `$ORCR_HERDR_BIN` → `$PATH`; missing →
  friendly install pointer, exit 2.
- Direct client for herdr's socket (`~/.config/herdr/herdr.sock`): connect, protocol
  handshake + version check (clear error naming the required herdr version), typed
  request/response wrappers for the operations orcr needs: pane/agent listing, agent
  start (argv, cwd, env, no-focus), pane send-text/send-keys, pane move (across
  workspaces), pane close, workspace/tab creation, agent_session + agent_status
  reads, terminal_id reads.
- Reconnect with backoff; `herdr_unreachable` error shape.

### Owned session bootstrap (spec §5.2)
- Ensure the `orcr` session's herdr server is running headless (via the herdr
  binary — the one bootstrap operation a socket can't do).
- Config `herdr.session` override.

### Test harness
- Mock provider: a scriptable CLI that acts like an agent TUI (echoes, sleeps,
  finishes turns on cue) and reports state through herdr's integration mechanism —
  the workhorse for all later e2e suites.
- e2e scaffolding: isolated `ORCR_HOME` per test, disposable herdr session names,
  guaranteed cleanup (drop guards), gated behind an env flag so unit tests stay fast.

## Acceptance

- Driver conformance suite passes against the installed herdr: every wrapped call
  round-trips; version handshake rejects a fabricated protocol number.
- Store round-trip tests: schema init idempotent; partial unique index enforces fqn
  reservation semantics (insert active duplicate fails, ended duplicate succeeds).
- Config: golden tests for every validation error; `ORCR_HOME` relocation works.
- Session bootstrap creates/reuses the owned session; empty-workspace auto-removal
  observed (create pane → close pane → workspace gone).

## Out of scope

The orcr server process, any CLI verb beyond internal test binaries, agents, queue,
GC, loops.
