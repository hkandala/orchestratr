# orcr CLI v2 — simplified design draft (for review)

orcr is a cross-harness agent orchestrator built ON herdr (a terminal workspace manager
for AI agents). Any coding agent (claude/codex/pi/opencode) — or a human — uses orcr to
spawn, steer, await, and supervise agents on any harness as REAL interactive TUI sessions
in herdr panes: plan-pricing-safe, attachable, steerable, visible.

This is a ground-up simplification of the v1 CLI. Goals:

1. **Seamless herdr integration is the moat.** orcr agents live in the user's normal
   herdr session, visible in the herdr UI, one keystroke from attach. orcr also SEES
   everything herdr sees: `orcr ls` lists ALL agents in the session, including ones the
   user started by hand in a herdr pane (never spawned via orcr), and orcr can send /
   wait / attach / kill those too.
2. **Minimal primitives.** Cut loop / goal / workflow / job as commands. Keep only the
   primitives that cannot be rebuilt from the outside: spawn, message, await, read,
   observe, stop, place, schedule (durability needs the daemon). Everything else is a
   script/SDK/skill recipe on top.
3. **Crystal-clear placement model** for sessions / workspaces / tabs / panes / cwd, with
   defaults that adapt to WHERE orcr is invoked from (inside a herdr pane vs a plain
   terminal) — but stay predictable and overridable.

## 1. Concept mapping (herdr → orcr)

herdr hierarchy: **Session → Workspace → Tab → Pane → Agent**.

| herdr concept | what it is | orcr's stance |
| --- | --- | --- |
| Session | a persistent herdr server namespace (separate state, sockets, panes) | orcr targets exactly ONE session per invocation. Default: the session you're in (inside herdr), else config `herdr.session`, which defaults to herdr's default session — NOT a hidden side session. `--session <name>` / `ORCR_SESSION` override. |
| Workspace | top-level project container (typically one per repo/task); has a default cwd; sidebar aggregates agent attention per workspace | placement unit. Resolved from cwd by default (see §2). `--workspace <id|label|new>` overrides. |
| Tab | addressable layout unit inside a workspace | **one tab per orcr agent** (default). Tab label = agent name/id. |
| Pane | a real terminal | the agent's TUI lives in the tab's single pane; `--split` opts into pane-splitting instead of a new tab. pane_id remains the only stable low-level handle. |
| Agent | a process herdr recognizes inside a pane (states: working/idle/blocked/done/unknown) | orcr's completion detection rides herdr agent states (working→idle). Foreign (non-orcr) agents are first-class read/steer targets. |
| Worktree | herdr-managed git worktree bound to a workspace | `run --worktree [<branch>]` provisions via herdr and runs the agent inside it. |

**Workspace ≠ cwd.** The agent's *process cwd* and its *placement in the herdr UI* are
two different axes, controlled independently:

- `--cwd <dir>` sets where the agent process runs. Default: the CALLER's cwd. Always.
- `--workspace` sets where its tab appears. Default: derived (see matrix below); never
  changes the process cwd.

## 2. Placement defaults — the context matrix

orcr detects its calling context from env (`HERDR_PANE_ID` / `HERDR_WORKSPACE_ID` /
`HERDR_SESSION` are set inside herdr-managed panes; `ORCR_ID` is set inside
orcr-spawned agents).

| caller context | session | workspace | tab/pane | cwd | focus |
| --- | --- | --- | --- | --- | --- |
| **A. human in a plain terminal** (no herdr env) | config `herdr.session` (default: herdr's default). If that session's server isn't running, orcr starts it headless (`herdr server`) — attach later with plain `herdr`. | match an existing workspace whose cwd equals the agent cwd (else nearest ancestor match); else CREATE one labeled after the directory | new tab, label = agent name | caller cwd (or `--cwd`) | never steal (`--focus` opt-in) |
| **B. agent (e.g. Claude Code) in a plain terminal** (no herdr env, maybe `ORCR_*` unset) | same as A | same as A | same as A | same as A | n/a (no herdr client focus to steal) |
| **C. human at a herdr pane** (herdr env set) | current session | current workspace | new tab in current workspace | pane cwd | no focus steal; herdr sidebar/badges show activity |
| **D. orcr-spawned agent spawning a subagent** (herdr env + `ORCR_*` set) | same session | parent's workspace | new tab NEXT TO the parent's tab; `--split right|down` puts it in the parent's tab as a split instead | parent's cwd (inherited) unless `--cwd` | never |
| **E. foreign agent in a herdr pane running orcr** (herdr env set, no `ORCR_*`) | current session | current workspace | new tab | pane cwd | never |

Notes:

- Row A/B is the "user runs claude code in iTerm and it spawns a codex reviewer"
  scenario: the reviewer appears in the user's herdr — instantly attachable — even
  though the caller isn't in herdr at all.
- Workspace-by-cwd matching means repeated spawns from the same repo pile into one
  workspace instead of spraying new ones. `--workspace new` forces a fresh one;
  `--workspace <label|id>` targets a specific one.
- Every placement decision is reported in the spawn result (`placement: {session,
  workspace, tab, pane}`) so callers/scripts never have to guess.
- The v1 auto-viewer ("spawn opens an `orcr top` pane") is DROPPED — with agents living
  in the user's real session, herdr's own UI is the viewer. `orcr top` stays as an
  explicit command.

## 3. Identifiers

- orcr-managed agents: `a<N>` (monotonic, never reused) + optional `--name` label.
  Schedules: `s<N>`. (l/g/w id types are gone with their commands.)
- Turn sugar `a7:t2` kept for `out`/`show`.
- **Foreign agents** (herdr-detected, not orcr-managed) are addressed by their herdr
  identity — terminal id, agent name/label — passed through as-is. Every verb that takes
  a target resolves: orcr id → orcr name → herdr agent target. Collisions error with a
  disambiguation hint.
- Reserved name grammar unchanged (`^[as]\d+$` rejected as names).

## 4. The verb set (14 → the whole surface)

```
orcr run      -a <harness> [-p <text> | -f <file|->] [flags] [--detach]
orcr ls       [--all] [--tree] [--foreign|--managed] [--json]
orcr show     <id> [--json]
orcr send     <id> [<text> | -f <file|->] [--steer | --turn] [--wait] [--json]
orcr wait     <id...> [--any] [--tree] [--timeout <dur>] [--json]
orcr out      <id | id:tN> [--turn N] [--recursive] [--format body|path|json]
orcr attach   <id>
orcr kill     <id...> [--tree] [--json]
orcr adopt    <herdr-target> [--name <label>] [--json]
orcr schedule add|ls|show|pause|resume|rm   (the ONE job primitive)
orcr top      [--pane]
orcr status   [--json]
orcr daemon   start|stop|status
orcr gc       [--dry-run] [--json]
```

(plumbing, undocumented in the skill: `orcr events [--follow]` for the TUI/SDK)

### run — spawn an agent

```
orcr run -a <harness> [-p <text> | -f <file|->]
         [--name <label>] [--model <m>] [--effort <e>]
         [--cwd <dir>] [--worktree [<branch>]]
         [--session <s>] [--workspace <id|label|new>] [--split right|down] [--focus]
         [--keep] [--timeout <dur>] [--mode tui|exec]
         [--detach|-d] [--parent <id>] [--json]
```

- **Waits by default** and prints the response body to stdout (the "better `-p` for every
  harness"). `--detach/-d` returns immediately, printing the new id — the fan-out path.
  (CHANGED from v1's async-default; review question R1.)
- Auto-closes after the first completed turn unless `--keep`. `--keep` implies you'll
  `send --turn` more work later; kept agents are reaped after config `idle_reap_min`.
- Placement flags (`--session/--workspace/--split/--focus`) per §2. `--cwd` is process
  dir only.
- `--worktree` provisions a herdr worktree in the resolved workspace and runs there.
- Env contract injected (ORCR_ID/PARENT/DEPTH/STORE/OUT) → children auto-record lineage.
- Prompt/response file contract unchanged (runs/<id>/NNN-prompt.md, NNN-response.md,
  preamble, response guarantee file→transcript→scrape).

### ls — the unified list (replaces ps/history/tree)

```
orcr ls [--all] [--tree] [--managed|--foreign] [--harness <h>] [--since <dur>] [--json]
```

- Default: live agents in the target session — **orcr-managed AND foreign** (from
  `herdr agent list`), one table: id/target, name, harness, status, workspace, age,
  last-activity. Foreign rows show their herdr target as the id and `origin: herdr`.
- `--all` includes ended agents (v1 `history`). `--tree` renders lineage (v1 `tree`);
  foreign agents are roots. Filters compose.

### show — one object, everything

State card for one agent: identity, origin (orcr/foreign), status, turns + paths,
children, placement (session/workspace/tab/pane), cwd, model, timings, exit_reason.
For foreign agents: herdr-known fields + pane info; no turns/run-dir unless adopted.

### send / wait / out / attach / kill

Same battle-tested semantics as v1 (steer-while-working vs new-turn-when-idle-kept,
intent flags, exit 7 on conflict; wait --any/--tree; out --recursive; kill graceful →
pane close, --tree bottom-up) — but now **foreign targets work too**:

- `send <herdr-target>` = pane send-text + enter (best-effort; no run-dir contract).
- `wait <herdr-target>` = herdr agent-status wait (working→idle).
- `out <herdr-target>` = transcript adapter if the harness is recognized, else pane
  scrape; source recorded.
- `attach <id>` focuses the pane in herdr if you're a herdr client, else hands this
  terminal over (herdr terminal session control).
- `kill` on a foreign target requires `--force` (we didn't start it; safety).

### adopt — bring a foreign agent under management

```
orcr adopt <herdr-target> [--name <label>]
```

Assigns an `a<N>` id, starts turn tracking + run-dir from the NEXT turn, enables the
full contract (send --turn, out, wait) on an agent the user started by hand. This is
the bridge that makes "orcr works with everything already in herdr" real, without
pretending we can reconstruct history we never saw.

### schedule — the one retained job primitive

```
orcr schedule add ("<cron>" | --every <dur> | --at <time>)
                  -a <harness> (-p|-f) [--max <n>] [--until <regex>]
                  [--catchup skip|once] [--expires <dur>] [run-flags…]
orcr schedule ls | show <id> | pause <id> | resume <id> [--at <time>] | rm <id>
```

- Absorbs v1 `loop`: `--every 15m --max 20 --until "ALL PASS"` IS a loop. One mental
  model: "run this agent on a cadence until a stop condition."
- Daemon-backed (auto-start), durable, `--catchup` for missed ticks, tz-honest
  confirmations. Ids `s<N>`.
- WHY it survives the cut: durability across caller death genuinely cannot be built
  from run/wait by a script that exits. Everything else can.

### CUT from the command surface (→ recipes)

| v1 command | replacement |
| --- | --- |
| `loop` | `schedule add --every … --max … --until …` (durable) or a 5-line shell/SDK loop (foreground) |
| `goal` (worker/judge iterate) | SDK/skill recipe: `run --keep` worker + `run` judge + `send --turn` feedback loop. Ships as a documented recipe + SDK helper, not a verb. |
| `workflow run <script>` | just run the script. The env contract already auto-parents every `orcr run` inside it. Grouping/orphan-cleanup sugar moves to the SDK (`withGroup()`), not the CLI. |
| `job ls/show/pause/resume/rm` | `schedule …` subcommands (only one job type remains) |
| `ps` / `tree` / `history` | `ls` / `ls --tree` / `ls --all` |
| `serve` | `daemon start|stop|status` (auto-start still the norm; `status` shows health) |
| auto-viewer pane | dropped; herdr UI is the viewer; `orcr top` explicit |

## 5. Output discipline, exit codes (unchanged from v1)

TTY human output; `--json` = exactly one envelope object on stdout
(`{"ok":true,"result":…}` / `{"ok":false,"error":{code,message,details}}`); events
--follow streams NDJSON. Exit: 0 ok · 2 env · 3 timeout · 4 blocked · 5 killed · 6 not
found · 7 state conflict · 1 other. Durations human-form (`45s`, `20m`); never ms.

## 6. Config (herdr-style TOML)

```toml
[defaults]  harness "claude" · model "" · effort "" · timeout "10m" · keep false
[limits]    max_depth 3 · max_agents_per_tree 10 · max_concurrent 4 · idle_reap "15m"
[herdr]     bin "" · session ""        # "" = default session; set for isolation
[placement] workspace "auto"           # auto | current | new | <label>
            agent_container "tab"      # tab | split
```

## 7. Review questions (answer explicitly)

R1. `run` waits by default (paseo-style, prints response) vs v1 async-default (prints
    id). Which is right for BOTH human and LLM callers? Cost of the flip?
R2. Is one-tab-per-agent the right default container vs splits? Tab sprawl on wide
    fan-outs (10 agents = 10 tabs)?
R3. Default session = user's default herdr session (visible, moat) vs dedicated `orcr`
    session (isolated, v1 behavior). Risks of sharing the user's session?
R4. Is `adopt` pulling its weight, or should foreign targets just work degraded
    everywhere with no adoption step?
R5. Is cutting goal/workflow/loop right? Anything that genuinely needs daemon
    supervision besides schedule?
R6. Workspace-by-cwd matching: exact match, ancestor match, or git-root match? What
    creates surprises?
R7. `ls` merging managed + foreign by default: right call, or should foreign be opt-in?
R8. Naming: `ls` vs `list`; `out` vs `logs`; `adopt`; `daemon` vs `server`. herdr uses
    `list`/`server`; paseo uses `ls/logs/daemon`.
R9. Anything in §4 still cuttable? Anything cut that will bite?
