# 03 · CLI surface

Design goal: the smallest vocabulary an LLM can learn from a skill and a human can learn
from `--help`. Verb-first (process model), like coreutils. Management nouns
(`schedule add/ls/rm`) nest under their verb.

## Identifiers (herdr-style, not UUIDs)

herdr addresses things as `w6:p3` — short, typed, structured. orcr does the same:

| Entity | Id form | Example |
| --- | --- | --- |
| agent | `a<N>` — monotonic counter, never reused | `a7` |
| loop | `l<N>` | `l2` |
| schedule | `s<N>` | `s1` |
| goal | `g<N>` | `g3` |
| workflow | `w<N>` | `w4` |
| turn (addressing sugar) | `<agent>:t<N>` | `a7:t2` |

- Counters are per-type, stored in sqlite, strictly increasing, never reused after
  kill/gc — so ids stay unambiguous in history.
- Anywhere an id is accepted, a `--name` label is too (names unique among live agents;
  reusing a live name is an error).
- `a7:t2` is accepted by `out` (and future verbs where a turn makes sense) as sugar for
  `--turn 2`.
- Run dirs are keyed by id: `~/.orcr/runs/a7/`.
- `w<N>` (workflow) does not collide with herdr's `w6:p3` workspace ids: they never appear
  in the same argument position; orcr never accepts herdr pane ids in its CLI.

## Verbs

```
orcr run   -a <harness> [-p <text> | --prompt-file <f>] [--name <label>]
           [--model <m>] [--effort <e>] [--cwd <dir>] [--timeout <s>]
           [--keep] [--mode tui|exec] [--worktree] [--parent <id>]
           [--reuse <id>] [--session <name>] [--wait] [--json]
orcr send  <id> [<text> | --prompt-file <f>] [--wait] [--json]
orcr wait  <id...> [--any] [--timeout <s>] [--json]
orcr out   <id | id:tN> [--turn N] [--path] [--recursive] [--paths] [--json]
orcr ps    [--json]
orcr tree  [<id>] [--watch] [--json]
orcr kill  <id...> [--tree] [--json]
orcr attach <id>
orcr status [--json]
orcr history [--since <dur>] [--name <l>] [--harness <h>] [--json]
orcr gc    [--dry-run] [--json]

orcr loop      -a <harness> (-p|--prompt-file) [--every <dur>|auto] [--tick-on <cmd>]
               [--max <n>] [--until <regex>] [--detach] [run-flags…]
orcr schedule  add (<cron> | --at <time>) -a <harness> (-p|--prompt-file)
               [--catchup skip|once] [--expires <dur>|--forever] [run-flags…]
orcr schedule  ls | pause <id> | resume <id> | rm <id> | from-loop <id>
orcr goal      -a <harness> (-p|--prompt-file) [--judge-agent <h>] [--judge-model <m>]
               [--max-iters <n>] [run-flags…]
orcr workflow  run <script.(ts|py|sh)> [--on-orphan kill|keep] [--json]
orcr top       [--pane] [--follow]
orcr events    [--follow] [--json]
orcr serve     [--install] [--foreground]
```

## Semantics quick-reference

- `run` is async by default → prints the new id (just the id on stdout when piped);
  `--wait` blocks until the turn completes and prints the response body.
- `run` auto-closes the pane after its first completed turn unless `--keep`.
- `send` to a **working** agent = steer of the current turn (one response); to an **idle**
  kept agent = next turn. To a `done`/dead agent = exit 6.
- `wait` default is all-of; `--any` returns on the first completion (prints its id).
- `out` default = latest response of that agent; `--recursive` walks descendants
  depth-first; with `--paths` prints `id<TAB>name<TAB>path` lines instead of bodies.
- `kill --tree` kills the subtree bottom-up (graceful recipe → pane close).
- `attach` hands this terminal to the live pane (returns when the user detaches).
- `status` never touches agents: herdr found/version, owned session state, daemon state,
  db health.

## Output discipline

- TTY: concise human output. Piped/`--json`: stable envelopes
  `{"ok":true,"result":…}` / `{"ok":false,"error":{"code":"…","message":"…"}}`.
- `--json` is available on every verb except pure-terminal ones (`attach`, `top`).
- Exit codes (global): `0` ok · `2` env/config (herdr missing, version skew) · `3` timeout
  · `4` blocked · `5` killed · `6` not found · `1` other.

## Examples

```sh
# a better `-p` for every harness — spawn, block, print response
orcr run -a codex --model gpt-5.2-codex -p "review this diff" --cwd ~/proj --wait

# fan-out / steer / fan-in / merge
orcr run -a claude -p "implement the parser" --name impl --keep
orcr run -a pi     -p "write docs for the parser" --name docs
orcr send impl "also handle escaped quotes"        # steers the running turn
orcr wait impl docs --timeout 1200
orcr out impl --recursive --paths

# jobs
orcr loop -a claude --prompt-file fix-tests.md --max 20 --until "ALL PASS"
orcr schedule add "0 9 * * 1-5" -a claude -p "triage new issues" --name triage
orcr goal -a claude -p "make the test suite pass" --judge-agent codex --max-iters 5
orcr workflow run ./parallel-review.ts
```
