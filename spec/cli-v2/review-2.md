# CLI v2 Review: Human CLI UX and Conventions

## R1. `run` waits by default vs async default

**Answer:** Waiting by default is the right call for both humans and LLM callers, despite the migration cost.

For humans, `orcr run -a codex -p "review this"` should behave like a command that returns an answer unless explicitly backgrounded. That matches `paseo run`, many AI CLIs, and ordinary shell expectations: foreground commands produce a result; background commands use a detach flag. For LLM callers, wait-by-default is even better because it removes the brittle two-step `run -> parse id -> wait -> out` dance for the common one-shot delegation case.

The cost of the flip is real for v1 scripts: existing fan-out code may accidentally serialize or block. The design needs a loud migration note and examples that make `--detach` the canonical fan-out form. I would also consider a config compatibility switch only during migration, but not long-term.

**Recommendation:** Keep wait-by-default. Make `--detach/-d` prominent in `orcr run --help`, docs, and migration warnings. Add a structured warning when an old v1 config is detected, because silent blocking will feel like a hang.

## R2. One tab per agent vs splits

**Answer:** One tab per agent is the right default, but the design needs an explicit fan-out layout behavior.

Tabs are a better default than splits because they preserve readable full-screen TUIs and align with the "one agent is one place I can attach to" mental model. Splits are useful for a small number of closely related agents, but they degrade fast with real TUI apps.

The weak spot is wide fan-out. Ten agents as ten sibling tabs is coherent, but only if herdr makes those tabs discoverable and the labels are short, stable, and searchable. A first-time user will otherwise assume "orcr sprayed my workspace."

**Recommendation:** Keep tab default. Add a fan-out convention: when more than one child is spawned from the same parent or script, group tab labels with a shared prefix and expose `orcr ls --tree` plus `orcr attach <id>` as the navigation path. Consider `--split` for explicit local comparison only, not as a default.

## R3. Default session: user's default herdr session vs dedicated `orcr`

**Answer:** Use the user's default herdr session. That is the product moat and the right human UX.

A hidden orcr session makes the system feel like yet another daemon manager. The draft's strongest premise is that orcr agents are real visible herdr agents, so defaulting to a dedicated side session would undercut the main advantage.

The risk is clutter and accidental control of user-started panes. Sharing the session means `ls`, `send`, `wait`, and especially `kill` now operate in the same namespace as the user's hand-started agents. That is acceptable only if origin, workspace, and target resolution are extremely clear.

**Recommendation:** Keep the default shared session. Make every destructive or ambiguous operation show origin and placement in human output. Keep `kill` of foreign targets guarded, and add an easy config example for teams that want `[herdr] session = "orcr"`.

## R4. Is `adopt` pulling its weight?

**Answer:** Yes, but adoption should be framed as "promote to managed", not required for basic control.

Foreign targets should work in degraded mode for `send`, `wait`, `out`, `attach`, and guarded `kill`; the draft already gets that right. `adopt` is valuable because it creates a clear boundary between "I can poke this pane" and "orcr now owns tracking, turn files, lineage, and stronger contracts."

The word `adopt` is understandable, but it may imply takeover of old history or process ownership. The draft says history cannot be reconstructed, which is correct, but that caveat must be visible in help.

**Recommendation:** Keep `adopt`. In `orcr adopt --help`, say "starts tracking from the next turn; does not import prior transcript history." Consider aliasing or describing it as "promote a herdr agent to an orcr-managed agent."

## R5. Cutting goal/workflow/loop

**Answer:** Cutting `goal` and `workflow` is right. Cutting standalone `loop` is right if `schedule add --every` has excellent help and examples.

`goal` and `workflow` are orchestration recipes, not CLI primitives. Keeping them would make v2 feel like v1 with renamed furniture. `schedule` is different because daemon durability is not easy to recreate from a shell script that exits.

The missing question is not "does goal deserve a verb"; it is whether the CLI exposes enough primitives to build agent-to-agent coordination safely. Compared with orca, orcr is intentionally avoiding inbox/task/gate primitives. That is fine for a small CLI, but it means examples and SDK helpers matter.

**Recommendation:** Keep the cut. Provide first-class recipes for foreground loop, durable schedule loop, worker/judge, and fan-out/fan-in. Do not reintroduce `workflow`; plain scripts are the better CLI convention.

## R6. Workspace-by-cwd matching

**Answer:** Prefer git-root matching first, then exact cwd, then no ancestor fallback unless explicitly documented and shown.

Exact cwd matching is predictable but too narrow in repos where people run commands from subdirectories. Ancestor matching is convenient but can surprise users in monorepos or nested repos: running from `repo/packages/api` might attach work to a broad parent workspace when the user expects a package-specific workspace.

Git-root matching best matches how developers think about projects and aligns with paseo's "agents working here" cwd-scoped ergonomics without making every subdirectory a separate workspace.

**Recommendation:** Use this resolution order: explicit `--workspace`; current herdr workspace when inside herdr; existing workspace with cwd equal to git root; existing workspace with cwd equal to process cwd; create at git root if inside a repo, else create at cwd. Make ancestor matching opt-in or only use nearest ancestor with a visible confirmation in spawn output.

## R7. `ls` merging managed and foreign by default

**Answer:** Merge by default, but make origin visually impossible to miss.

The whole value proposition is that orcr sees the real herdr session, including agents the user started manually. If foreign agents are hidden behind `--foreign`, first-time users will think orcr cannot see them.

The danger is accidental overreach. A unified list can make foreign agents look equally managed even when they lack run directories, turn files, and full contract semantics.

**Recommendation:** Keep merged default. Add an `ORIGIN` column with values like `orcr` and `herdr`, and avoid displaying foreign targets in the same id shape as `a<N>`. Keep `--managed` and `--foreign`; add `orcr ls -q` later only if scripts need ids-only output.

## R8. Naming: `ls` vs `list`; `out` vs `logs`; `adopt`; `daemon` vs `server`

**Answer:** The naming is mostly good for a compact AI-agent CLI, but it should reduce friction with herdr where it directly wraps herdr concepts.

`ls` is right. It matches paseo, docker muscle memory, and short frequent use. Herdr uses `list`, but orcr is a higher-level user CLI and benefits from terseness. A `list` alias would be cheap if the parser allows it.

`out` is the weakest name. Users know `logs` from docker, kubectl, paseo, and almost every daemon/process tool. `out` is short, but it is not obvious whether it means stdout, final answer, transcript, or run directory. It also conflicts with the draft's own `ORCR_OUT` file contract language.

`adopt` is acceptable and memorable, though help must clarify that history starts from the next turn.

`daemon` is better than `server` for orcr. Herdr owns `server`; orcr's background component exists for schedules/events/gc, so `daemon` matches paseo and common Unix vocabulary. Since users will run both herdr and orcr, having `herdr server` and `orcr daemon` is a useful distinction.

**Recommendation:** Keep `ls`, `adopt`, and `daemon`. Rename `out` to `logs` or make `logs` the primary documented command with `out` as an alias. Add aliases `list -> ls` and maybe `server -> daemon` only if they do not expand the visible help surface too much.

## R9. Anything in Section 4 still cuttable? Anything cut that will bite?

**Answer:** `top` and maybe `status` are the most cuttable; permission/blocked surfacing is the thing most likely to bite.

`top` overlaps with herdr's UI and was explicitly demoted when the auto-viewer was dropped. If it remains, it should justify itself as a terminal dashboard for non-herdr contexts, not as a primary primitive.

`status` and `daemon status` may confuse users. If `status` means orcr health and `daemon status` means background process health, the distinction needs to be crisp. Git has `status` because the repo state is the core object; orcr's core objects are agents and schedules.

What may bite from the cut list is not `goal` or `workflow`; it is the absence of a clear permission or blocked-attention surface. Paseo has `permit`; herdr has agent states including `blocked`. Humans supervising AI agents need to know "who needs me" quickly.

**Recommendation:** Consider cutting `top` from v2 initial release or marking it experimental. Keep `status` only if it reports actionable session-wide health and blocked agents, not just daemon health. Add a first-class way to filter attention states, such as `orcr ls --blocked` or `orcr status` showing blocked/needs-input agents.

## Additional Findings

### High: The verb surface mixes paseo-style verbs with herdr-style substrate concepts

The draft says to judge especially against herdr's noun-first conventions, but v2 is almost entirely verb-first: `run`, `ls`, `show`, `send`, `wait`, `out`, `attach`, `kill`, `adopt`, `top`, `status`, `daemon`, `gc`. That is coherent for orcr if the top-level object is always "agent", but `schedule` and `daemon` are noun-first islands.

First-time users coming from herdr may try `orcr agent list`, `orcr agent attach`, or `orcr schedule add`; users coming from paseo will try `orcr logs`. The current design favors paseo muscle memory but does not say that explicitly.

**Recommendation:** Embrace "agent is the implicit noun" in help text: `orcr <verb> ...` operates on agents unless the command is a named subsystem like `schedule` or `daemon`. Provide hidden or low-noise aliases for `list` and `logs`, and include "See also: herdr agent list" style hints where relevant.

### High: `send` intent flags are too easy to misuse

`send <id> [--steer | --turn] [--wait]` carries a subtle state machine: steer while working, new turn when idle and kept, conflict exit 7 otherwise. Humans will get this wrong, especially across mixed managed and foreign targets. The word `send` does not reveal whether it presses Enter, appends to a TUI, starts a new request, or interrupts work.

This is a real UX risk because `send` changes agent behavior and can corrupt an in-progress task if interpreted incorrectly.

**Recommendation:** Make the default behavior conservative and explicit in help. If target is working, require `--steer` unless the message is clearly a pane-level send to a foreign target. If target is idle and managed, require or strongly prefer `--turn` for a new task. At minimum, print a human-facing confirmation like `sent as steer to working agent a7` or `started turn 3 on a7`.

### High: `kill` is too blunt for a terminal workspace world

In docker and kubectl, `kill` is expected to terminate. In herdr, there are at least three user-visible things: the process, the pane, and possibly the tab. The draft says "kill graceful -> pane close", which makes `kill` sound like it may also destroy UI placement.

For foreign agents, requiring `--force` is good, but the semantics of force are unclear. Does it send Ctrl-C, close the pane, kill the process group, or remove tracking?

**Recommendation:** Split or document lifecycle semantics precisely. Consider `stop` as the user-facing graceful command and `kill --force` as the hard termination path. If `kill` remains, help must say whether the pane/tab is closed and how to preserve it.

### Medium: `run --keep` is doing too much hidden lifecycle work

`run` auto-closes after first completed turn unless `--keep`; kept agents are later reaped by idle timeout. That is efficient, but it violates a likely human expectation that a visible TUI session remains visible after work completes.

This is especially risky because the whole product promise is attachable, visible real TUI sessions. Auto-closing makes sense for one-shot foreground calls, but it should not surprise users watching herdr.

**Recommendation:** In TTY human mode, make the completion message explicit: `closed pane after turn; use --keep to leave it attachable`. Consider config default `keep=false` for scripts but an interactive prompt-free hint for humans after first use.

### Medium: `--mode tui|exec` needs a clearer mental model

The design says orcr agents are real interactive TUI sessions, but `--mode exec` appears in `run` without explanation. Users will ask whether exec agents appear in herdr, whether attach works, whether `send` works, and whether output contracts differ.

This flag can undermine the product model unless it is clearly scoped.

**Recommendation:** Either cut `--mode exec` from the simplified CLI or define it sharply: placement, attachability, send behavior, transcript source, and compatibility by harness. If it is mainly plumbing, hide it from first-level help.

### Medium: `schedule add` is correct but too verbose compared with `run`

`orcr schedule add ("<cron>" | --every <dur> | --at <time>) -a <harness> (-p|-f)` is understandable, but users will look for symmetry with `run`. Paseo uses `schedule create`; herdr uses noun-first subcommands. The draft uses `add`, `ls`, `show`, `pause`, `resume`, `rm`, which is compact but not fully conventional.

The biggest discoverability issue is where run flags go and whether schedule creates agents immediately or only at the first tick.

**Recommendation:** Keep `schedule add`, but make `orcr schedule --help` example-driven. Show `schedule add --every 15m -- orcr run ...` if schedules are command-like, or show the exact run-flag passthrough boundary if not. Consider `create` as an alias for users coming from paseo and kubectl-style nouns.

### Medium: `status` is underspecified and may become a junk drawer

`orcr status [--json]` could mean current session, daemon, scheduler, storage, active agents, blocked agents, or health checks. Since there is also `daemon status`, the command invites overlap.

Humans use `status` for "what should I know right now?" If it prints implementation health, it will disappoint. If it prints blocked agents and schedule health, it may be valuable.

**Recommendation:** Define `status` as an operator summary: target session, daemon health, active/blocked/done counts, next schedules, storage path, and warnings. Keep machine detail in `daemon status` and `show`.

### Medium: Duration unit conventions conflict with herdr

The draft says durations are human-form and never ms, while herdr uses timeout ms. That is a good user-facing choice, but it creates a wrapper mismatch. Users familiar with herdr may pass `--timeout 10000` and expect 10 seconds; orcr may parse it differently or reject it.

**Recommendation:** Require explicit units for all human duration flags, including `--timeout 10s`, and reject bare numbers with a helpful error. Do not silently interpret bare numbers.

### Medium: Target resolution order risks dangerous ambiguity

The draft resolves `orcr id -> orcr name -> herdr agent target`. That is convenient, but names and herdr labels are human-created and collision-prone. In a shared session, ambiguity is normal, not exceptional.

The high-risk cases are `send`, `attach`, and `kill`, where choosing the wrong target has immediate effect.

**Recommendation:** Keep collision errors, but make disambiguation excellent. Print the exact alternatives with origin, workspace, tab, pane, and suggested stable target. Consider requiring exact `a<N>` ids for destructive operations unless `--force-target` or a unique name is present.

### Low: `show` is well named but may need `inspect` as an alias for docker users

`show <id>` is understandable and matches many CLIs, but docker users often reach for `inspect`, and kubectl users reach for `describe`. This is not worth expanding the visible command surface, but aliases could reduce friction.

**Recommendation:** Keep `show` as primary. If aliases are cheap, add hidden `inspect` and maybe `describe`; do not document all three equally.

### Low: `gc` is conventional for developers but not self-explanatory

`gc` is familiar from git, but first-time users may not know whether it deletes run directories, old panes, schedules, logs, or stale adopted agents.

**Recommendation:** Keep `gc`, but make `--dry-run` the first example and show categories in output. Never let human `gc` delete panes; reserve it for orcr-owned storage unless a separate explicit flag is provided.

### Low: `--all` on `ls` inherits two different meanings

In many tools, `--all` means hidden/all namespaces. Here it means ended agents too. Paseo's `ls -a all` has similar behavior, but orcr also has session/workspace and foreign/managed scopes, so "all" can be misread as all sessions or all workspaces.

**Recommendation:** Keep `--all` for brevity, but document it as "include ended agents in the target session." If cross-session listing exists later, use a distinct flag like `--all-sessions`.

### Low: `--workspace new` is handy but slightly magical

Using the literal string `new` in a field that otherwise accepts id or label creates a reserved-word edge case. A user may have a workspace labeled "new".

**Recommendation:** Prefer `--new-workspace` or reserve `new` with a clear escaping/disambiguation rule. If keeping `--workspace new`, reject labels that collide with reserved selector words.

## Verdict

The v2 direction is strong: make agents visible in the user's real herdr session, keep agent verbs compact, and push heavyweight orchestration into recipes and SDKs. I would approve the simplification with changes.

The required fixes before locking the CLI are: make `logs` the primary name or alias for `out`; define `send` and `kill` semantics more safely; resolve workspace matching around git roots instead of broad ancestor matching; make merged foreign/managed listings visually explicit; and clarify `status`, `top`, and `--mode exec` so they do not become vague escape hatches.

The best overall convention is: orcr should be paseo-like at the top level because agent is the implicit noun, but herdr-like whenever it exposes herdr placement, sessions, workspaces, tabs, panes, and daemon/server boundaries.
