# M5 · Loops — implementation notes

Decision log for the loop milestone: durable cron over any command, `loop`/`loop run` verbs,
the scheduler, and `server enable/disable`.

## Deviations from spec

- **`--once-at` time grammar** was under-specified; implemented as: a relative duration
  (`30m`, `2s` → `now + dur`, the common scripting form) first, else an RFC3339 timestamp,
  else a local wall-clock `YYYY-MM-DDTHH:MM[:SS]` / `YYYY-MM-DD HH:MM[:SS]`. Stored verbatim
  in `cadence_value`; the resolved UTC-ms is the single `next_fire_at`.
- **A missed `once` fire is not skipped on restart** (only cron missed fires are skipped-and-
  logged). A one-shot that never ran while the server was down fires when the server comes back
  (late, but once). Cron missed fires are skipped + logged per spec §6.2/§11.3.

## Decisions on under-specified points

- **Own cron evaluator** (`src/cron.rs`) instead of a cron crate: five-field
  `min hour dom month dow`, `*`/`a`/`a-b`/`a,b`/`*/n`/`a-b/n`, dow 0-6 (0/7 = Sunday),
  standard "both dom+dow restricted → OR" semantics. Next-fire is computed by **stepping
  wall-clock minutes in the creating tz** and converting each candidate to UTC — trivially
  DST-correct ("9am weekdays stays 9am"); spring-forward gaps skipped, fall-back folds take the
  earliest. Bounded to a 4-year search. tz captured at create via `iana-time-zone`; database via
  `chrono-tz`. DST acceptance is proven by the `cron.rs` unit tests (fixture UTC clock over both
  2026 US transitions).
- **Process-group identity guard**: each run is `setsid`'d (pgid == pid) and its **OS process
  start time** recorded (`/proc/<pid>/stat` field 22 on Linux; `proc_pidinfo(PROC_PIDTBSDINFO)`
  on macOS). A signal or recovery only targets a pgid whose leader is alive **and** whose start
  time still matches — never a reused pgid. If the platform can't read a start time, it degrades
  to "leader alive" (best-effort).
- **Run finalization ownership**: a fresh run has a monitor thread that `wait()`s the child;
  the stop/timeout path sets `stopping` first, so the monitor sees `stopping` and defers — the
  stop/timeout path finalizes (`stopped`/`timeout`). `finish_run` only updates a run still in
  `running`/`stopping`, so the two finalizers are idempotent. Recovered orphan runs (alive after
  a restart) get a poll monitor; dead ones are closed out `failed` at recovery.
- **Stop is synchronous in the handler**: `stopping` barrier → TERM → `run_term_grace` → KILL →
  glob-kill `<loop>/<run_id>/**` (reuses `agent.kill --force`, looped until clean) → finalize.
  `timings.run_term_grace` (default 10s) + `timings.loop_tick` (default 1s) added to config.
- **Loop-run scope**: a caller whose `ORCR_ID` resolves to a `loop_runs` row is a *directory* —
  its scope is its whole run path, and it parents children *inside* it (`caller_context` in
  `engine.rs`). Agents descended from a loop get `ORCR_LOOP_DATA_DIR = data/<loop_name>`.
- **Namespace protection** (`check_loop_namespace`): while a loop is active, a root/unrelated
  caller cannot create anything under its level-1 name (`invalid_request`, reason
  `reserved_name`); only a caller whose path is under `<loop>/…` may, and only while that run is
  `running` (the admission barrier — a `stopping`/ended run rejects new agents with
  `state_conflict`, reason `run_stopping`).
- **`loop logs`** interleaves each run's `run.log` (source=command, JSONL `{ts,stream,text}`,
  size-capped + rotated to `run.log.N`) with `loop*`/`loop_run*` events (source=orcr), tagged
  `[<name>/<run_id>]`, sorted by ts, `--tail`/`--follow`/`--run`/`--source` filters. `--follow`
  is a re-poll (like `agent logs --follow`). No new streaming socket method — `loop.logs` is a
  plain request the CLI re-polls.
- **enable/disable** (`src/service.rs`): macOS launchd plist (`dev.orchestratr.orcr`, RunAtLoad,
  KeepAlive-on-crash, absolute binary path, propagated `ORCR_HOME`/`ORCR_HERDR_BIN` + redirected
  logs); Linux systemd user unit (`orcr.service`, Restart=on-failure). Runs entirely CLI-side
  (no server needed — mirrors `herdr integration`). The loader step (`launchctl`/`systemctl`) is
  best-effort so a headless CI session that lacks the bus still gets the durable unit file.
  Golden unit-file tests assert the content. Windows Task Scheduler task deferred to §17.

## Discovered facts / gotchas

- Standard-library `Command::pre_exec` is the only portable way to `setsid` a child; the run's
  process group therefore survives a `kill -9` of the server (as at a real reboot), which the
  restart-recovery e2e exploits (kills the run's pgid explicitly to force the dead-run path).
- New event kinds added: `loop.ended` (once-loop end), `loop_run.stopping`.
- **Test-teardown safety gotcha**: a run command that spawns `orcr agent run` can, if it runs
  against a *torn-down* throwaway `ORCR_HOME` (tempdir already deleted at test end), fall back to
  the default config (`herdr.session = "orcr"`) and bootstrap the **real** `orcr` session — a
  safety-rule violation. This is purely a test artifact (production `~/.orcr` persists). The
  loop e2e drop guard now kills every run's process group (via the recorded `pgid`, read over the
  live socket) *before* stopping the server / deleting the home, so no lingering `orcr agent run`
  ever executes against a dead home. Verified: 3 consecutive full-suite runs leak no session and
  leave no orphan run processes.

## Verifier & reviewer history

- **Round 1 (verifier FAIL → reviser fixes):**
  - _Missed cron fire had no test coverage_ (medium): the only restart-recovery e2e used an annual
    cron that never came due, so the `nf <= now` skip-and-log branch in `recover_loops_on_start`
    was unproven. Added `e2e_missed_cron_fire_skipped` (tests/loop_e2e.rs): creates a `* * * * *`
    loop, kills the server before its slot, waits past the slot in real wall-clock, restarts, then
    asserts (a) a `loop.skipped`(reason=`missed_while_down`) event via `loop logs --source orcr`,
    (b) no run row was created for the missed slot (never replayed), (c) `next_fire_at` advanced
    forward. Passes against live herdr; full 8-test loop_e2e suite green, no session leak.
  - _Snapshot hardcoded `loops: []`_ (low): `build_snapshot` (server/mod.rs) now populates `loops`
    from `store.list_loops(&[], None, false)` via the shared `loops::loop_row_json` (made
    `pub(super)`), so the drift-proof snapshot API carries the loop noun after M5, matching spec §13.
  - _Create echo showed raw cron + bare UTC-ms_ (low): `cmd_loop_create` (cli.rs) now renders the
    cadence via `cron::describe` (previously dead code) and the next fire via a new
    `cron::describe_next_fire`, which formats `next_fire_at` as a human local+UTC timestamp
    (spec §6.2 "cadence in words, local + UTC"). Added a `describe_next_fire` unit test.

- **Round 2 (reviewer FAIL → reviser fixes):**
  - _Slot promotion was not concurrency-safe_ (medium, the FAIL): `promote_pending` read the
    active count + oldest pending under the lock, released it, then did file I/O + `Command::spawn`
    before `record_run_start` marked the run `running` — and `record_run_start` did an
    unconditional UPDATE. Two exit-monitor threads (or resume/stop/recovery) could observe the same
    free slot and spawn the SAME pending run twice, orphaning the first process group and exceeding
    `max_concurrency`. Fix: the slot is now **reserved atomically** in one `BEGIN IMMEDIATE` txn.
    `allocate_run` inserts a free-slot run already `running` (so it counts toward capacity before
    the lock drops); a new `store::claim_pending_run(loop_uuid, max)` counts active and, only if
    below cap, flips the oldest pending run pending→running and returns it (else `None`).
    `promote_pending` calls it and spawns only a claimed row. `record_run_start` no longer sets
    status — it only fills pid/pgid/start_time/timeout `WHERE status IN ('running','stopping')`, so
    it fills the pid for the killer without clobbering a concurrently-entered `stopping` barrier.
    Because `BEGIN IMMEDIATE` serializes writers, two claims can never win the same slot. Proven by
    three store unit tests (`fresh_allocation_reserves_slot_and_emits_fired`,
    `claim_pending_run_never_exceeds_capacity`, `record_run_start_preserves_stopping_barrier`) and a
    new e2e (`e2e_concurrent_promotion_no_double_spawn`: cap 2, 8 fast queued runs whose commands
    tally their run path — exactly one line per run, no duplicates, no extra run rows).
  - _Fresh pending run emitted `loop.coalesced`_ (low): `allocate_run` now always emits
    `loop.fired` (with `pending:true` when queued) for a freshly-created run; `loop.coalesced` is
    reserved for the true fold path (an existing pending scheduled run). Manual queued runs no
    longer show up as coalesces in `loop logs --source orcr`.
  - _Timeout stop stalled the scheduler tick_ (low): `enforce_run_timeouts` ran on the single tick
    thread and called `stop_run_process`, which sleeps `run_term_grace` between TERM and KILL,
    stalling all firing/timeout checks for the grace period. `stop_run_process` is split into
    `enter_stop_barrier` (fast, sets `stopping` — done synchronously on the tick so the next tick
    won't re-select the run) + `finish_stop` (the blocking TERM→grace→KILL→glob-kill→finalize). The
    timeout path enters the barrier then dispatches `finish_stop` to a dedicated thread; the manual
    `loop run stop` handler still runs synchronously (it is off the tick thread — by design).
  - _`loop logs` full-scanned the events table_ (low): `handle_loop_logs` called
    `events_since(0, 100_000)`; it now uses a new `store::events_for_refs(&refs)` keyed on the
    `events(ref_uuid, seq)` index, fetching only the loop's + its runs' events (all `loop.*` events
    ref the loop uuid, `loop_run.*` the run uuid). Retention-trimmed old orcr-source lines are still
    unavailable (documented on the method) — command output survives in `run.log`.

## Comprehensive-review updates (round 1)
- **`server.status.loops_firing` is now derived, not hardcoded `true`.** It reports the
  enable-state: whether `server enable` has registered a launchd/systemd unit
  (`service::is_enabled` lstats the platform unit path). Faithful to §6.4's durability
  framing — the scheduler always runs while the server is up, so the useful/distinct signal
  is whether loop firing survives a reboot before any `orcr` command runs. The `loops` array
  already shows what is scheduled.
- **`set_next_fire` / `set_last_fire` now route through `with_immediate_tx`** like every
  other store write, instead of a bare `conn.execute` — restoring the "all writes go through
  `BEGIN IMMEDIATE`" invariant (§12). Functionally benign before (single connection behind
  `Mutex<Store>`), but the inconsistency is removed.

## Deferred / out of scope
- top (M6), SDK loop helpers (M7), Windows Task Scheduler (lands with Windows support, §17).
- Real launchd/systemd `enable` round-trip against the developer's live login session is NOT
  run in automated e2e (it would write the real `dev.orchestratr.orcr` unit + touch the user's
  launchd); covered by golden unit-file tests + left for manual e2e "where available".
