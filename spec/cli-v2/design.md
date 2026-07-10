# orcr CLI v2 — final design (post-review)

Ground-up simplification of the v1 CLI, designed against the herdr / paseo / orca CLI
references and hardened through four independent design reviews (transcripts:
`review-1..4.md`; pre-review draft: `cli-v2-draft.md`; rendered commentable reference:
`orcr-cli-reference.html`). This file is the authoritative markdown version.

Status: **design locked pending owner review comments; not yet implemented.** The v1
implementation and `spec/01..10` still describe the shipped v1 surface.

## 1 · The mental model

1. **orcr spawns real agent TUIs in the user's real herdr session.** Every agent is a
   visible tab, one keystroke from attach. No hidden side session — the herdr UI *is* the
   viewer.
2. **orcr sees everything herdr sees.** `orcr ls` lists hand-started herdr agents alongside
   orcr-managed ones; `send / wait / out / attach` work on them too, with an honest,
   machine-readable downgrade of guarantees (contract levels, §3).
3. **Two independent axes:** `--cwd` = where the agent process runs; `--workspace` = where
   its tab appears. Defaults: cwd = caller's cwd, workspace = derived from cwd. Neither
   silently changes the other.
4. **`run` waits and prints the answer** — the better `-p` for every harness. Fan-out is
   explicit: `--detach`.
5. **Files are the inter-agent API.** Prompts/responses are markdown under `runs/<id>/`;
   after `done` the response file always exists; provenance (`file|transcript|scrape`)
   recorded.
6. **One durable job primitive: `loop`.** Cron, interval, or once-at cadence with a stop
   rule. Goals, judges, workflows = recipes over `run/send/wait/out`, not verbs.
7. **Nine core commands** (`run ls show send wait out attach kill loop`), three advanced
   (`adopt daemon top`), three plumbing (`status gc events`).

## 2 · Placement: sessions, workspaces, tabs, panes, cwd

herdr hierarchy: **Session → Workspace → Tab → Pane → Agent**.

| herdr concept | orcr's stance |
| --- | --- |
| Session | one session per invocation: the session you're in when inside herdr, else config `herdr.session` — default **herdr's default session** (not a hidden side session). Override `--session` / `ORCR_SESSION`. `session = "orcr"` in config restores full isolation. |
| Workspace | the placement unit; resolved from the agent's cwd (git-root-first, order below); `--workspace <id|label|new>` overrides |
| Tab | **one tab per agent**, labeled name/id, parent-prefixed on fan-outs (`impl/a8`) |
| Pane | the agent TUI is the tab's single pane; `--split right|down` opts into splitting the caller's tab; `pane_id` stays the only stable low-level handle |
| Agent | completion rides herdr states (`working → idle`), then the response file is finalized; foreign agents are first-class read/steer targets |
| Worktree | `run --worktree [<branch>]` provisions via herdr; cwd becomes the worktree path (conflicting `--cwd` errors); placement = the worktree's workspace |

**Workspace ≠ cwd.** A workspace *has* a default cwd but is a UI container, not a
directory. `--cwd` controls the process; `--workspace` controls the tab's home.

### Context matrix (defaults per caller)

| caller | session | workspace | tab | cwd |
| --- | --- | --- | --- | --- |
| human/agent in a plain terminal | config session (herdr default); server auto-started headless if down (`herdr_server: existing\|started\|failed` reported) | resolved from cwd; created if no match | new tab | caller's cwd |
| human at a herdr pane | current session | current workspace | new tab | pane cwd |
| orcr-spawned agent spawning a subagent | same session | parent's workspace | new tab `parent/child`; `--split` to sit beside parent | parent's cwd |
| foreign agent in a herdr pane running orcr | current session | current workspace | new tab | pane cwd |

Focus is never stolen (`--focus` opt-in). Every spawn result reports resolved
`{session, workspace, tab, pane}` + `workspace_resolution`.

### Workspace resolution order

1. Explicit `--workspace <id|label>` (or `new`).
2. Inside herdr with no `--cwd` override → caller's current workspace.
3. Existing workspace whose cwd == **git worktree root** of the agent cwd (canonical
   physical paths).
4. Existing workspace whose cwd == exact cwd.
5. Nearest ancestor workspace, only within the same git worktree.
6. Create at git root (or cwd when not in git), labeled after the directory.

Reported as `workspace_resolution: explicit|current|git_root|exact_cwd|ancestor|created`.

### Ownership (what makes session-sharing safe)

- Every orcr-created workspace/tab/pane carries a durable machine-readable marker (pane
  env + herdr agent metadata, orcr as source).
- `gc`, idle reaping, auto-close, `kill --tree` touch **only marked resources**.
- Destructive ops on foreign targets require `--force`.
- Full isolation remains one config line.

## 3 · Targets, ids & contract levels

| form | example | meaning |
| --- | --- | --- |
| `a<N>` | `a7` | orcr-managed agent; monotonic, never reused |
| `l<N>` | `l2` | loop (durable job) |
| name | `impl` | live `--name` label; reserved grammar `^[al]\d+$`, `:t\d+`, `new` rejected as names; live-name reuse rejected |
| herdr target | `codex-2` | foreign agent by herdr identity, passed through |
| `a7:t2` | | turn sugar for `out`/`show` (= `--turn 2`) |
| canonical | `orcr:a7` · `name:impl` · `herdr:codex-2` | unambiguous forms for scripts |

Bare targets resolve `orcr id → orcr name → herdr target`; collisions error with every
match listed (`ambiguous_target`).

### Contract levels

| contract | who | guarantees |
| --- | --- | --- |
| `managed` | orcr-spawned | everything: run-dir, turns, lineage, response guarantee, steer/turn, clean kill |
| `adopted` | after `orcr adopt` | full contract from the next orcr-initiated turn; prior output stays `untracked` |
| `foreign` | herdr-detected, not orcr-started | best-effort read/steer/wait/attach; `out` via transcript adapter or pane scrape (source recorded); no run-dir/turns; kill needs `--force` |

Every JSON row/object carries `origin`, `contract`, `capabilities`. Missing capability →
`capability_unavailable` + the missing key. `out --require-source file|transcript` turns
silent fallback into loud failure.

## 4 · Global conventions

- TTY: concise human output. `--json`: exactly one envelope on stdout
  (`{"ok":true,"result":…}` / `{"ok":false,"error":{code,message,details}}`); logs to
  stderr. Exception: `events --follow --json` streams NDJSON.
- Exit codes: `0` ok · `2` environment · `3` timeout · `4` blocked · `5` killed ·
  `6` not found · `7` state conflict · `1` other.
- Stable error codes: `ambiguous_target`, `state_conflict` (+ `current_status` + valid
  next commands), `capability_unavailable`, `herdr_unreachable`, `placement_failed`,
  `limit_exceeded`, `lost_pane`, `lost_session`.
- Durations require units (`45s 20m 3h 30d`); bare numbers rejected with a hint; ms never
  appear in the CLI.
- Placement echo on every creating/moving command.
- `status --json` carries `version`, `schema_version`, `features` (capability probe).
- JSON identity reserves `host` (always `local` today) for future remote support.

## 5 · Command reference

Tiers: **core** (skill-taught): run ls show send wait out attach kill loop ·
**advanced**: adopt daemon top · **plumbing**: status gc events.

### run — spawn an agent (core)

```
orcr run -a <harness> [-p <text> | -f <file|->]
         [--name <label>] [--model <m>] [--effort <e>]
         [--cwd <dir>] [--worktree [<branch>]]
         [--session <s>] [--workspace <id|label|new>] [--split right|down] [--focus]
         [--keep] [--timeout <dur>] [--detach|-d]
         [--parent <id>] [--dry-run] [--mode tui|exec] [--json]
```

- **Waits by default**: blocks through the first turn, prints the response body, exits by
  outcome (0 done · 3 timeout · 4 blocked · 5 killed). Completion = response file
  finalized, never herdr-idle alone.
- `--detach/-d` prints the new id immediately; `--json` always includes the id.
- Auto-closes after the response is finalized (never before); `close_reason` recorded.
  `--keep` → idle worker for `send --turn`; reaped after config `idle_reap`.
- `--parent` must be a live managed/adopted agent in the same session; depth/tree caps and
  cycles rejected pre-spawn. Inside an orcr agent, lineage is automatic via env
  (`ORCR_ID/PARENT/DEPTH/STORE/OUT` injected into every spawn).
- `--dry-run`: resolve placement + validate, spawn nothing.
- `--mode exec` (advanced): headless invocation, still inside a herdr pane, registered via
  pane report-agent with orcr as source.

Result (abridged): `{id, name, harness, origin, contract, status, host, placement{…},
workspace_resolution, herdr_server, turn{n, prompt_path}, response{text, path, source},
kept, close_reason, permissions:"bypass"}`.

### ls — the unified list (core)

```
orcr ls [--tree] [--all] [--status <s>] [--managed|--foreign]
        [--harness <h>] [--since <dur>] [--json]
```

- Default: live agents in the target session, **managed and foreign merged**; ORIGIN
  column always shown; foreign rows never look like `a<N>`.
- `--tree` = lineage (replaces v1 `tree`); `--all` = include ended (replaces `history`);
  `--status blocked` = the "who needs me" filter. Filters compose.
- JSON rows carry `origin`/`contract`/`capabilities`.

### show — one object, everything (core)

`orcr show <target|target:tN> [--json]` — identity, origin/contract/capabilities, status,
placement, cwd, model/effort, timings, exit/close reasons, turns (paths + sources),
children, parent. Shares one schema with `ls` rows (`show` = expanded form).

### send — steer or next turn (core)

```
orcr send <target> [<text> | -f <file|->] [--steer | --turn] [--raw] [--wait] [--json]
```

- Working agent → **steer** (`NNN-prompt.K.md`, one response; completion re-arms after the
  last input). Blocked agents accept steers.
- Idle kept agent → **next turn**.
- Bare send resolves by state and reports what it did; scripts/skill pin `--steer/--turn`;
  mismatch → exit 7 with `current_status` + valid alternatives.
- Foreign: works on herdr-recognized agents; unrecognized panes require `--raw`.
- Ended target → 7; unknown → 6.

### wait — fan-in (core)

```
orcr wait <target...> [--any] [--tree] [--timeout <dur>] [--json]
```

- All-of default; `--any` first completion; `--tree` includes live descendants.
- Managed: completes on response-file finalization. Foreign: rides herdr states;
  already-idle returns immediately (noted); else next stable working→idle.
- Timeout → 3 with `completed/pending/blocked`; blocked → 4 with `block_reason`;
  vanished pane/session → `lost_pane`/`lost_session` promptly, never a hang.

### out — read the answer (core)

```
orcr out <target|target:tN> [--turn N] [--recursive]
         [--format body|path|json] [--require-source file|transcript]
```

- Latest response body by default; `--recursive` walks descendants depth-first;
  `--format path` prints `id name path` lines.
- Provenance explicit per item (`source: file|transcript|scrape`). Raw screen content is
  `herdr pane read`'s job, not `out`'s.

### attach — go look at it (core)

`orcr attach <target> [--takeover]` — inside herdr: focuses the tab; plain terminal: hands
the terminal to the pane (detach returns). Observe by default; `--takeover` claims input
and is **required** for foreign targets.

### kill — stop execution (core)

```
orcr kill <target...> [--tree] [--force] [--json]
```

- Managed: graceful per-harness shutdown recipe (~5s) → close orcr-owned pane+tab; status
  `killed`; run dir + history remain.
- `--tree`: bottom-up, **managed descendants only** by default; foreign nodes skipped and
  listed; `--force` to cross. Foreign kill always requires `--force`; result lists
  closed vs skipped.
- `kill l2` stops a loop; `loop rm l2` deletes its definition.

### loop — the one durable job primitive (core)

```
orcr loop add ("<cron>" | --every <dur> | --once-at <time>)
              -a <harness> (-p <text> | -f <file>)
              [--max <n>] [--until <regex>] [--key <k>]
              [--catchup skip|once] [--expires <dur>] [run-flags…]
orcr loop ls | show <id> | pause <id> | resume <id> [--once-at <time>] | rm <id>
```

- Cadences: five-field cron (stored UTC; confirmations echo local + UTC), `--every`
  (measured from tick completion), `--once-at` one-shot (the *once* is in the flag name on
  purpose; confirmation says "fires once at …, then ends"; re-arm via `resume --once-at`).
- `--max` / `--until <regex>` (matched against each tick's response file) are termination
  policy. `--every 20m --max 20 --until "ALL PASS"` = durable fix-until-green.
- Each tick spawns a fresh agent; ticks appear under `l<N>` in `ls --tree`. No built-in
  judge — a tick can spawn its own (tick agents have the CLI), or use the worker/judge
  recipe (§7 pattern 3).
- `-f` prompt files re-read every tick. `--key` = idempotent creation (returns the
  existing `l<N>` on retry). `--catchup skip|once` for missed ticks. Recurring loops run
  until removed; `--expires` opts into expiry. Creation prints cadence in words + exact
  cancel command.

### adopt — promote foreign → managed (advanced)

`orcr adopt <herdr-target> [--name <label>]` — assigns `a<N>` immediately; full contract
starts with the next orcr-initiated turn after idle (`adopted`, `pending: true` until
then). Nothing back-filled; never splices into a turn orcr didn't start.

### status — one health probe (plumbing)

`orcr status [--json]` — version/schema_version/features, store + lock, daemon (running /
pid / pending loops + next fire), herdr binary/version, target session + server
reachability, live counts by status (blocked named), reconciliation drift. There is no
separate `daemon status`.

### daemon — supervisor control (advanced)

`orcr daemon start|stop` — auto-starts on demand (first `loop add`, detached-run timeout,
kept-agent reap). Owns: loop execution, idle reaping, detached timeouts, reconciliation;
single writer for durable job state (foreground verbs RPC through it when up, store lock
when not). Named `daemon` deliberately: `herdr server` is the substrate, `orcr daemon` the
supervisor.

### top — tree TUI (advanced)

`orcr top [--pane]` — for SSH/plain terminals; `--pane` opens a split inside herdr. v1's
auto-viewer is gone — agents live in the visible session; herdr is the viewer.

### gc — reconcile (plumbing)

`orcr gc [--dry-run]` — diffs store vs live session: closes orcr-marked panes unknown to
the store, marks vanished agents `lost`, drops dead session registrations. Structurally
cannot touch unmarked (user) resources.

### events — the feed (plumbing)

`orcr events [--follow] [--json]` — spawns, status changes, turns, steers, loop ticks,
reconciliation. `--follow --json` = NDJSON stream.

## 6 · What was cut — and its replacement

| v1 | v2 replacement | why |
| --- | --- | --- |
| `loop` + `schedule` (two verbs) | one `loop` primitive (`--every`/cron/`--once-at` + `--max`/`--until`) | both were "run an agent on a cadence with a stop rule"; one-shots explicit in the flag |
| `goal` | recipe + SDK `iterateUntil()` | worker/judge iteration is orchestration policy |
| `workflow run` | just run the script; env contract auto-parents; SDK `withGroup()` for grouping/orphan cleanup | "execute a script" needs no verb |
| `job ls/show/pause/resume/rm` | `loop …` subcommands | one durable job type left |
| `ps` / `tree` / `history` | `ls` / `ls --tree` / `ls --all` | one list, three views |
| `serve` + split status | `daemon start|stop` + single `status` | one health command |
| auto-viewer pane | dropped; herdr UI is the viewer; `top` explicit | agents are visible now |
| hidden `orcr` session | user's default session + ownership markers; isolation = one config line | visibility is the product |

Goal recipe (worker + independent judge), shell form:

```sh
orcr run -a claude -p "$(cat goal.md)" --name worker --keep -d
for i in 1 2 3 4 5; do
  orcr wait worker --timeout 15m
  verdict=$(orcr run -a codex -p "Judge against goal.md. First line: PASS or FAIL: reasons.
$(orcr out worker)")
  case "$verdict" in PASS*) break;; esac
  orcr send worker --turn "Judge feedback — address and retry: ${verdict#FAIL:}"
done
orcr kill worker
```

Deliberately not added: orca-style coordination primitives (inbox/ask/gates/task boards) —
compose on top via files + SDK against stable ids, lineage, run dirs, events. Remote hosts
and a permission broker are future work; JSON shapes reserve room (`host`, capabilities,
blocked semantics).

## 7 · SDK: the six workflow patterns

SDK (TS + Python) = thin typed wrapper over `orcr … --json`. Surface: `orcr.run(opts) →
Agent` (waits by default; `detach: true` → live handle), `agent.out/.turn/.steer/.wait/
.kill/.show`, `orcr.wait(agents, {any,tree,timeout})`, `orcr.loop.add(opts)` (durable —
process can exit), `orcr.withGroup(label, fn)`.

All six canonical patterns are expressible today; full TypeScript for each lives in the
rendered reference (`orcr-cli-reference.html`, §7):

1. **Classify-and-act** — cheap classifier run → route table → dispatch to the right
   harness.
2. **Fanout-and-synthesize** — N detached runs → `orcr.wait` → collect `out()` → one
   synthesizer run.
3. **Adversarial verification** — kept worker + K refuter runs on a different harness;
   majority-refuted feedback goes back via `worker.turn(…)`; iterate.
4. **Generate-and-filter** — mixed-harness detached generators → single filter run with
   rubric + dedupe.
5. **Tournament** — seeded attempts → pairwise judge bracket (`while pool.length > 1`) →
   winner.
6. **Loop-until-done** — foreground: while-loop until two consecutive dry rounds.
   **6b — set up and leave:** do steps + consolidation in-process, then
   `orcr.loop.add({every, promptFile, until, max, key})` and `process.exit(0)` — the
   daemon owns it from there (`orcr loop ls` to check in).

Patterns compose; any durable-loop tick can itself run a pattern (tick agents have the
CLI). v1 `goal` ≡ pattern 3 with one verifier.

## 8 · Configuration

```toml
# ~/.orcr/config.toml — every key optional; defaults shown
[defaults]
harness = "claude"
model   = ""          # empty = harness default
effort  = ""
timeout = "10m"
keep    = false

[limits]
max_depth           = 3
max_agents_per_tree = 10
max_concurrent      = 4
idle_reap           = "15m"

[herdr]
bin     = ""          # empty = $ORCR_HERDR_BIN → $PATH
session = ""          # "" = herdr's default session · "orcr" for full isolation

[placement]
workspace = "auto"    # auto | current | git-root | cwd | new | <label>
container = "tab"     # tab | split
```

`ORCR_STORE` relocates the store; `ORCR_SESSION` overrides the session per-invocation.

## 9 · Design-review adjudication

Four independent reviews (LLM-caller ergonomics · human CLI conventions · systems/herdr
integration · minimalism/extensibility). Adopted: contract levels + capability maps
everywhere; git-root-first canonical workspace matching with reported reason; single
`status`; ownership markers scoping gc/reap/kill; `send --raw` and `attach --takeover`
gates; hardened adopt semantics; auto-close only after response finalization;
`lost_pane`/`lost_session` fail-fast; `herdr_server` reporting; `loop --key`; `--parent`
validity rules; required duration units; `run --dry-run`; feature probe; reserved `host`.

Adjudicated against (reasons recorded): isolated-session default for plain terminals
(visibility is the moat; markers + force-gates answer the blast radius; first decision to
revisit if markers prove insufficient) · managed-only `ls --json` (seeing everything is
the point; safety via fields + gates, not hiding) · renaming `out` → `logs` (`out` is the
final-answer contract, not a stream) · an automation-mode env var (two default-sets make a
CLI unpredictable; the skill teaches explicit flags instead).

Post-review naming call (owner): the durable primitive was renamed `schedule` → `loop` —
the headline use is loop engineering and the name should say so; the one-shot case is
defused by `--once-at` + explicit confirmation output.
