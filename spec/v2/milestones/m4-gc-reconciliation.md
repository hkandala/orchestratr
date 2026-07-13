# M4 · GC & reconciliation

Lifecycle at scale: parking, reaping, attach protection, drift repair, and tracking
the agents orcr didn't create. M4 is done when a hundred agents can churn for hours
and the owned session stays tidy without ever harming a pane it shouldn't.

## Scope

### GC auto (spec §5.4, §11.2)
- Tick ~30s; all transitions CAS-versioned.
- Park: turn-complete + idle ≥ `idle_after` → two-phase move to the `idle` workspace
  (`move_state: parking` → status `parked`); `home_workspace` recorded.
- Reap: parked ≥ `kill_after` → graceful kill (`exit_reason: reaped`) + pane closed.
- Un-park on `send`: `move_state: unparking`, cancel pending reap, move pane home
  (recreate the tab if gone), confirm location, status → `idle`, then deliver.
- `--gc never` exemption; no default timeout anywhere; explicit `--timeout`
  enforcement (kill with `exit_reason: timeout`).

### attach (spec §6.1)
- `agent attach <path|uuid> [--takeover]` — terminal-mediated: `agent.attach.prepare`
  inserts the lease FIRST (same transaction as target/location resolution), returns
  the exec command; the CLI runs `herdr agent attach`, heartbeats while it runs,
  releases on exit (abrupt CLI death → `expires_at`/heartbeat expiry; fields:
  mode, connection, client_pid, expires_at).
- GC defers moves/reaps while a lease is fresh — including across server restarts;
  queued/ended targets → `state_conflict`.

### Reconciliation (spec §11.5)
- On server start + periodically: vanished panes → `lost` (path reserved; resolved
  to `ended (lost)` once herdr is reachable and one following poll still misses the
  terminal, or on explicit kill — outage never frees names, no indefinite quarantine
  either); marked panes with no store row → **counted and reported in
  `server status` as unknown marked panes, never touched** (clean up via herdr);
  unmarked panes in the owned session → counted, reported, untouched; half-done
  `move_state` moves (token-owned exclusive leases) → completed or rolled back.
- `server status` gains the drift/unknown-marked/unmarked counts.

### Unmanaged discovery (spec §5.7)
- Poll/stream herdr for agents in non-owned sessions every few seconds — **supported
  providers only** (both integrations present, §11.4); others ignored entirely.
- Rows keyed by (herdr session, `terminal_id`); path `unmanaged/<session>/<pane>`;
  uuid like any row; new terminal = new row; terminal gone → `ended`.
- Unmanaged lifecycle statuses (working/idle/blocked/unknown/ended); the §5.7
  behavior contract enforced verb-by-verb (`kill` needs `--force`; no GC; send/wait/
  attach/logs work).

## Acceptance

- Park → send → un-park e2e: agent returns to its home workspace, clocks reset,
  delivery lands after the move confirms.
- Kill the server mid-park-move (fault injection around the herdr move) → restart →
  reconciler completes or rolls back; status/location always agree afterward.
- Attach guard: park/reap deferred while attached (including after a server restart
  with a live lease); resumes after detach.
- Unknown-marked-pane drill: delete the agent's store row under a live pane →
  reconciler reports it in `server status`, never closes it.
- Foreign-pane safety: a user shell opened inside the owned session is reported and
  never touched across many GC cycles.
- Unmanaged drill: hand-start an agent in a user session → appears in `ls` within
  seconds with correct provider/status; close its pane → row `ended`; logs work when
  its herdr integration reports an `agent_session`.
- Soak: 100 mock agents churning for an hour → workspaces stay clean, no leaked
  panes, no wrongly-closed panes (assertion via herdr snapshot diff).

## Out of scope

Loops (M5), top (M6). Background-subagent detection (future; the §5.4 caveat is
documented behavior in M4).
