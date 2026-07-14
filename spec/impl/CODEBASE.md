# orchestratr codebase map (living)

A concise, cumulative map of the code as it exists **right now**, so implementers don't
have to re-read the whole source tree to get oriented. **Read this first** to understand
the current layout, then open only the specific files you need to touch (their exact
signatures live in the source, not here). **Every milestone's scribe updates this file**
to reflect what that milestone added/changed.

> This is a map, not a mirror — for exact function signatures, open the file. For the
> *why* behind decisions, read the per-milestone `notes.md` files (especially the herdr
> facts in `m0-foundations/notes.md`, which are load-bearing for the driver).

Current state: **through M0 (foundations).**

## Crate & binaries

- Crate `orchestratr` (lib at `src/lib.rs`), edition 2021, rust 1.89, `default-run = "orcr"`.
- Binaries: `orcr` (`src/bin/orcr.rs`) and `orcr-mock-agent` (`src/bin/orcr-mock-agent.rs`).
- `orcr` currently exposes a hidden `__m0-selfcheck` subcommand used by the harness (no
  user-facing verbs yet — those start in M1/M2). CLI arg parsing is minimal/hand-rolled
  so far; `clap` will likely come in with the real verb surface (M1/M2) — add it then.
- Deps in use (M0): `anyhow`, `thiserror` (v1), `serde`/`serde_json`, `rusqlite`
  (bundled, WAL), `uuid` (v4 + v7), `dirs`, `libc`, `chrono` (clock+std). dev: `tempfile`.
  Add new deps as milestones need them (tokio/socket server in M1, clap for CLI, cron/
  chrono-tz for loops in M5, ratatui/crossterm for top in M6, etc.).

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
- `store/` — sqlite (WAL), single-writer.
  - `schema.rs` — the full §12 schema (`agents`, `turns`, `attaches`, `loops`,
    `loop_runs`, `events` + all indexes incl. the partial unique path index) and a `meta`
    table stamping `schema_version` (mismatch → `store_version_mismatch` refusal).
  - `mod.rs` — the typed data-access layer; **all writes go through `BEGIN IMMEDIATE`
    transaction helpers**. Extend this with typed row structs + query/insert/update fns
    as milestones add behavior (agents in M2, turns in M3, loops in M5, events in M1…).
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
  - `integration.rs` — per-provider integration scaffolding (launch argv, both-layers-
    required checks). claude + codex land as real integrations in M2.
  - `contract.rs` — the driver conformance table (§11.7) pinned to named herdr methods
    with fixed shapes; checked against a fixture generated from live `herdr api schema`
    (`tests/conformance_live.rs`), so herdr version drift fails CI.

## Binaries

- `src/bin/orcr.rs` — the CLI entrypoint (hidden `__m0-selfcheck` for now).
- `src/bin/orcr-mock-agent.rs` — the **mock provider**: a scriptable fake agent TUI that
  self-discovers its herdr pane from injected env (`HERDR_SOCKET_PATH`, `HERDR_PANE_ID`)
  and reports its own state to herdr via `pane.report_agent` (state = idle|working|
  blocked|unknown, + optional transcript pointer). This is the workhorse for all e2e
  suites — use it instead of real providers in automated tests. Env knobs include
  `ORCR_MOCK_NO_REPORT` (suppress self-reporting when the test drives state elsewhere).

## Tests & the e2e harness

- Unit + lightweight tests run by default (fast): `handshake.rs`, `home_config.rs`,
  in-crate `#[cfg(test)]` modules.
- **e2e tests are gated behind `ORCR_E2E=1`** (so `cargo test` stays fast). Run them
  with `ORCR_E2E=1 cargo test --test e2e`. They exercise real behavior against **live
  herdr** using the mock provider.
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
- New e2e → copy the disposable-home + disposable-session harness in `tests/`.
