# Review: orcr CLI v2 draft

Lens: minimalism and extensibility. I am judging the draft as a substrate CLI for an SDK, scripts, and agent-authored automation, not as a full orchestration product.

## R1. `run` waits by default vs async default

Waiting by default is right. It matches the most common human and LLM call shape: "run this task and give me the answer." It also makes `orcr run -a codex -p ...` a portable, harness-neutral better prompt command, which is the cleanest product wedge in the whole design.

The cost is real for v1 users and fan-out scripts: every existing `run` call that expected an id now needs `--detach` or `-d`. That is acceptable if v2 is explicitly a breaking CLI redesign, but the migration needs to be sharp: emit a prominent note in v1 compatibility docs, make `--detach` short and memorable, and ensure `--json --detach` returns only the id/placement envelope with no response fields.

Recommendation: keep wait-by-default. Do not add config to flip it globally; that would make scripts non-portable. If compatibility pressure is high, add a temporary `ORCR_V1_ASYNC=1` migration escape hatch, not a documented long-term mode.

## R2. One tab per agent vs splits

One tab per agent is the right default for visibility, isolation, and attachability. Splits are good for intentional local comparison, but they are a poor default for machine fan-out because they degrade quickly and create layout coupling between unrelated agents.

The tab-sprawl concern is real for 10+ agents, but the answer should be grouping and filters, not using splits by default. A tab maps cleanly to "one agent TUI"; a split maps to "I am composing a layout."

Recommendation: keep one tab per agent. Add `--split` only as an explicit placement flag. For wide fan-outs, make `ls --tree`, workspace sidebar state, and naming conventions carry the load. Consider a future `--group-label` metadata flag before adding another container mode.

## R3. Default session: user's default herdr session vs dedicated `orcr`

The user's default herdr session is the right default. It is the moat: agents are visible, attachable, and part of the workspace the user already trusts. A hidden `orcr` session recreates v1's isolation but undercuts the main reason to build on herdr.

The risks are clutter, accidental interaction with user-started agents, and permission ambiguity when orcr can steer or kill foreign panes. Those are manageable if destructive operations are conservative and every placement decision is explicit in output.

Recommendation: keep default herdr session. Add a config profile for isolation (`herdr.session = "orcr"`), but do not make it the default. Require `--force` for destructive foreign operations and make `orcr run --json` always report session/workspace/tab/pane.

## R4. Is `adopt` pulling its weight?

Barely. The concept is useful, but a top-level `adopt` verb may be too much surface area for what is fundamentally "assign an orcr id and start recording from now." Foreign targets should already work in degraded mode for `send`, `wait`, `out`, `attach`, and cautious `kill`; that is enough for most users.

The strongest reason to keep adoption is SDK ergonomics. A script that wants stable ids, lineage, run-dir paths, and turn files needs a way to convert a herdr pane into an orcr-managed handle without respawning it. That is an extensibility requirement, not a daily human workflow.

Recommendation: keep the capability, but consider making it a flag on `show` or `send` rather than a prominent verb: `orcr show <target> --adopt --name ...` or `orcr manage <target>`. If it remains top-level, mark it as advanced and define the exact post-adoption contract: no reconstructed history, next turn only, stable id, run-dir from adoption timestamp.

## R5. Cutting goal/workflow/loop; anything else needs daemon supervision?

Cutting `goal` and `workflow` is right. They are orchestration policies, not irreducible terminal primitives. An SDK can rebuild worker/judge loops and workflow grouping from `run`, `send`, `wait`, `out`, `kill --tree`, lineage metadata, and the run-dir contract.

Cutting `loop` as a separate verb is also right if `schedule add --every --max --until` is expressive enough. A loop is either a foreground script or a durable schedule; a third CLI noun is not worth it.

The only daemon-backed primitive that clearly survives is `schedule`: "run this later or repeatedly after the caller exits" cannot be implemented by a dead script. Everything else should be SDK or recipe. The possible exception is permission brokering, but the draft does not yet define a permission model, so adding a CLI primitive now would be premature.

Recommendation: keep only `schedule` as the durable job primitive. Ensure the SDK exposes enough lifecycle hooks to rebuild v1 `goal` and `workflow`: parent/group ids, run-dir paths, event stream, recursive wait/out/kill, and structured exit reasons.

## R6. Workspace-by-cwd matching

Git-root match should be the primary default, with exact cwd as a tiebreaker. Exact cwd alone fragments a repo into many workspaces when agents are launched from subdirectories. Ancestor matching alone can surprise users by placing a task in a broad parent workspace such as `$HOME` or a monorepo root when the current package deserves its own workspace.

Recommendation: resolve in this order: explicit `--workspace`; current herdr workspace when invoked inside herdr; existing workspace whose cwd equals git root for the process cwd; exact cwd match; nearest ancestor match only if the ancestor is a git root or has explicit herdr workspace metadata; otherwise create a workspace labeled after the git root or cwd. Always print the resolved workspace in human and JSON output for `run`.

## R7. `ls` merging managed + foreign by default

Merging managed and foreign by default is right for the herdr moat, but only for live local visibility. The default table should answer "what agents are active in this herdr session?" If foreign agents are hidden by default, orcr stops feeling like it sees the user's real terminal workspace.

The danger is semantic overreach: foreign rows do not have run dirs, turn history, lineage, or reliable harness adapters. Mixing them with managed rows can make scripts assume a uniform contract that does not exist.

Recommendation: keep merged default for human `orcr ls`. For JSON, include an explicit `origin` and `capabilities` object per row, e.g. `{"turns":false,"run_dir":false,"kill_requires_force":true}`. Keep `--managed` and `--foreign`; consider making SDK helpers default to managed-only unless asked for foreign.

## R8. Naming

`ls` is fine. It is short, script-friendly, and consistent with paseo. `list` can be an alias if herdr muscle memory matters, but do not document both equally.

`out` is less good. It is terse but opaque to new users, and `logs` is a better mental model for terminal-backed agents. The draft's `out --format body|path|json` is really "retrieve the captured response/output." `logs` with `--format body` is not perfect either, but it is discoverable.

`adopt` is understandable but product-heavy; it implies ownership transfer. `manage` is blander and possibly more accurate. If the command stays rare/advanced, `adopt` is acceptable.

`daemon` should probably be `server` if the substrate is herdr and the draft wants herdr integration to feel native. However, paseo uses `daemon`, and users understand "daemon status." The bigger problem is having both `status` and `daemon status`.

Recommendation: document `ls`, maybe alias `list`. Rename `out` to `logs` or provide `logs` as the documented command and keep `out` as compatibility sugar. Collapse `status` and `daemon status` before bikeshedding `daemon` vs `server`.

## R9. Anything in §4 still cuttable? Anything cut that will bite?

Still cuttable or mergeable:

- `status` and `daemon status` should merge. One health command is enough.
- `show` and `ls` should not fully merge, but `ls --json <id>` should not become a second object API. Keep `show` for one object; keep `ls` for collections.
- `adopt` is the most questionable top-level verb. Keep the capability, but lower its prominence.
- `top` is optional. If herdr UI is the viewer, `top` should be explicitly secondary or shipped as an extension/TUI helper, not treated as core.
- `gc` may be necessary operationally, but it is maintenance surface. If auto-retention can handle 95 percent of cases, keep `gc` as plumbing/advanced.

Cut items that may bite:

- Removing `workflow` is fine only if the SDK gets group metadata and cleanup semantics. Without `withGroup()` and recursive cleanup, every SDK user will reimplement grouping badly.
- Removing `goal` is fine only if the recipe is first-class in docs/tests. Worker/judge loops are a core AI-agent pattern; they should be outside the CLI, not outside the product.
- Removing `job` is fine because only schedules remain. If future durable job types appear, do not resurrect generic `job`; add capabilities under the durable primitive or an SDK scheduler API.

Recommendation: reduce the visible core to `run`, `ls`, `show`, `send`, `wait`, `logs/out`, `attach`, `kill`, `schedule`, `server/daemon status/start/stop`, and maybe `gc`. Treat `top`, `events`, and `adopt/manage` as advanced/plumbing unless proven common.

## Additional Findings

### High: The design needs an explicit capability model for foreign targets

Foreign agents are first-class in the prose, but not equivalent in contract. They lack run-dir history, turn boundaries, lineage, response guarantees, and sometimes harness-specific output parsing. If the CLI presents them as normal rows without machine-readable limitations, SDKs will build brittle assumptions.

Recommendation: every target resolution should produce a capability set: `send`, `wait`, `attach`, `kill`, `turns`, `run_dir`, `recursive_out`, `structured_status`, `requires_force`. Human output can compress this; JSON must expose it. Commands should fail with a precise "capability unavailable" error rather than silently degrading when the caller requested structured behavior.

### High: Do not add an orca-style coordination primitive yet

Inbox, ask, gates, task boards, and dispatch ids are real needs for multi-agent systems, but adding them to this CLI would reverse the simplification. orca is a coordination layer; orcr should be the terminal/session substrate. The moment orcr adds `ask` or `gate`, it owns semantics for authority, routing, persistence, and conflict resolution.

Recommendation: leave coordination to files, SDK helpers, and recipes for v2. Make sure the substrate can support it: stable ids, parent/group metadata, event stream, run-dir paths, and atomic file writes. A future coordination package can build `inbox/ask/gates` on top without changing the core verbs.

### Medium: `schedule` is the right surviving job primitive, but it is overloaded

`schedule add` currently covers cron, interval loops, one-shot `--at`, max runs, until regex, catchup, expiry, and run flags. That is still the right noun, but the command risks becoming the place where every durable automation feature accumulates.

Recommendation: keep the primitive, but narrow v2 semantics: one-shot, cron/every, catchup policy, pause/resume/rm, and run template. Treat `--until <regex>` and `--max` as schedule termination policy, not a general loop engine. Avoid adding worker/judge, fan-out, or dependency graphs under `schedule`.

### Medium: `status` and `daemon` should merge

`orcr status` and `orcr daemon status` create a distinction users should not have to learn. A minimal CLI should have one obvious place to answer "is orcr healthy and what session is it targeting?"

Recommendation: keep `orcr status` as the public command and include daemon/server health, target herdr session, event backlog, schedule runner state, and version. Make `orcr daemon start|stop` or `orcr server start|stop` advanced control commands. If a subcommand status remains, alias it to `orcr status`.

### Medium: Permission and remote-host future-proofing need flags in the data model, not verbs

The draft mentions sessions but not remote hosts or permission surfaces. paseo has `--host` and `permit`; adding those verbs now would bloat v2. But ignoring them in identifiers and JSON will make them painful later.

Recommendation: include `host` and `authority` fields in internal target identity and JSON output now, even if they always default to local/current. Reserve global `--host` without implementing remote transport if necessary. Model permissions as events/capabilities first; defer `permit` commands until a concrete permission broker exists.

### Medium: The default workspace matching policy needs guardrails for monorepos

The draft's "existing workspace whose cwd equals agent cwd else nearest ancestor" will surprise users in monorepos and home-directory workflows. It can place many unrelated package tasks into one broad workspace.

Recommendation: prefer git-root-aware matching and require explicit metadata for broad ancestor matches. Add a `placement.workspace` config with `auto | current | git-root | cwd | new | <label>` rather than only `auto | current | new | <label>`.

### Low: `show` and `ls` should stay separate, but their JSON schemas must align

Merging `ls` and `show` fully would make `ls` too polymorphic. Keeping both is cleaner: collection vs object. The risk is schema drift where `ls --json` rows and `show --json` objects describe the same agent differently.

Recommendation: define a shared `AgentRef`/`AgentSummary` schema used by both, with `show` returning an expanded object. Do not add `ls <id>` as a hidden alternate show path.

### Low: `top` is no longer core if herdr is the viewer

The draft correctly drops the auto-viewer, but keeping `top` in the main surface muddies the claim that herdr UI is the viewer. A separate terminal TUI is useful in SSH, CI, or non-herdr clients, but it is not primitive.

Recommendation: keep `top` as optional/advanced. Do not include it in the minimal mental model or "14 verb" pitch. If maintained, make it read from `events` and require no special core semantics.

### Low: `--mode tui|exec` needs sharper boundaries

`orcr` is positioned around real interactive TUI sessions. `--mode exec` may be useful for harnesses with non-interactive modes, but it can weaken the model if exec agents do not have attachable panes or herdr agent states.

Recommendation: define whether exec mode still runs inside a herdr pane. If yes, it is just a harness launch mode. If no, it is outside the core promise and should be deferred.

## Minimal Core I Would Ship

For v2, the smallest durable substrate that still lets an SDK rebuild v1 is:

- `run`, with wait-by-default, `--detach`, placement output, env contract, run-dir contract, parent metadata.
- `send`, `wait`, `logs/out`, `attach`, `kill`, with explicit capability checks for foreign targets.
- `ls` and `show`, with merged live visibility but machine-readable origin/capabilities.
- `schedule`, only for durable time-based invocation.
- `status` plus advanced `daemon/server start|stop`.
- `events` as documented SDK plumbing, even if hidden from the human quickstart.

That core is enough to rebuild v1 `loop`, `goal`, and `workflow` in an SDK if group metadata, recursive operations, and event streaming are reliable.

## Verdict

The draft is directionally right: keep `schedule` as the only durable job primitive, cut `goal/workflow/loop` from the CLI, default to visible herdr sessions, and make `run` wait by default. That is the correct minimalism.

The main fixes before implementation are to lower the prominence of `adopt`, merge `status`/`daemon status`, define foreign-target capabilities explicitly, and future-proof identity for host/authority without adding remote or permission verbs yet. Do not add orca-style inbox/ask/gates to core orcr; preserve enough substrate semantics for an SDK or coordination layer to build them cleanly.
