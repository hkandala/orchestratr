# 06 · Jobs: loop, schedule, goal, workflow

Jobs are **always daemon-supervised**: creating one auto-starts `orcr serve` (02). Every
creation prints: what runs, human-readable cadence, persistence/expiry, and the exact
cancel command. Jobs reuse run-flags for the agents they spawn. Ids: `l<N>`, `s<N>`,
`g<N>`, `w<N>`.

Management is uniform across job types: `orcr job ls | show | pause | resume | rm <id>`
(`schedule …` forms are aliases). `kill <id>` stops a job's execution; `job rm` deletes
its definition.

These designs adopt six battle-tested patterns from Claude Code's /loop and /schedule
skills: prompt-file re-reading, event-gated ticks with fallback heartbeat, self-pacing
with a stated reason, durability by default, cancel hints on creation, and one-shot runs
with timezone honesty.

## loop — `l<N>`

`orcr loop --harness <h> (-p|--prompt-file) [--every <dur>|auto] [--tick-on <cmd>]
[--max n] [--until <regex>] [--foreground]`

- Each tick spawns an agent with the loop prompt (fresh agent per tick; kept-agent reuse
  is a possible later optimization, deliberately not in v1).
- **`--prompt-file` is re-read every tick** — edit a running loop's instructions
  mid-flight without restarting.
- Cadence:
  - `--every 5m` — fixed interval, measured from tick completion.
  - `--every auto` — self-paced: the preamble asks the agent to end its response with
    `NEXT_CHECK: <duration> — <reason>`; supervisor parses, clamps to [30s, 24h],
    surfaces the reason in `ps`/`top`/`job show`. Missing marker → fallback 10m.
  - `--tick-on "<cmd>"` — event-gated: cheap probe; tick fires when exit code flips to 0
    or stdout changes; `--every` becomes the fallback heartbeat.
- Stop: `--max` ticks, `--until` regex on the tick's response file, `orcr kill l<N>`.
- `--foreground` runs a non-durable loop in this terminal (experiments); everything else
  is daemon-supervised — there is no `--detach`, durability is the default.

## schedule — `s<N>`

`orcr schedule add ("<cron>" | --at "<time>") --harness <h> (-p|--prompt-file)
[--catchup skip|once] [--expires <dur>] [--name]`

- Cron five-field, stored in UTC; **the creating timezone is stored** and every
  confirmation echoes both ("9am PDT = 16:00 UTC").
- **Recurring schedules run forever by default** (cron expectation); `--expires 30d`
  opts into auto-expiry (`ended_reason = expired`).
- `--at` accepts RFC3339 or friendly forms ("tomorrow 09:00"); one-shots end after
  firing (`ended_reason = fired`) and re-arm via `schedule resume <id> --at <time>`
  (resume without `--at` on a fired one-shot → state_conflict, exit 7).
- Missed ticks (daemon down, laptop asleep): `--catchup skip` (default) | `once` (one
  make-up tick on daemon start).

## goal — `g<N>`

`orcr goal --harness <h> (-p|--prompt-file) [--judge-harness <h>] [--judge-model <m>]
[--max-iters 5]`

Iterate worker → judge → feedback until pass:

1. Worker agent (kept alive) runs the task.
2. Judge agent evaluates: goal text + worker's latest response (+ subtree summaries);
   must answer first-line `PASS` or `FAIL: <reasons>` (enforced via judge preamble).
3. `FAIL` reasons are steered back into the SAME worker session (`send --turn`); repeat.
4. Ends on PASS (`done`), `--max-iters` (`failed`, exit_reason `goal-max-iters`), or kill.

**Judge defaults to the worker's harness+model** — a deliberate convenience default. When
defaulted, output labels the evaluation `self-check` and JSON carries
`judge_independent:false`; `--judge-harness codex` is the recommended independent-eyes
pattern (and the headline cross-harness demo).

## workflow — `w<N>`

`orcr workflow run <script.(ts|py|sh)> [--on-orphan kill|keep]`

- Runs the script as a child process with the env contract set (ORCR_ID=w<N>) — every
  `orcr run` inside auto-parents to the workflow node.
- The script IS the workflow: any language, using the CLI (or SDK sugar — 08).
- Exit 0 → `done`; nonzero → `failed`. Children alive at exit follow `--on-orphan`
  (default `kill`).
- stdout/stderr → `runs/w4/log.txt`; script path + argv recorded in `spec_json` for
  future replay (10).
