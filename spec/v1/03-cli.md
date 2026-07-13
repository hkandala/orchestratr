# 03 · CLI surface

Design goal: the smallest vocabulary an LLM can learn from a skill and a human can learn
from `--help`. Verb-first (process model). Reviewed against tmux/docker/kubectl/gh/git
conventions; adjudicated outcomes in [decisions.md](decisions.md).

## Identifiers (herdr-style, not UUIDs)

herdr addresses things as `w6:p3` — short, typed, structured. orcr does the same:

| Entity | Id form | Example |
| --- | --- | --- |
| agent | `a<N>` — monotonic counter, never reused | `a7` |
| loop | `l<N>` | `l2` |
| schedule | `s<N>` | `s1` |
| goal | `g<N>` | `g3` |
| workflow | `w<N>` | `w4` |
| turn (interactive sugar) | `<agent>:t<N>` | `a7:t2` |

- Counters per-type in sqlite, strictly increasing, never reused — ids stay unambiguous
  in history.
- Anywhere an id is accepted, a live `--name` label is too. **Typed-id patterns
  (`^[alsgw]\d+$`, `:t\d+`) are reserved and rejected as names**; an ambiguous positional
  errors with a disambiguation hint rather than guessing.
- `a7:t2` is interactive sugar for `--turn 2`, accepted only where a turn is meaningful
  (`out`, `show`). The skill teaches `--turn`, not the sugar.
- Run dirs are keyed by id: `~/.orcr/runs/a7/`.

## Verbs

```
orcr run    --harness|-a <h> [-p <text> | --prompt-file <f|->] [--name <label>]
            [--model <m>] [--effort <e>] [--cwd <dir>] [--timeout <dur>]
            [--keep] [--mode tui|exec] [--worktree] [--parent <id>]
            [--session <name>] [--wait] [--json]
orcr send   <id> [<text> | --prompt-file <f|->] [--steer | --turn] [--wait] [--json]
orcr wait   <id...> [--any] [--tree <id>] [--timeout <dur>] [--json]
orcr out    <id | id:tN> [--turn N] [--recursive] [--format body|path|json] [--json]
orcr show   <id> [--json]              # canonical single-object state
orcr ps     [--json]
orcr tree   [<id>] [--watch] [--json]
orcr kill   <id...> [--tree] [--json]  # stops execution (graceful → forceful)
orcr attach <id>
orcr status [--json]
orcr history [--since <dur>] [--status <st>] [--parent <id>] [--name <l>]
             [--harness <h>] [--limit N] [--json]
orcr gc     [--dry-run] [--json]

orcr loop      --harness <h> (-p|--prompt-file) [--every <dur>|auto] [--tick-on <cmd>]
               [--max <n>] [--until <regex>] [--foreground] [run-flags…]
orcr schedule  add ("<cron>" | --at <time>) --harness <h> (-p|--prompt-file)
               [--catchup skip|once] [--expires <dur>] [run-flags…]
orcr schedule  resume <id> [--at <time>]      # fired one-shot requires --at
orcr goal      --harness <h> (-p|--prompt-file) [--judge-harness <h>]
               [--judge-model <m>] [--max-iters <n>] [run-flags…]
orcr workflow  run <script.(ts|py|sh)> [--on-orphan kill|keep] [--json]
orcr job       ls | show <id> | pause <id> | resume <id> | rm <id>   [--json]
orcr top       [--pane]
orcr events    [--follow] [--json]
orcr serve     [--foreground] · serve install (M4)
```

`--harness` is the canonical long flag (`-a` short alias). Durations everywhere accept
suffix strings (`45s`, `20m`, `3h`, `30d`); a bare number means seconds. Milliseconds
never appear in the user CLI.

## Semantics quick-reference

- `run` is async by default → prints the new id; `--wait` blocks through the first turn.
  Auto-closes after that turn unless `--keep`.
- `send`:
  - to a **working** agent = **steer** of the current turn (one response);
  - to an **idle kept** agent = **next turn**;
  - `--steer` / `--turn` pin the intent — mismatch with live state → `state_conflict`
    (exit 7). Bare `send` resolves by state and reports `"mode"` in JSON. Scripts and the
    skill use intent flags; humans can stay bare.
  - to a done/dead agent → exit 6 (id unknown → 6; known but ended → 7).
- `wait`: all-of default; `--any` returns on first completion; `--tree <id>` waits on all
  live descendants of a node. On timeout: exit 3, JSON lists
  completed/pending/blocked.
- `out`: default latest response body; `--turn N` or `id:tN` selects a turn;
  `--recursive` walks descendants depth-first; `--format path` prints
  `id<TAB>name<TAB>path` lines (with `--recursive`: one per descendant).
- `show` is THE state query: identity, status, turns (with paths), children ids, timings,
  model/effort, cwd, pane ref, exit_reason.
- `kill` stops execution: graceful per-harness recipe (~5s deadline) → pane close;
  `--tree` bottom-up. Deleting a job *definition* is `job rm` (alias `schedule rm`).
- `attach` hands this terminal to the live pane; returns on detach.
- Lifecycle-invalid operations → exit 7 / `state_conflict` with
  `{current_status, wanted, id}` in error details.

## Output discipline

- TTY: concise human output. `--json`: **exactly one JSON object on stdout**;
  all logs/progress to stderr.
- Envelopes: `{"ok":true,"result":…}` / `{"ok":false,"error":{"code":…,"message":…,
  "details":{…}}}`.
- Exception: `events --follow --json` streams NDJSON — one `{type,id,time,payload}`
  object per line. One-shot `events --json` returns a single envelope.
- Exit codes: `0` ok · `2` env/config · `3` timeout · `4` blocked · `5` killed ·
  `6` not found · `7` state conflict · `1` other.

### JSON result shapes (stable; the contract for the SDK)

```
run          {agent:{id,name,harness,model,status,…}, turn:{n,prompt_path},
              paths:{run_dir,response}, permissions:"bypass"}
run --wait   + response:{text,path,source:"file|transcript|scrape"}
send         {id, mode:"steer"|"turn", turn:{n,prompt_path}} (+response with --wait)
wait         {completed:[ids], pending:[ids], blocked:[ids], timed_out:bool}
out          {items:[{id,name,turn,path,source,text?}]}   # text omitted for --format path
show         {agent:{…full row…}, turns:[…], children:[ids]}
ps/history   {agents:[{…}]} / {items:[{…}]}
tree         {roots:[{id,name,status,children:[…recursive…]}]}
kill         {killed:[ids], skipped:[{id,reason}]}
job ls       {jobs:[{id,type,status,cadence,next_run,…}]}
```

## Examples

```sh
# a better `-p` for every harness — spawn, block, print response
orcr run --harness codex --model gpt-5.2-codex -p "review this diff" --cwd ~/proj --wait

# fan-out / steer / fan-in / merge (canonical automation path: run → wait → out)
orcr run -a claude -p "implement the parser" --name impl --keep
orcr run -a pi     -p "write docs for the parser" --name docs
orcr send impl --steer "also handle escaped quotes"
orcr wait impl docs --timeout 20m
orcr out impl --recursive --format path

# jobs
orcr loop -a claude --prompt-file fix-tests.md --max 20 --until "ALL PASS"
orcr schedule add "0 9 * * 1-5" -a claude -p "triage new issues" --name triage
orcr goal -a claude -p "make the test suite pass" --judge-harness codex --max-iters 5
orcr workflow run ./parallel-review.ts
orcr job ls
```
