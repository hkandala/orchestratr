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

## Verifier & reviewer history

_(pending)_

## Deferred / out of scope
- top (M6), SDK loop helpers (M7), Windows Task Scheduler (lands with Windows support, §17).
- Real launchd/systemd `enable` round-trip against the developer's live login session is NOT
  run in automated e2e (it would write the real `dev.orchestratr.orcr` unit + touch the user's
  launchd); covered by golden unit-file tests + left for manual e2e "where available".
