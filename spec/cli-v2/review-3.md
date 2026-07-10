# orcr CLI v2 Review: Systems Correctness and herdr Integration

## R1. `run` Waits By Default

**Answer: mostly right, but only if `run` has a strict first-turn contract and a predictable escape hatch.**

Waiting by default is the right default for human and LLM callers because it makes `orcr run -p ...` composable as a better cross-harness command substitution: start a real TUI agent, wait for one answer, print the body, exit nonzero on timeout/block/kill. LLM callers benefit most because they can call one primitive and get a result without remembering `run && wait && out`.

The cost of the flip from v1 is real: any existing fan-out script that expects `run` to return an id immediately will accidentally serialize. Recommendation: require a migration warning in docs and examples, keep `--detach/-d` short and prominent, and add `--json` output that always includes both `id` and `placement` even in wait mode. Also consider `orcr run --detach --wait` invalid or explicitly defined, so scripts do not encode ambiguous intent.

Correctness recommendation: default wait must mean "wait for the first orcr-managed turn response file to be finalized", not merely "herdr says idle". herdr idle is a signal; the run-dir response file is the contract.

## R2. One Tab Per Agent

**Answer: yes for the default, but fan-out needs grouping controls before this ships.**

One tab per agent matches herdr's hierarchy better than splits: it gives each TUI stable screen real estate, a label, independent focus, and safer close semantics. Splits are useful for a human actively supervising two panes, but they become unreadable and operationally risky at 5-10 agents.

The tab-sprawl objection is acceptable if orcr gives users a way to collapse or group fan-outs. Recommendation: keep one tab per agent as default, make `--split` opt-in, and add a fan-out placement rule: spawned children should get labels with parent prefix and ordinal, and `ls --tree`/`top` should be the intended way to inspect broad runs. Also define whether `kill --tree` closes tabs or only terminates processes; with one tab per agent, tab close is destructive UI cleanup and must be explicit or tightly scoped to orcr-owned tabs.

## R3. Default Session Is The User's Default herdr Session

**Answer: this is the highest-risk design choice. I would not make the user's default session the unconditional default for plain-terminal invocations.**

Sharing the user's normal herdr session is compelling for visibility, but it weakens several safety boundaries:

- Focus safety: even with "never steal focus", tab creation, sidebar activity, and attach/focus commands can race with the user's current herdr interaction.
- GC safety: `orcr gc` must never reap user-created panes, tabs, workspaces, or hand-run agents that merely resemble orcr resources.
- Kill safety: foreign targets requiring `--force` is good, but name collisions and stale adoption records can still turn `kill a7` into "close a pane in the user's visible workspace" after herdr state changed.
- Name collisions: user tabs/panes/agents may use labels that collide with orcr ids, orcr names, harness names, or herdr targets.
- Operational blast radius: if orcr creates many agents, hits a bug, or runs a schedule, it pollutes the user's everyday herdr session.

Recommendation: use the current herdr session when invoked from inside herdr, but for plain terminal invocations default to an `orcr` session unless config explicitly opts into `herdr.session = ""` / default. That preserves the moat for in-herdr usage while keeping unattended automation isolated. If the design insists on default-session sharing, orcr must tag every managed workspace/tab/pane with durable metadata and enforce "only mutate tagged resources unless `--foreign --force` is present".

## R4. `adopt`

**Answer: adopt is pulling its weight, but the draft underspecifies the dangerous middle.**

Foreign targets should work degraded for read/send/wait/attach, but adoption is necessary for the run-dir contract, stable ids, lineage, and later turn tracking. Without adoption, orcr would pretend it can provide guarantees for history it never observed.

Recommendation: keep `adopt`, but make it explicit that adoption cannot start while a foreign agent is `working` unless the user passes something like `--from-next-idle` or `--force-current-turn-degraded`. Mid-turn adoption is otherwise a race: orcr cannot know the prompt boundary, whether a send is steering or starting a turn, or when the response file should be sealed. Adoption should create a managed id immediately, mark historical output as `untracked`, and begin full contract only after an observed idle baseline followed by an orcr-initiated `send --turn`.

## R5. Cutting Goal/Workflow/Loop

**Answer: mostly right, but daemon supervision is needed for more than schedules if the CLI promises durable background waits and cleanup.**

Cutting `goal`, `workflow`, and standalone `loop` is right for the CLI surface. They are recipes unless orcr owns a shared task board, inbox, or coordinator model like orca. Keeping them as verbs would recreate v1's complexity.

However, the daemon may still need to own:

- schedule execution;
- idle reaping;
- durable timeout enforcement for detached runs;
- single-writer serialization of state mutations;
- recovery/reconciliation after herdr server restart or death.

Recommendation: do not describe schedule as the only daemon-backed concern. Define the daemon as the optional single writer for durable state and background policy, while stateless foreground verbs may still execute directly through a lock. If there is no daemon, detached run timeouts and idle reaping must be best-effort or disabled with a clear status warning.

## R6. Workspace-By-Cwd Matching

**Answer: exact cwd plus canonical git-root fallback is safer than nearest ancestor alone.**

Nearest ancestor matching creates surprises in monorepos, nested repos, worktrees, symlinked paths, and no-git scratch dirs. Example failures:

- `/repo/packages/a` and `/repo/packages/b` both collapse into `/repo` even if separate workspaces exist.
- A symlinked checkout and its realpath create duplicate or mismatched workspaces.
- Git worktrees share a common repository identity but have distinct working tree roots; matching by main repo root can place agents in the wrong workspace.
- No-git directories under `/tmp` may match a broad ancestor workspace accidentally.

Recommendation: canonicalize cwd using physical path resolution, then match in this order: explicit `--workspace`; current workspace if inside herdr and no override; exact canonical workspace cwd; exact git worktree root; nearest ancestor only if it is within the same git worktree and the distance is small/defined; otherwise create a new workspace. The spawn result should report the matched rule, not just the final placement.

## R7. `ls` Merging Managed And Foreign

**Answer: right for `orcr ls`, risky for machine consumers unless origin and mutability are obvious.**

Merged `ls` supports the core promise that orcr sees everything herdr sees. But scripts that previously assume every row is orcr-managed could try to `out`, `wait`, or `kill` rows with weaker guarantees.

Recommendation: keep merged default for human output, but make the table visually explicit: `origin`, `managed`, or `contract` should not be optional. For JSON, require `origin`, `mutable_by_orcr`, `run_dir`, `contract_level`, and `destructive_requires_force`. Consider making `orcr ls --json` include both arrays, `managed` and `foreign`, or at least stable discriminated rows.

## R8. Naming

**Answer: names are acceptable but should align with herdr where the operation is fundamentally herdr-shaped.**

`ls` is fine and common for agent CLIs. `out` is terse but less discoverable than `logs`; since paseo uses `logs` and users will expect logs, recommendation: keep `out` as the precise contract command and add `logs` as an alias if command aliases are acceptable. `adopt` is a good name. `daemon` is okay for orcr's own process, but herdr uses `server`, and the draft also auto-starts `herdr server`; that distinction must be explicit.

Recommendation: use `orcr daemon` only for the orcr state/schedule/reconciliation process, and always say `herdr server` when referring to the substrate. Avoid overloading "server" for orcr unless you intentionally mirror herdr noun-first style.

## R9. Cuttable Or Missing Surface

**Answer: `top` is the most cuttable; permission/interrupt handling is the missing bite risk.**

`top` is less essential if herdr is the real viewer and `ls/show/events` exist. Keep it only if it exposes orcr-specific contract state that herdr cannot show: turn files, lineage, schedules, and blocked waits.

The missing surface that may bite is not goal/workflow; it is permission and intervention state. paseo has `permit`, herdr has agent detection states including `blocked`, and real TUIs often pause for approvals. If orcr relies on `working -> idle`, blocked/permission states need first-class wait and exit behavior.

Recommendation: add explicit blocked/approval semantics to `wait`, `run`, `ls`, and JSON status before adding any higher-level orchestration verbs. A minimal form is enough: `status=blocked`, exit 4, `block_reason` if herdr can expose it, and `send --steer` allowed while blocked.

## Additional Findings

### High: User Default Session Sharing Lacks An Ownership Boundary

The design says orcr agents live in the user's normal herdr session and `orcr ls` sees all agents, but it does not define a durable ownership marker for resources that orcr is allowed to mutate. A label is not enough; labels collide and users rename things.

Recommendation: every orcr-created herdr workspace/tab/pane/agent should carry machine-readable metadata if herdr supports it, or a reserved env/report-agent identity if it does not. `gc`, `kill`, auto-close, idle reap, and tab close must operate only on resources with this marker. Foreign resources should require `--force` for destructive actions and should never be touched by `gc`.

### High: Daemon And Stateless Verbs Need Single-Writer Discipline

The draft has auto-starting daemon-backed schedules, foreground `run`, `send`, `wait`, `gc`, and adoption all mutating the same ids, run dirs, and herdr panes. Without one writer or a lock protocol, races are likely: two `send --turn` commands can assign the same turn, `gc` can reap while `wait` is finalizing output, and schedule ticks can spawn beyond concurrency limits.

Recommendation: define a state-store lock and mutation model. Either all mutating verbs RPC through the daemon when it is running, or every mutating process acquires the same file lock and writes idempotent transactions. This should be in the design, not left to implementation.

### High: herdr Server Death Mid-Run Is Not Specified

orcr depends on herdr as the process owner, pane registry, transcript source, and state detector. If the herdr session dies while an orcr run is waiting, the current design does not say whether the agent process dies, whether orcr can recover the run dir, or what exit code is returned.

Recommendation: specify reconciliation states: `lost_session`, `lost_pane`, `lost_agent`, and `orphaned_run`. Foreground `run/wait` should return a distinct error details code under exit 1 or 5, not hang until timeout. The daemon should reconcile managed ids against `herdr agent list` and mark missing panes terminal rather than inventing success from stale response files.

### High: Foreign `send`/`wait` Reliability Is Overstated

The draft says `send <herdr-target>` is pane send-text plus enter and `wait` is herdr working->idle. That is not enough for foreign TUIs. Some targets may be `unknown`, already `idle`, blocked, not recognized as an agent, or in a shell prompt rather than a TUI input. Sending text plus Enter can execute a shell command in a user's pane.

Recommendation: require a pre-send capability check for foreign targets: target must resolve to a herdr agent with a recognized harness or an explicit `--raw-pane`/`--force` mode. `wait` on foreign targets should handle `unknown` separately and should support a detection baseline: if already idle before send, wait for working then idle, or wait for output change, not merely idle.

### High: Adopt Mid-Turn Can Corrupt Turn Semantics

Adopting a currently working foreign agent and then enabling `send --turn`, `out`, and `wait` risks mixing a pre-adoption human prompt with an orcr-managed response. It also creates ambiguous response files.

Recommendation: adopt should default to "observe until next idle, then managed from next orcr send". If the user adopts while working, the managed id can exist immediately, but `contract_level` should remain `degraded_until_next_turn`.

### Medium: Workspace Matching Needs Canonical Identity, Not String Equality

The draft says cwd equals or nearest ancestor, but path strings are not stable enough. Symlinks, case-insensitive filesystems, bind mounts, and worktree paths can make the same directory appear different or different directories appear related.

Recommendation: store canonical physical path plus git worktree root plus repository id where available. Matching should report `match_reason` and avoid ancestor matching across git boundaries.

### Medium: Headless herdr Auto-Start Needs Failure Semantics

From a plain terminal, orcr may auto-start `herdr server`. The design does not say what happens if the server is already starting, the socket is stale, another user process owns the session, startup succeeds but workspace creation fails, or the user has no herdr binary configured.

Recommendation: make startup a distinct phase in `status` and spawn JSON: `herdr_server: existing|started|failed`. Use a startup lock per session, verify the server with a read-after-start command, and surface actionable errors under exit 2 for environment/setup failures.

### Medium: `attach` From Plain Terminal Is Underspecified

The draft says `attach` focuses the pane if inside herdr, else hands the terminal over. That is a large behavioral split. It can also attach to a user pane that orcr did not create.

Recommendation: specify whether attach is read-only, takeover, or full terminal control, and require confirmation/`--takeover` for foreign targets. Align directly with herdr `agent attach [--takeover]` semantics.

### Medium: Auto-Close After First Turn Can Race With Output Collection

`run` auto-closes after first completed turn unless `--keep`, while output guarantee uses response file, transcript, then scrape. Closing the pane too soon can remove the best fallback source or hide a failure prompt that appeared after idle.

Recommendation: only auto-close after the response file is finalized or after transcript scrape succeeds. Keep a short post-idle settle window and record `close_reason`. If fallback was pane scrape, close after scrape, not before.

### Medium: Parent/Child Placement And `--parent` Need Authority Rules

The matrix gives orcr-spawned agents parent placement, and `run` exposes `--parent <id>`. It does not define whether any caller can attach a new child to any parent, including a foreign or ended agent.

Recommendation: `--parent` should require the parent to be managed or adopted, live or recently ended with known placement, and in the same session unless `--session` explicitly overrides and is validated. Reject parent-child cycles and depth-limit violations before spawning.

### Medium: `kill --tree` Bottom-Up Is Right But Needs Per-Origin Policy

The draft says foreign kill requires `--force`, but `kill --tree` can include mixed managed and foreign descendants after adoption or manual parent assignment.

Recommendation: define tree kill as managed-only by default. If the tree contains foreign targets, return exit 7 with a list of skipped targets unless `--force-foreign` is supplied. Do not let a generic `--force` erase the origin distinction.

### Low: `--worktree` Placement Is Ambiguous

`--worktree` provisions via herdr in the resolved workspace and runs there, but the earlier model says `--cwd` and workspace placement are independent. A worktree changes process cwd and may deserve a separate workspace.

Recommendation: define precedence: `--worktree` should set process cwd to the created worktree path unless `--cwd` is rejected as conflicting. Consider defaulting placement to the worktree's workspace, not the caller's current workspace, to avoid a tab labeled as one project but running in another.

### Low: ID And Target Resolution Needs A Stable Ordering Spec

The draft says target resolution is orcr id -> orcr name -> herdr agent target and collisions error. Good, but collision handling will be frequent in a shared session.

Recommendation: expose canonical target forms in `ls/show`, such as `orcr:a7`, `name:foo`, `herdr:pane:<id>`, and allow users/scripts to pass those forms. Do not rely on labels for automation.

### Low: `status` Should Include Cross-System Health

`status` is listed but not described. In this design it is important because orcr spans its own state, daemon, herdr server, and herdr session.

Recommendation: define `orcr status --json` as a health report for orcr store lock, daemon, configured herdr binary, target session, herdr server reachability, and reconciliation drift counts.

## Verdict

The v2 direction is good: reduce orchestration verbs, lean on herdr for visible real TUIs, keep schedule as the durable primitive, and make foreign agents visible. But the draft is too optimistic about sharing the user's default herdr session and about treating herdr detection state as a complete correctness contract.

My recommendation is to proceed only after tightening four areas: resource ownership markers, single-writer state mutation, canonical workspace matching, and explicit failure states for foreign agents and lost herdr sessions. I would also change the plain-terminal default back to an isolated `orcr` herdr session unless the user opts into the default session. Inside an existing herdr pane, using the current session is the right integration point.
