# M0 · Foundations — todos

Ships: home layout, config, store, herdr socket driver, owned session, test harness + mock provider.

## Setup
- [x] Read master-prompt.md + full spec.md + this milestone file + herdr-driver-reference.md
- [x] Probe live herdr: schema, session list, wire envelope, protocol handshake, one-req-per-conn
- [x] Record discovered facts in notes.md
- [x] Cargo project scaffold (lib + `orcr` stub bin + `orcr-mock-agent` bin)
- [x] CI workflow (fmt/lint/test) + release build config

## Error model (§13)
- [x] `OrcrError` enum: nine codes + `details` + message
- [x] Exit-code mapping (0/1/2/3/4/5/6/7)
- [x] JSON error envelope `{ok:false,error:{code,message,details}}`
- [x] Unit tests: code→exit mapping, serialization

## Home layout (§11.6)
- [x] `ORCR_HOME` resolution (env override → `~/.orcr`)
- [x] Subpaths: store, `logs/`, `data/`, socket path, lock file
- [x] Safety checks: owned by current uid, not group/world-writable → `unsafe_home`
- [x] Create layout (dirs, umask 077 semantics)
- [x] Unit/integration tests: relocation via `ORCR_HOME`, unsafe-home detection

## Config (§14)
- [x] Duration parsing (units required, positive) — `45s/20m/3h`
- [x] `Config` struct: defaults / herdr / concurrency / timings / logs; all optional w/ defaults
- [x] Strict JSON load from `config.json` (missing file = all defaults)
- [x] Validation: durations units+positive, `concurrency.max ≥ 1`, per-provider caps clamped to max (warn), `herdr.session` valid
- [x] Unknown keys warn + suggest nearest valid name (ignored, not fatal)
- [x] Precedence helper: CLI flag → config → default
- [x] Golden tests for every validation error + warning; ORCR_HOME relocation of config

## Store (§12)
- [x] sqlite (WAL) init; full §12 schema: agents, turns, attaches, loops, loop_runs, events
- [x] All indexes incl. partial unique path index `UNIQUE(path) WHERE status NOT IN ('ended')`
- [x] Schema version stamp (`meta` table) + refusal on mismatch
- [x] Typed DAL; all writes via `BEGIN IMMEDIATE` tx helper
- [x] Minimal agent insert/get to exercise path reservation
- [x] Tests: init idempotent; active dup path insert fails; ended dup succeeds; version mismatch refused; tx helper rolls back

## herdr socket driver (§4, §11.7)
- [x] Binary discovery: config `herdr.bin` → `$ORCR_HERDR_BIN` → `$PATH`; missing → friendly pointer, exit 2
- [x] Wire protocol: request/success/error envelopes, tagged-union results
- [x] Connection per request (blocking UnixStream + timeouts)
- [x] Handshake: ping → read reported protocol → reject mismatch (`unsupported_version`)
- [x] Reconnect with backoff; `herdr_unreachable` error shape
- [x] Typed ops: agent.start, agent.list, pane.list, pane.get, pane.send_text, pane.send_keys, pane.move, pane.close, workspace.create, workspace.list, session.snapshot, notification.show, ping
- [x] Session enumeration (`herdr session list --json`) + per-session socket resolution
- [x] `done`-status normalization helper (done→idle for completion; ended on pane close)
- [x] herdr integration-state reads (`herdr integration status` parse) → per-provider {orcr,herdr}
- [x] Unit tests: envelope (de)serialization, handshake reject via stub socket, done-normalization, integration-status parse

## Driver contract table + conformance fixture (§11.7)
- [x] Contract table: every orcr op → named herdr method + fixed req/result shapes
- [x] Conformance fixture generated from `herdr api schema --json` (checked into repo)
- [x] Test: every pinned method exists in the live schema with expected param/result tags; version drift fails
- [x] Minimum herdr protocol version declared + handshake-checked

## Owned session bootstrap (§5.2)
- [x] Ensure owned session's herdr server running headless (via binary)
- [x] Discover its socket from `session list --json`
- [x] Config `herdr.session` override honored
- [x] Reuse if already running (idempotent)

## Test harness + mock provider
- [x] Mock provider CLI: scriptable (echo/sleep/finish-turn on cue), TUI-like
- [x] Mock reports state to herdr via `pane.report_agent` (best-effort integration mechanism)
- [x] e2e scaffolding: isolated `ORCR_HOME` per test, disposable session name `orcr_test_<rand>`
- [x] Guaranteed cleanup (drop guard: `herdr session stop`+`delete`)
- [x] Gate behind env flag `ORCR_E2E=1` so unit runs stay fast

## Acceptance criteria (prove each)
- [x] Driver conformance suite passes against installed herdr: every wrapped call round-trips
- [x] Version handshake rejects a fabricated protocol number
- [x] Store round-trip: schema init idempotent
- [x] Partial unique index: insert active duplicate path fails; ended duplicate succeeds
- [x] Config: golden tests for every validation error
- [x] Config: `ORCR_HOME` relocation works
- [x] Session bootstrap creates/reuses the owned session
- [x] Empty-workspace auto-removal observed (create pane → close pane → workspace gone)

## Green gates
- [x] `cargo build` clean
- [x] `cargo test` (unit) green
- [x] `cargo test` e2e (ORCR_E2E=1) green against live herdr + mock
- [x] `cargo clippy` clean (no warnings)
- [x] `cargo fmt` applied

## Deferred / out of scope
- Path grammar + glob matcher (§5.1) — consumers land in M2; M0 store only needs the
  partial unique index + minimal agent insert.
- orcr server process, CLI verbs, queue, GC, loops, reconciliation engine — M1+.
- Async/event-stream driver (`events.subscribe`) — M1+.