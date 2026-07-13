# M5 · Loops

Durable scheduling: run any command on a cadence, surviving the caller's shell and the
machine's reboots. Ships the whole `loop` noun plus `server enable/disable` (which
exists so loops fire after a reboot).

## Scope

### loop create (spec §6.2)
- Cadence: five-field cron stored **with the creating timezone**, evaluated in it
  (DST-correct), occurrences persisted as UTC `next_fire_at`; or `--once-at <time>`.
- Payload: argv after `--`, exec'd directly (no shell); creation echoes parsed argv,
  cadence in words (local + UTC), and the cancel command.
- Name = one group segment, **mandatory** (positional first argument; no
  auto-generated loop names); always root-level (never inherits a creator agent's
  group); unique among active loops; internal uuid so removed names are reusable
  without history collisions.
- `--max-concurrency` (default 1), `--overlap queue|skip`, `--timeout` (no default).

### Runs & identity (spec §6.2, §12)
- Every run: uuid + **run_id** (5-char alnum, unique per loop); path
  `<loop_name>.<run_id>`; `due_at` = scheduled fire time.
- Own process group (pid/pgid recorded); env = §5.3 contract (run uuid + run path);
  cwd = loop's creation cwd; stdin `/dev/null`; stdout/stderr captured line-tagged,
  size-capped + rotated.
- Group inheritance from run context: agents spawned inside land under
  `<loop_name>.<run_id>.…` (completes the M2 inheritance stub).

### Scheduler (spec §11.3)
- Fire path: capacity check → transactional run allocation → spawn process group;
  at cap → overlap policy (`queue`: coalescing single `pending_due_at`; `skip`: log).
- Run exit: status mapping (`ok`/`failed`/`timeout`/`stopped` + exit code/signal);
  pending fire fires immediately when a slot frees.
- Missed fires (server down / machine asleep): skipped and logged, never replayed.
- Restart recovery: serialized per-loop transaction (verify running pgids → close
  dead runs + prefix-kill their agents → recompute active count → honor
  paused/ended → decide pending fire exactly once → recompute `next_fire_at`).
- Every scheduler action is an event row (fired, coalesced, skipped, paused-hold,
  timed out, stopped).

### Verbs
- Definition verbs: `loop create/pause/resume/rm/ls/logs`; run verbs under the
  `loop run` sub-noun:
- `loop run start <name>` — manual trigger (works on paused loops); prints
  `<loop_name>.<run_id> <run_uuid>`.
- `loop run stop <name> [<run_id>] [-y]` — TERM pgid → grace → KILL → prefix-kill
  the run's agents; run status `stopped`; TTY confirmation.
- `loop run ls <name> [--all]` — run_id, status, due_at vs started, duration, agent
  count.
- `loop ls [<name>...] [--status] [--all]`.
- `loop logs <name> [--run <run_id>] [--source orcr|command] [--tail] [--follow]` —
  interleaved command output + orcr scheduler events, lines tagged
  `[<name>.<run_id>]`.
- `loop pause|resume <name>...` — pending fire held/released.
- `loop rm <name>... [--kill-active] [-y]` — end the definition
  (`removed`/`removed_by_run`); history queryable; self-termination from inside a run
  via `orcr loop rm "${ORCR_FQN%%.*}"`.

### server enable/disable (spec §6.4)
- macOS launchd agent (`dev.orchestratr.orcr`, `RunAtLoad`, `KeepAlive`); Linux
  systemd user unit (`Restart=on-failure`); echo unit path + verify command;
  `unsupported_platform` elsewhere (Windows task lands with Windows support).
- `disable` removes the registration; running server + store untouched.

## Acceptance

- DST boundary tests: a "9am America/New_York weekdays" loop fires at 9am across both
  transitions (fixture clock).
- Overlap: cap 1 + slow runs → exactly one pending fire, later fires coalesce; `skip`
  drops with a log line.
- `loop run start` on a paused loop fires once; scheduled fires stay held.
- `loop run stop <name> <run_id>` kills one of two concurrent runs; the other survives; the
  stopped run's agents are prefix-killed.
- Reboot simulation: kill server with a running run + a pending fire → restart →
  dead run closed out, its agents killed, pending fire decided exactly once, missed
  cron fires skipped-and-logged.
- `loop logs --run` isolates one run's lines when two runs interleave.
- enable/disable round-trip on macOS and Linux CI (unit-file golden tests +
  launchctl/systemctl verification where available).

## Out of scope

top (M6), SDK loop helpers (M7).
