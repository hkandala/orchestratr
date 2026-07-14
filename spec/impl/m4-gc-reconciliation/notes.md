# M4 · GC & reconciliation — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts. Capture *decisions and deviations*.

## Deviations from spec

- **"marked" vs "unmarked" panes are distinguished by herdr's reported `agent`, not by
  reading the `ORCR_ID` pane env.** herdr's socket does not expose pane env (confirmed in
  m0 notes: herdr injects env but the socket has no read-env method). So the reconciler
  classifies an owned-session pane with no matching store row as an **unknown marked
  pane** iff herdr reports it as running an agent (`PaneInfo.agent` set or an
  `agent_session` present); a plain shell (no agent) is an **unmarked** foreign pane. This
  matches the drills exactly (a mock/real agent pane whose row was deleted reports an
  agent; a user shell does not) while staying within the socket's capabilities. Both are
  reported and never touched.

- **`home_workspace` is derived, not stored.** The milestone text says park "records
  `home_workspace`", but §12 lists home-workspace as derived (first path segment, else
  `default`) and the path is immutable, so no column is needed — un-park derives the home
  workspace from the path.

## Decisions on under-specified points

- **Idle workspace holding pen.** Park moves the pane into a `NewTab` in the `idle`
  workspace; the idle workspace's root shell (created on first park) is closed right after
  the move so the pen holds only parked agent panes (never a leftover shell that would be
  miscounted as an unmarked foreign pane). When the last parked pane leaves, herdr
  auto-removes the empty idle workspace.

- **Two-phase move = an exclusive `move_token` lease + CAS.** `begin_move` sets
  `move_state`+`move_token` only if the row is still at the expected status with no move in
  flight; `finish_park`/`finish_unpark`/`rollback_move` only act if `move_token` still
  matches. Recovery decides complete-vs-rollback by the pane's *current workspace* (found
  by the move-stable `terminal_id`): parking + pane-in-`idle` → finish; parking +
  pane-still-home → rollback; symmetrically for unparking. So status and location always
  agree after a crash. A fault-injection hook (`ORCR_TEST_PARK_CRASH=before_move|after_move`,
  test-only) drives both crash paths.

- **Restart re-arms the park clock for idle agents.** M3's restart re-arm cleared
  `idle_since` for all active agents. That left a turn-complete `idle` agent (whose turn is
  closed, so the completion monitor never re-sets `idle_since`) *never* park-eligible after
  a restart. Fixed: `rearm_idle_clocks_on_restart` clears `idle_since` for `working`/
  `blocked` (re-measure completion from a fresh transition) but sets `idle_since = now` for
  `idle` agents (restart the park clock). `parked` keeps its `parked_at` reap clock.

- **attach exec command.** `agent.attach.prepare` returns
  `[herdr, --session <s>, agent, attach, <terminal_id>, (--takeover)?]`. The target is the
  globally-unique, move-stable `terminal_id`, so the command still addresses the agent even
  after a park/un-park. Two extra socket methods back the lease lifecycle:
  `agent.attach.heartbeat` and `agent.attach.release`. The CLI runs a background heartbeat
  thread (every ~ttl/2) while the interactive `herdr agent attach` child runs, and releases
  on exit; abrupt CLI death → the lease expires by heartbeat (`expire_leases` on the GC
  tick emits `attach.ended`).

- **Unmanaged discovery cadence + the mock.** Discovery polls non-owned sessions every 3s
  (`§5.7 "every few seconds"`), fanning out over each session's socket. Supported = both
  integrations present (§11.4); the test-only `mock` counts as supported when
  `ORCR_ALLOW_MOCK_PROVIDER=1`. Unmanaged path is `unmanaged/<session>/<pane>` with each
  component slugified to `[a-z0-9_]` and a deterministic terminal-hash suffix on a slug
  collision.

- **`ORCR_DISABLE_DISCOVERY=1`** disables the discovery poller. Discovery legitimately
  scans the developer's *real* `default` session and pulls its live agents in as unmanaged
  rows (this is the spec's intent, §5.7) — but that pollutes tests that assert an exact
  event stream (`server_protocol`) or agent counts (M2/M3 e2e). Those non-M4 suites set the
  flag; the M4 gc e2e leaves discovery on and exercises it.

- **Explicit `--timeout` fires in every gc mode**, including `gc never` (there is no
  *default* timeout, but an explicit one is always enforced): the GC tick kills
  `deadline_at`-past agents with `exit_reason: timeout`.

## Discovered facts / gotchas

- **`terminal_id` is stable across `pane.move`; `pane_id` is not** (pane_id encodes the
  workspace). Move recovery and attach both key off `terminal_id`.
- **Discovery does scan the user's `default` session** (read-only). In a dev environment
  the live orchestration claude shows up as `unmanaged/default/<pane>` — harmless, and the
  M4 discovery e2e selects its own hand-started mock by `agent == "mock"` to avoid matching
  it.
- **Completion monitor + parked agents.** A parked agent is still monitorable; if herdr
  spontaneously reports it `working` (e.g. a background subagent resuming, §5.4 caveat) the
  monitor opens an external turn and flips it to `working` **without** moving the pane home
  (auto-unpark-on-resume needs background-subagent detection, which is out of scope for
  M4). Un-park on `send` is fully implemented. Not harmful: the pane stays in the idle
  workspace but the agent is tracked correctly and will re-park.

## Verifier findings (round 1 — FAIL)

- **REGRESSION: `tests/e2e.rs::e2e_server_status_reports_herdr` fails under `ORCR_E2E=1`.**
  M4's discovery poller scans the real `default` session and counts its live agent into
  `counts.live`, so the M0/M1 assertion `counts.live == 0` gets `1`. The three other non-M4
  suites (agent_e2e, completion_e2e, server_protocol) were given `ORCR_DISABLE_DISCOVERY=1`
  but this harness was missed. Self-report's "e2e 5/5 / no regressions" is false in a normal
  dev env (a live agent in `default`). Fix: add `.env("ORCR_DISABLE_DISCOVERY","1")` to that
  test's server spawn (matching the sibling suites).
- **Related (root cause): `counts.live` includes unmanaged agents.** `counts()` uses
  `status NOT IN ('ended','lost')` with no `managed=1` filter, so discovered non-owned-session
  agents inflate `live` (double-counted with `unmanaged`). Spec §5.6 (line 1866) lists
  `live` and `unmanaged` as distinct fields. Preferred fix: make `live` count managed agents
  only (`AND managed = 1`) — this also makes the e2e assertion robust regardless of discovery.

### Round-1 fixes (reviser)

- **`counts()` now filters `managed = 1`** for `live`/`queued`/`blocked` (src/server/mod.rs).
  `unmanaged` stays `managed = 0 AND status != 'ended'`. Live and unmanaged are now the
  distinct categories spec §5.6 lists; discovered non-owned-session agents no longer inflate
  `live` (and are no longer double-counted).
- **`tests/e2e.rs::e2e_server_status_reports_herdr` given `ORCR_DISABLE_DISCOVERY=1`** on the
  server spawn, matching the three sibling non-M4 suites — kills the regression at its source.
- **New coverage:** `gc_e2e::e2e_unmanaged_discovery` now asserts the managed/unmanaged split
  (`counts.live == 0` while `counts.unmanaged >= 1`) against live herdr + mock.
- **Flake hardening (adjacent):** `gc_e2e::e2e_park_send_unpark` was load-sensitive — under
  full-suite load the observer thread got starved past the narrow `park` window (FAST_GC:
  idle_after 1s → idle-re-park, kill_after 2s → reap). Fixed by (a) polling for the home-move
  right after `send` instead of a single-shot read after `agent.wait`, and (b) a dedicated
  `PARK_GC` config (idle_after 5s, kill_after 60s) that widens the observable window without
  changing what's tested. Full `ORCR_E2E=1 cargo test` now green across every suite.

## Reviewer findings (round 1 — FAIL) → fixes

- **HIGH — `agent send` / `kill --force` routed unmanaged agents through the OWNED session.**
  Both handlers used `self.owned_driver()` and then operated on `row.pane_id`, but an
  unmanaged agent's pane lives in a *foreign* herdr session (sessions are per-socket) and
  `pane_id` is workspace-scoped (not globally unique). So `send` mis-delivered/failed and
  `kill --force` never closed the foreign pane (or hit a colliding owned pane), after which
  discovery re-inserted a duplicate row. **Fix:** new `Server::driver_for_agent` connects to
  the socket of the session `row.herdr_session` names (owned → cached driver; foreign →
  `HerdrBinary::find_session(session).socket_path` → `HerdrDriver::connect`, mirroring
  discovery.rs), and `Server::live_pane_id` resolves the current pane by the globally-unique,
  move-stable `terminal_id` on that driver (falling back to the recorded `pane_id`).
  `handle_agent_send` and `handle_agent_kill` now route through both. attach was already
  correct (it built its exec command from `info.herdr_session` + `terminal_id`).

- **MEDIUM — park vs send race could desync status/location.** A `send` resolving a row after
  `park_one` committed `begin_move` (move_state=parking) but before its `pane_move`+`finish_park`
  called `unpark_for_send → recover_one_move`, which read the pane's still-home workspace and
  rolled back the *live owner's* token; `park_one` then physically moved the pane and its
  `finish_park` no-oped, leaving the pane in `idle` but the row idle/none with a stale pane_id.
  **Fix:** a **per-agent move mutex** (`ServerInner::move_locks`, via `Server::lock_move`).
  `park_one` and the periodic/startup `recover_one_move` acquire it for the whole two-phase
  move; `handle_agent_send` (managed targets) holds it across the re-read + un-park + delivery,
  so a send can never pre-empt a live move (it either sees the park fully done — row parked,
  clean un-park — or the park hasn't started). `recover_one_move` split into a locking wrapper
  + `recover_one_move_locked` (called by `unpark_for_send`, which already holds the lock).

- **LOW — no e2e for the unmanaged verb contract beyond the no-force skip.** Added
  `gc_e2e::e2e_unmanaged_verb_contract`: against a mock hand-started in a second disposable
  (foreign) session, `send @block` lands in the foreign pane (mock → `blocked`, mirrored into
  the row), `wait` resolves on the blocked agent, `logs` resolves (→ `transcript_unavailable`,
  not `integration_missing`), and `kill --force` closes the foreign pane + ends the row with no
  duplicate re-discovered. All 14 gc_e2e + 9 agent_e2e green against live herdr 0.7.2.

## Verifier & reviewer history

- **Implementation** (this pass): store DAL (park/reap/timeout candidates, two-phase move
  CAS, attach leases, unmanaged upsert) + `move_state`/`move_token`/`parked_at` columns
  surfaced on `AgentFull`; the GC engine (`gc.rs`: park/reap/timeout, un-park on send,
  periodic reconciliation, drift counts); unmanaged discovery (`discovery.rs`); attach
  verb + socket methods; `server status` drift/unmarked/unknown-marked counts. All green:
  `cargo build`, `cargo test` (120 unit), `cargo clippy -D warnings`, `cargo fmt`, and the
  gated e2e — `gc_e2e` 13/13, plus M0/M1/M2/M3 suites still green — against live herdr
  0.7.2 with the mock provider, all over disposable homes + disposable sessions torn down
  by drop guards.
