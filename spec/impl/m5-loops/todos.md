# M5 · Loops — todos

Ships: loop create/pause/resume/rm/ls/logs + run start/stop/ls, scheduler, server enable/disable.

## Tasks

- [x] Read master-prompt.md + full spec.md + this milestone file + herdr-driver-reference.md

### Cadence / cron
- [x] `cron.rs`: parse five-field cron (`*`, `a`, `a-b`, `a,b`, `*/n`, `a-b/n`); dow 0-6 (0/7=Sun), months 1-12
- [x] DST-correct next-fire: evaluate in the creating tz, persist UTC `next_fire_at`
- [x] `--once-at <time>`: parse an absolute/relative time → single UTC fire
- [x] tz detection: creating timezone captured at create time (iana-time-zone)
- [x] unit tests: cron field parsing, next-fire, DST spring-forward/fall-back at 9am NY weekdays

### Store DAL (loops / loop_runs)
- [x] `create_loop` (uuid, name unique among active/paused, cadence, tz, cwd, caps) + `loop.created` event
- [x] `find_loop_by_name` (active first, else most recent ended), `loop_by_uuid`, `list_loops`
- [x] `pause_loop`/`resume_loop`/`end_loop` (removed|removed_by_run|fired) + events
- [x] `active_loop_names` (namespace protection)
- [x] `allocate_run` (uuid + run_id unique per loop + kind + due_at; pending-vs-running decided by caller under one txn); coalesce pending scheduled runs
- [x] `record_run_start` (pid/pgid/pgid_start_time/started_at), `finish_run` (status+exit+signal), `set_run_stopping`, `cancel_pending_run`
- [x] `runs_for_loop` (filter status/all), `active_runs`, `pending_runs`, `running_runs`, `run_by_id_or_uuid`
- [x] `loops_due` (next_fire_at <= now), `set_next_fire`/`set_last_fire`
- [x] run agent count derived (`<loop>/<run_id>/**` active)

### Scheduler (server/loops.rs)
- [x] tick thread: compute due loops, allocate + spawn or coalesce/skip, honor pause
- [x] fire path: allocate run row (transactional), start process in own process group (setsid), record pid/pgid + start time
- [x] run process: cwd = loop creation cwd; env = §5.3 contract (ORCR_ID=run uuid, ORCR_PATH=run path, ORCR_LOOP_DATA_DIR); stdin /dev/null; stdout/stderr line-tagged capture
- [x] run exit reaping: map exit code/signal → ok/failed/timeout/stopped; start oldest pending when slot frees
- [x] overlap queue (coalesce ≤1 pending scheduled) vs skip (drop + log)
- [x] per-run timeout: TERM pgid → grace → KILL pgid → glob-kill `<loop>/<run_id>/**`
- [x] stop path: `stopping` admission barrier → TERM → grace → KILL → glob-kill until clean snapshot
- [x] run.log JSONL writer {ts,stream,text} + size-cap + rotation + sidecar index
- [x] scheduler event rows: fired/coalesced/skipped/paused_hold/timed_out/stopped
- [x] missed fires skipped + logged (never replayed)
- [x] restart recovery: per-loop txn (verify pgids by start-time → close dead + glob-kill agents → recompute active → honor paused/ended → decide pending → recompute next_fire, skip missed)
- [x] signal only a pgid whose start time matches (pid reuse guard)

### Namespace protection & run scope
- [x] active-loop level-1 reservation: root/unrelated cannot create `nightly/foo` or `/nightly/foo` (invalid_request reserved_name)
- [x] a command inside `nightly/<run_id>` can create descendants
- [x] loop names rejected as agent level-1 always while active; reusable after ended
- [x] loop-run scope resolution: `caller_id` = a run → scope is full run path; ORCR_LOOP_DATA_DIR propagated to descendants
- [x] loop names themselves rejected if reserved (idle/unmanaged) or level-1 reserved by active loop

### Verbs / CLI
- [x] `loop create <name> ("<cron>"|--once-at) [--max-concurrency][--overlap][--timeout] -- <cmd...>` echo argv + cadence words + cancel cmd
- [x] `loop pause|resume <name>...`
- [x] `loop rm <name>... [--kill-active] [-y]` (TTY confirm; self-rm from run)
- [x] `loop ls [<name>...] [--status] [--all]`
- [x] `loop logs <name> [--run][--source orcr|command][--tail][--follow]` interleaved, `[<name>/<run_id>]` tagged
- [x] `loop run start <name>` → `<path> <run_uuid>`
- [x] `loop run stop <name> [<run_id|run_uuid>] [-y]` (TTY confirm)
- [x] `loop run ls <name> [--status][--all]`
- [x] register loop.* methods as implemented; add live handlers in server dispatch
- [x] server status: loops_firing + loops list + next fires

### server enable/disable (§6.4)
- [x] macOS launchd plist (`dev.orchestratr.orcr`, RunAtLoad, KeepAlive, absolute binary path, ORCR_HOME/ORCR_HERDR_BIN + log paths)
- [x] Linux systemd user unit (`orcr.service`, Restart=on-failure)
- [x] echo unit path + verify command; `unsupported_platform` (exit 2) elsewhere
- [x] disable removes registration; running server + store untouched
- [x] unit-file golden tests

## Acceptance criteria

- [x] DST boundary: "9am America/New_York weekdays" fires at 9am across both transitions (fixture clock)
- [x] Overlap cap 1 + slow runs → exactly one pending fire, later fires coalesce; skip drops with a log line
- [x] `loop run start` on a paused loop fires once; scheduled fires stay held
- [x] `loop run stop <name> <run_id>` kills one of two concurrent runs; the other survives; stopped run's agents glob-killed
- [x] Reboot simulation: kill server with running run + pending fire → restart → dead run closed, agents killed, pending fire decided once, missed cron fires skipped-and-logged
- [x] `loop logs --run` isolates one run's lines when two runs interleave
- [x] enable/disable round-trip: unit-file golden tests + launchctl/systemctl verification where available

## Deferred / out of scope
- top (M6), SDK loop helpers (M7)
- Windows Task Scheduler task (lands with Windows support, §17)
