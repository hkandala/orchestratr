# orcr CLI v2 Review: LLM/Agent-Caller Ergonomics

Lens: can a non-human caller learn this from a compact skill file and use it reliably in shell scripts? I am weighting determinism, parseability, failure-mode clarity, stable defaults, and whether degraded foreign-target behavior creates silent surprises.

## R1. `run` Waits By Default

**Answer:** Wait-by-default is right for simple human and LLM one-shot use, but only if the blocking contract is explicit and script-safe. The current draft says `run` waits and prints the response body, which matches the common "ask another agent and use the answer" pattern. That is excellent for LLM callers because it removes the common race of `run` then immediately `out` before the first turn is complete.

**Cost of the flip:** The real cost is fan-out and long-running tasks. In v1, `run` naturally returned an id for orchestration. In v2, every orchestration recipe must remember `--detach`, and agents will occasionally hang a parent process by forgetting it. That is fixable, but it must be front-and-center in the skill.

**Recommendation:** Keep wait-by-default, but make `--json` for waiting `run` return both the agent id and response body:

```json
{"ok":true,"result":{"id":"a7","status":"idle","response":"...","response_path":"...","placement":{...}}}
```

Also add `--no-body` or `--format id|body|json` if stdout body purity matters. A scriptable caller needs the id even when it waited successfully.

**Severity:** Medium. The default is good, but losing the id from the happy path would push agents into brittle parsing or extra `ls/show` calls.

## R2. One Tab Per Agent

**Answer:** One tab per agent is the right default for LLM callers. Splits are layout-heavy and introduce non-deterministic visual geometry; tabs are easier to name, attach, close, and reason about.

The tab-sprawl problem is real for 10-agent fan-outs, but it is mostly a human UI issue. For agents, the more important invariant is "one target maps to one tab/pane unless I asked for a split." That invariant is clear.

**Recommendation:** Keep one-tab-per-agent as default. Add an explicit fan-out convenience flag or config, such as `--group-tab <label> --split grid`, only as an advanced option. Do not make splits automatic based on count; that would be context-dependent behavior agents cannot predict.

**Severity:** Low. The proposed default is agent-friendly.

## R3. Default Session Is User's Herdr Session

**Answer:** Defaulting to the user's default herdr session is a strong product choice for the "visible, attachable, steerable" moat, but it is risky for LLM callers because it mixes automation with ambient human state.

The risks are target collisions, accidental steering of a human-started agent, noisy `ls`, and destructive commands in a shared namespace. A coding agent operating from a 100-line skill should not need to understand the user's whole herdr topology before spawning a reviewer.

**Recommendation:** Keep the user's default herdr session for interactive humans, but make automation mode easy and explicit. If `ORCR_AUTOMATION=1` or `--automation` is set, default to a namespaced session or workspace label such as `orcr:<repo>`, while still allowing `--session default`. At minimum, every mutating command should echo the resolved session/workspace in JSON and support `--managed-only` or `--origin orcr` filters.

**Severity:** High. Shared-session defaults can cause an agent to act on the wrong target or depend on irrelevant human state.

## R4. Is `adopt` Pulling Its Weight?

**Answer:** Yes, `adopt` is necessary. Foreign targets working "degraded everywhere" is useful for humans, but dangerous for LLM callers if it looks like the same contract. `send`, `wait`, and `out` mean materially different things on managed vs foreign targets: no run-dir, no response guarantee, no turn history, uncertain transcript adapter, and different kill safety.

**Recommendation:** Keep `adopt`, and make the managed/foreign distinction impossible to miss. Require `--allow-foreign` for degraded operations in non-interactive or `--json` mode, or return a warning field such as:

```json
"contract":"foreign_degraded",
"missing":["turn_tracking","response_file","replayable_prompt"]
```

For robust scripts, recommend `orcr adopt <target>` before multi-turn work.

**Severity:** High. Same verbs with degraded semantics will otherwise create silent false confidence.

## R5. Cutting Goal/Workflow/Loop

**Answer:** Cutting `workflow` and `goal` from the CLI is right if the SDK and recipes are real, tested, and shipped. Keeping `schedule` is also right because durability across caller death cannot be recreated by a short-lived script.

I am less convinced that cutting foreground `loop` entirely is harmless. `schedule add --every ... --max ... --until ...` is a durable loop, but a foreground loop is a common LLM pattern: run worker, run judge, feed back, stop on condition, return final answer now. That can be a recipe, but the recipe must be tiny and deterministic.

**Recommendation:** Cut the verbs, but ship official recipes with exact shell snippets and JSON contracts. Add an SDK helper for `with_group` and `iterate_until` if you want to avoid re-growing CLI verbs. Do not make agents rediscover state cleanup, tree wait, or orphan kill behavior from prose.

**Severity:** Medium. The cut is directionally right, but missing first-class recipes will cause each LLM skill to reinvent fragile orchestration.

## R6. Workspace-By-Cwd Matching

**Answer:** Git-root match should be the primary default, with exact cwd as a tie-breaker. Exact cwd creates surprising fragmentation in monorepos and when agents run from subdirectories. Ancestor match is better but can still pick a broad parent workspace when a nested git repo or worktree exists.

**Recommendation:** Resolve placement in this order:

1. Explicit `--workspace`.
2. Current workspace if inside herdr and no `--cwd` override.
3. Workspace whose cwd equals the git root for `--cwd` or caller cwd.
4. Workspace whose cwd equals exact cwd.
5. Nearest ancestor workspace.
6. Create new workspace at git root, or cwd if not in git.

Always report `workspace_resolution_reason` in JSON, for example `git_root_match`, `exact_cwd_match`, `ancestor_match`, or `created`.

**Severity:** Medium. The draft's exact/ancestor wording is usable for humans but too implicit for scripts.

## R7. `ls` Merging Managed And Foreign

**Answer:** Merging managed and foreign by default is good for human situational awareness, but bad as the default for LLM automation. An LLM caller usually wants resources it owns, with stable contracts. Foreign rows broaden the target set and increase collision risk.

**Recommendation:** Split defaults by output mode. Human table output can merge managed and foreign. `orcr ls --json` should default to managed only unless `--foreign` or `--all-origins` is passed. Alternatively, keep merged JSON but require an `origin` field and make examples always filter `origin=="orcr"` before acting.

**Severity:** High. Automated callers using `ls --json | jq ... | xargs orcr kill` should not accidentally include foreign panes.

## R8. Naming

**Answer:** The naming is mostly fine, but a few choices hurt learnability from a short skill file.

`ls` is acceptable for agent callers because it is terse and common in CLIs like paseo. `list` as an alias would help humans and align with herdr. `out` is compact but less discoverable than `logs`; for LLMs, either is learnable, but `out` should stay if it means "final response/output" rather than terminal logs. `adopt` is a good verb because it signals a contract transition. `daemon` is better than `server` for orcr because the user should not confuse the orcr scheduler/control process with herdr's server.

**Recommendation:** Support aliases without expanding the canonical skill surface:

- Canonical: `ls`, alias `list`.
- Canonical: `out`, alias `logs`.
- Canonical: `daemon`, alias `server` only if it delegates clearly or prints a hint.
- Keep `adopt`.

The skill should teach only canonical names to preserve the 100-line budget.

**Severity:** Low. Naming is not the main risk, but aliases reduce friction.

## R9. Anything Still Cuttable? Anything Cut That Will Bite?

**Answer:** `top` and `gc` are cuttable from the core LLM skill, though they can remain as human/admin commands. `status` and `daemon status` may overlap; for agents, one health command is enough.

What was cut that may bite is not `goal` as a verb, but the missing coordination primitives around permission requests, questions, and structured final outputs. The references show paseo has `permit`, and orca has task/inbox/gate concepts. orcr does not need to become orca, but agents need clear blocked-state and ask-human behavior if they are supervising other agents.

**Recommendation:** Keep the CLI surface, but document `top`, `gc`, and maybe `events` as out-of-skill/admin. Add a minimal machine-readable blocked/question contract before inventing more workflow verbs:

```json
{"status":"blocked","reason":"needs_approval","details":{...}}
```

If permission handling is delegated to harnesses, say so explicitly.

**Severity:** Medium. The visible verb list is okay, but blocked/approval states are under-specified for orchestration.

## Additional Findings

### Finding 1: Foreign-Target Polymorphism Needs A Hard Contract Boundary

**Severity:** High.

The design says foreign targets are first-class read/steer targets and "foreign targets work too" for core verbs. For LLM callers, that is the most dangerous part of the draft. A command like `orcr out a7` and `orcr out my-claude-pane` may both succeed while providing radically different guarantees. The former can point to a response file; the latter may be a pane scrape.

**Recommendation:** Add a required concept of `contract_level`: `managed_full`, `adopted_from_next_turn`, `foreign_adapter`, `foreign_scrape`. Include it in every `show`, `ls --json`, `send --json`, `wait --json`, and `out --format json` result. In non-interactive scripts, require `--allow-degraded` to operate below `managed_full`, except for read-only `show`.

### Finding 2: Context-Dependent Placement Defaults Are Too Rich For Agent Skills

**Severity:** Medium.

The section 2 matrix is clear for a human reviewer, but too much for a compact agent skill. The hard part is not learning the matrix once; it is predicting it after environment inheritance, nested agents, plain terminals, and foreign agents. The rows differ in workspace, adjacency, focus, and cwd semantics.

**Recommendation:** Preserve the behavior, but teach agents a deterministic rule: "For scripts, always pass `--cwd`, `--workspace`, and `--json`; use `--parent` when spawning subagents." Also add `orcr run --dry-run-placement --json` or `orcr place --explain --json` so a caller can inspect the exact resolved placement without spawning.

### Finding 3: The Parent/Lineage Contract Is Underspecified

**Severity:** Medium.

The draft mentions `ORCR_ID/PARENT/DEPTH/STORE/OUT` and `--parent <id>`, but does not fully state which wins when both env and flag exist, what happens when max depth is exceeded, or how lineage works for adopted/foreign agents.

**Recommendation:** Define precedence and failure codes:

- `--parent` overrides env parent.
- Missing parent id exits 6.
- Depth/agent limit exits 7 or a dedicated limit code.
- Foreign parents are allowed only after `adopt`, or lineage is marked `external`.

Agents need this to write reliable fan-out scripts.

### Finding 4: `send` Defaults Need More Machine-Safe Shape

**Severity:** High.

The draft inherits v1 steer-vs-turn semantics with intent flags and exit 7 conflicts, which is good, but the synopsis still makes intent optional. For LLM callers, optional intent invites ambiguous behavior: if the target is idle-kept, should this be a new turn; if working, should it steer; if state detection is stale, does it conflict?

**Recommendation:** In the LLM skill, require one of `--steer` or `--turn` for `send`. Consider making non-interactive `send --json` without intent return a conflict error that includes `current_status` and the valid next commands. Humans can keep the convenience default.

### Finding 5: `run` Auto-Close Versus `--keep` Needs Observable Finality

**Severity:** Medium.

`run` waits by default, prints the body, and auto-closes after the first completed turn unless `--keep`. That is clean for one-shots, but the distinction between "agent is gone," "pane closed," "turn complete," and "response persisted" matters for scripts.

**Recommendation:** `run --json` should report `exit_reason`, `closed:true|false`, `kept:true|false`, `response_path`, and `transcript_source`. If auto-close fails, the command should still return success for the turn but include a cleanup warning, not hide the lifecycle state.

### Finding 6: Output Body On Stdout Conflicts With Automation Metadata

**Severity:** Medium.

Printing the response body to stdout is ergonomic, but it means the caller cannot both stream/use the body and learn placement/id unless it opts into JSON. Many agents will prefer plain shell commands and then need the id for follow-up.

**Recommendation:** Make the skill strongly prefer `--json` for all agent calls. For plain output, add `--print id`, `--print body`, or `--print response-path`. Do not require scraping human text to recover ids.

### Finding 7: Error Details Need To Be Normative, Not Just Envelope-Shaped

**Severity:** Medium.

The envelope shape is good, but the design only names numeric exits and a generic `code,message,details`. LLM callers need stable machine codes for placement failure, herdr unavailable, ambiguous target, foreign degraded disallowed, state conflict, timeout, blocked, and limit exceeded.

**Recommendation:** Define a small stable error-code enum and require actionable details:

```json
{"code":"ambiguous_target","details":{"query":"reviewer","matches":[...]}}
```

For `timeout`, include whether the agent is still running and the id/pane so the caller can recover.

### Finding 8: `out` Source Fallback Can Hide Bad Answers

**Severity:** High.

The draft says managed output has a file-transcript-scrape guarantee and foreign output may use transcript adapter or pane scrape. From an agent-caller perspective, pane scraping should not be equivalent to a response file. It can include prompts, shell noise, partial output, UI chrome, or stale visible text.

**Recommendation:** `out --format body` may preserve convenience, but `out --format json` must include `source`, `confidence`, `turn`, `complete`, and `path` when available. Add `--require-source file|transcript|scrape` so scripts can fail instead of silently consuming low-confidence output.

### Finding 9: `kill` Safety For Managed Agents Also Needs Scope Guards

**Severity:** Medium.

Requiring `--force` for foreign kill is good, but managed agents in a shared user session can still be dangerous if adopted or if ids/names collide in unexpected ways. `kill --tree` in particular needs clear ownership and lineage boundaries.

**Recommendation:** For `kill --tree`, default to managed descendants only. Require `--include-foreign --force` to cross into foreign/adopted targets. JSON should report exactly which panes were closed and which were skipped.

### Finding 10: Scheduling Needs Idempotency For Agent-Generated Scripts

**Severity:** Medium.

`schedule add` is durable and daemon-backed, but the draft does not mention idempotency. LLM agents frequently retry commands after uncertain failures. Without a key, retries can create duplicate schedules.

**Recommendation:** Add `--key <stable-key>` to `schedule add`, with create-or-update or conflict semantics. Return the existing `s<N>` when the key already exists, depending on a documented `--replace` flag.

### Finding 11: JSON Should Be The Primary Agent Interface

**Severity:** Low.

The design says TTY human output and `--json` envelope. That is correct, but the agent-caller ergonomics should go further: every official LLM recipe should use `--json` and parse fields, never tables or prose.

**Recommendation:** State that the skill-facing contract is JSON-first. Human tables can evolve; JSON field names should be versioned and tested.

### Finding 12: There Is No Explicit CLI Version/Capability Probe

**Severity:** Low.

A 100-line skill file may outlive the installed CLI. Agents need a cheap way to know whether `adopt`, foreign contracts, schedule keys, or placement explanations exist.

**Recommendation:** Add `orcr status --json` fields for `version`, `schema_version`, `features`, `herdr_version`, and `daemon_required`. Avoid making agents infer support from failed commands.

## Verdict

The v2 simplification is directionally strong: fewer verbs, visible herdr-native agents, wait-by-default one-shots, and explicit adoption are the right foundations. For human operators, the design is close.

For LLM/agent callers, the current draft is not deterministic enough yet. The biggest issues are shared-session defaults, merged managed/foreign listing, foreign-target polymorphism, optional `send` intent, and output fallback that can silently degrade from response files to pane scraping. These are fixable without rewriting the design: make JSON the normative automation contract, expose resolved placement and contract level everywhere, require explicit degraded/foreign operation in scripts, and provide stable error codes plus source guarantees.

My recommendation is to keep the core verb set and wait-by-default `run`, but tighten the machine contract before implementation. If a Codex or Claude skill can say "always use `--json`; pass explicit `--cwd`/`--workspace`; require `managed_full`; parse `response_path`; use `--detach` for fan-out; require `--turn`/`--steer`," then v2 can be reliable for agents without giving up the herdr UI moat.
