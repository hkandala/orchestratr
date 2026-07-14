# M2 · Agent core — todos

Ships: identity (uuid+path), queue, spawn pipeline, claude+codex integrations, run/send/kill/ls, status model.

## Scope

### Identity & paths (§5.1, §5.3)
- [x] `path` module: grammar (`segment=[a-z0-9_]{1,64}`, ≤8 segments, ≤256 chars, last=name)
- [x] `{rand}` placeholder expansion (creation only, 5 [a-z0-9])
- [x] absolute vs relative resolution against caller scope (leading `/` = absolute)
- [x] caller-scope derivation from `ORCR_ID` (agent → path minus name; loop-run stub)
- [x] reserved level-1 names (`idle`, `unmanaged`; active-loop names deferred to M5)
- [x] depth-limit + reserved-name + grammar errors (`invalid_request` reasons)
- [x] pattern grammar `*` (one segment) / `**` (any depth), anchored full-path match
- [x] wildcards rejected for singleton verbs
- [x] `--name` vs `--path` (exactly one, mandatory)

### Store DAL (§12)
- [x] full agent insert (launch payload columns) in one BEGIN IMMEDIATE txn
- [x] queue_seq allocation; FIFO promotion with global + per-provider capacity recount
- [x] path-in-use → `state_conflict` (reason path_in_use, occupying uuid/path/status)
- [x] resolution: uuid / uuid-prefix (≥8) / path (active first, else latest ended)
- [x] location update (pane_id/terminal_id/herdr_session)
- [x] agent_session capture
- [x] cancel_requested interlock set/read
- [x] status transitions + exit_reason + ended_at
- [x] input_seq bump + turns row (send bookkeeping)
- [x] ls query with filters (pattern, provider, status, managed/unmanaged, all)

### Spawn pipeline (§11.1)
- [x] durable row (queued) before any herdr call; launch.json written to data dir
- [x] data dir created at path-mirrored location `$ORCR_HOME/data/<segs>/<uuid>/`
- [x] promotion → starting; ensure session; ensure level-1 workspace
- [x] agent.start with env contract (§5.3) + launch token; location recorded
- [x] cancel_requested checked before/after each herdr step
- [x] agent_session captured when reported
- [x] first prompt delivery (two-call: send-text → ~1s → enter)
- [x] status starting → working
- [x] stuck-start guard (max_starting, reset by progress) → failed, slot released
- [x] crash recovery on restart (pane_id/tab-label match → repair or fail; no dup panes)

### Integrations (§11.4)
- [x] claude + codex launch argv (bypass-permissions), model/effort mapping
- [x] startup recipe hook + graceful-shutdown recipe
- [x] both-layers-required: `run -a p` → `integration_missing` naming missing layer+install
- [x] per-provider integration state in `server status`

### Verbs (§6.1)
- [x] `agent run` full flag surface, prints `<path> <uuid>`, TTY stderr hints
- [x] `agent send` exact target, delivered_while + input_seq
- [x] `agent kill` patterns+uuids, TTY confirm default / `-y`, graceful → pane close,
      killed/canceled reasons, result classification (§6.1)
- [x] `agent ls` tree render + display transform, filters, flat JSON rows

### Status model (§5.6)
- [x] `queued→starting→working`, `ended`, `lost` (reconciler stub), `blocked` passthrough
- [x] pre-M3 normalization: herdr idle/done held as `working`
- [x] exit_reason values wired

### Protocol / server
- [x] flip agent.run/send/kill/ls handlers to implemented in api.rs
- [x] caller identity params (ORCR_ID/ORCR_PATH) threaded through run
- [x] events emitted (agent.created/status_changed/ended, queue.changed/promoted)
- [x] api.snapshot returns real agents/queue

## Acceptance criteria (prove each)
- [x] e2e (mock): run → pane appears under right workspace/tab; env contract present; send delivers; kill graceful → pane closed → workspace emptied
- [x] 50 concurrent runs, cap 5: FIFO held, never over cap, queue drains
- [x] concurrent same-path spawns: exactly one wins, other `state_conflict` path_in_use
- [x] path-model conformance table (name/relative/absolute/scope/ended-reuse/depth) over CLI + socket
- [x] kill during `starting`: canceled cleanly between steps (fault injection)
- [x] provider reporting idle immediately after start held at `working` (no false completion)
- [x] crash mid-spawn (kill -9 between steps) → restart → recovery repairs/fails; no dup panes
- [~] real claude + real codex e2e — best-effort / deferred to manual-e2e phase

## Deferred / out of scope
- turn completion / wait / idle (M3), transcripts/logs (M3), gc parking (M4),
  attach (M4), unmanaged discovery (M4), loops (M5)
- active-loop-name reservation (M5): level-1 reserved set is `idle`+`unmanaged` for now
- real-provider e2e is best-effort (mock is the automated gate, per master-prompt §6)
