# 06 · Jobs: loop, schedule, goal, workflow

Jobs are supervised by the daemon (`orcr serve`, auto-started — 02). Every job creation
prints: what runs, the human-readable cadence, expiry, and the exact `orcr kill <id>` to
cancel. Jobs re-use the full set of run-flags for the agents they spawn (model/effort/
cwd/worktree/…). Job ids: `l<N>`, `s<N>`, `g<N>`, `w<N>` (03).

These designs deliberately adopt six battle-tested patterns from Claude Code's /loop and
/schedule skills: prompt-file re-reading, event-gated ticks with fallback heartbeat,
self-pacing with a stated reason, durability escalation, auto-expiry with cancel hints,
and one-shot runs with timezone honesty.

## loop — `l<N>`

`orcr loop -a <h> (-p|--prompt-file f) [--every <dur>|auto] [--tick-on <cmd>] [--max n]
[--until <regex>] [--detach]`

- Each tick spawns (or `--reuse`s) an agent with the loop prompt.
- **`--prompt-file` is re-read every tick** — edit a running loop's instructions
  mid-flight without restarting it.
- Cadence:
  - `--every 5m` — fixed interval (measured from tick completion).
  - `--every auto` — **self-paced**: the preamble asks the agent to end its response with
    `NEXT_CHECK: <duration> — <reason>`; the supervisor parses it, clamps to
    [30s, 24h], and surfaces the reason in `ps`/`top`. Missing marker → fallback 10m.
  - `--tick-on "<cmd>"` — event-gated: run the probe command cheaply; tick fires when its
    exit code flips to 0 or its stdout changes; `--every` becomes the fallback heartbeat.
- Stop conditions: `--max` ticks, `--until` regex matched against the tick's response
  file, or `orcr kill l<N>`.
- Without `--detach` and with no daemon running, warn: "this loop stops when this process
  exits — use --detach for a daemon-supervised loop", mirroring Claude Code's escalation
  prompt. `orcr schedule from-loop l<N>` converts a loop into a schedule.

## schedule — `s<N>`

`orcr schedule add ("<cron>" | --at "<time>") -a <h> (-p|--prompt-file) [--catchup
skip|once] [--expires 30d|--forever] [--name]`

- Cron five-field, stored in UTC; **the tz the user created it in is stored** and every
  confirmation echoes both local and UTC ("9am PDT = 16:00 UTC").
- `--at` accepts RFC3339 or friendly forms ("tomorrow 09:00"); one-shots keep their row
  after firing (`ended_reason = fired`) and are re-armable via `schedule resume` with a
  new `--at`.
- Missed ticks (daemon down, laptop asleep): `--catchup skip` (default) or `once` (run a
  single make-up tick on daemon start).
- Default `--expires 30d`; expiry recorded as `ended_reason = expired`.
- `ls / pause / resume / rm` manage lifecycle.

## goal — `g<N>`

`orcr goal -a <h> (-p|--prompt-file) [--judge-agent <h>] [--judge-model <m>]
[--max-iters 5]`

Iterate worker → judge → feedback until pass:

1. Worker agent (kept alive) runs the task.
2. Judge agent evaluates: gets the goal text + worker's latest response (+ `out
   --recursive` of the worker subtree) and must answer with a first line `PASS` or
   `FAIL: <reasons>` (enforced via the judge preamble).
3. `FAIL` reasons are `send`-steered back into the SAME worker session; repeat.
4. Ends on PASS (`done`), `--max-iters` (`failed`, exit_reason `goal-max-iters`), or kill.

**Judge defaults to the worker's harness+model** (independent-eyes via
`--judge-agent codex` is the recommended cross-harness pattern, but not forced).

## workflow — `w<N>`

`orcr workflow run <script.(ts|py|sh)> [--on-orphan kill|keep]`

- Runs the script as a child process with the env contract set (ORCR_ID=w<N>,
  ORCR_PARENT if nested) — every `orcr run` inside auto-parents to the workflow node.
- The script is the workflow: any language, using the CLI (or SDK sugar — 08).
- Script exit 0 → `done`; nonzero → `failed`. Children still running at exit follow
  `--on-orphan` (default `kill`).
- stdout/stderr captured to the workflow's run dir (`w4/log.txt`); the script path and
  argv recorded in `spec_json` for future replay (10).
