# M4 · GC & reconciliation — todos

Ships: gc auto park/reap, attach + leases, reconciler, unmanaged discovery.

## Required reading
- [x] master-prompt.md + spec.md (§5.4, §5.6, §5.7, §6.1, §11.2, §11.4, §11.5, §12, §13, §14) + milestone + herdr-driver-reference + prior notes

## GC auto (§5.4, §11.2)
- [x] GC engine thread ticks every `timings.gc_tick` (~30s); all transitions CAS-versioned via `move_token`/status guards
- [x] Park: turn-complete + idle ≥ `idle_after` → two-phase `move_state: parking` → pane moved to `idle` workspace → status `parked` (home workspace derived from path)
- [x] Reap: parked ≥ `kill_after` → graceful kill (`exit_reason: reaped`) + pane closed
- [x] Un-park on `send`: `move_state: unparking`, cancel pending reap, move pane home (recreate tab if gone), confirm location, status → `idle`, reset clocks, THEN deliver
- [x] `--gc never` exempt from park/reap; `gc immediate` unaffected (already M3)
- [x] No default timeout; explicit `--timeout` → kill (`exit_reason: timeout`) on `deadline_at` expiry (all gc modes)
- [x] GC defers park/reap while an attach lease is fresh (survives restart)
- [x] Unmanaged agents never GC'd

## attach (§6.1, §5.4, §11.2)
- [x] `agent.attach.prepare`: validate target (active; queued/ended → state_conflict); insert lease FIRST in same tx as location read; return exec command (`herdr --session <s> agent attach <terminal_id> [--takeover]`)
- [x] `agent.attach.heartbeat` + `agent.attach.release` socket methods (registered in api schema)
- [x] CLI `agent attach <path|uuid> [--takeover]`: prepare → heartbeat loop → exec herdr attach → release on exit
- [x] Lease fields: mode, connection, client_pid, started_at, heartbeat_at, expires_at; abrupt death → heartbeat/expires expiry
- [x] GC guard reads fresh lease (expires_at > now)

## Reconciliation (§11.5)
- [x] On start + periodically: vanished panes → `lost` (path reserved)
- [x] `lost` → `ended (lost)` once herdr reachable + one following poll still misses the terminal (outage never frees names; no indefinite quarantine)
- [x] Marked panes (agent-running) with no store row → counted + reported as unknown marked panes, never touched
- [x] Unmarked panes (plain shells) in owned session → counted + reported, untouched
- [x] Half-done `move_state` moves → completed or rolled back (token-owned)
- [x] `server status` gains drift/unknown-marked/unmarked counts

## Unmanaged discovery (§5.7)
- [x] Poll non-owned sessions every few seconds; supported providers only (both integrations), others ignored
- [x] Rows keyed by (herdr session, terminal_id); path `unmanaged/<session>/<pane>`; uuid like any row
- [x] New terminal = new row; terminal gone → `ended`; slug collisions → deterministic suffix
- [x] Unmanaged statuses working/idle/blocked/unknown/ended
- [x] Behavior contract: kill needs `--force` (already), no GC, send/wait/attach/logs work

## Acceptance criteria (prove each)
- [x] Park → send → un-park e2e: returns home workspace, clocks reset, delivery lands after move confirms
- [x] Kill server mid-park-move → restart → reconciler completes or rolls back; status/location agree
- [x] Attach guard: park/reap deferred while attached (incl. after restart with live lease); resumes after detach
- [x] Unknown-marked-pane drill: delete store row under a live pane → reported in status, never closed
- [x] Foreign-pane safety: user shell in owned session reported + never touched across many GC cycles
- [x] Unmanaged drill: hand-start agent in a user session → appears in `ls` within seconds; close pane → `ended`; logs work with agent_session
- [x] Soak: mock agents churning → workspaces clean, no leaked/wrongly-closed panes (herdr snapshot diff) — automated at a CI-feasible scale (default 20, `ORCR_SOAK_AGENTS` override); the full 100×1h is a manual soak

## Green gates
- [x] cargo build, cargo test (unit), cargo clippy -D warnings, cargo fmt
- [x] gc_e2e.rs against live herdr + mock provider

## Verifier round-1 fixes
- [x] `counts.live/queued/blocked` filter `managed = 1` (spec §5.6: live vs unmanaged distinct)
- [x] `tests/e2e.rs::e2e_server_status_reports_herdr` gets `ORCR_DISABLE_DISCOVERY=1` (fix regression)
- [x] gc_e2e unmanaged-discovery asserts counts split (live 0, unmanaged >= 1)
- [x] Harden `e2e_park_send_unpark` against load starvation (poll home-move + PARK_GC wide window)
- [x] Full `ORCR_E2E=1 cargo test` green across every suite (no regressions)

## Deferred / out of scope
- Loops (M5), top (M6). Background-subagent detection (future; §5.4 caveat documented).
