# M7 · SDK & skill

Make orcr programmable and teachable: the TypeScript SDK (a first-class client of the
socket API), the tested workflow recipes, and the skill that teaches any agent the
vocabulary. Ends with publishable packages and docs.

## Scope

### TS SDK (spec §8)
- **Generated protocol client**: every socket method from `api schema`, 1:1, typed —
  regenerated in CI so drift fails the build.
- **Convenience layer** (each helper documents its underlying calls):
  - `orcr.agent.run()` → handle (`uuid`, `path`, `name`, `dataDir` —
    mirroring `ORCR_AGENT_DATA_DIR`/`ORCR_LOOP_DATA_DIR`,
    `wait()`, `send()`, `logs()`, `followLogs()` (AsyncIterable), `lastResponse()`,
    `kill()`).
  - Collections take §5.1 patterns, CLI-identical: `orcr.agent.wait/ls/kill`.
  - `orcr.ask()` — run(gc: immediate) → settle wait → lastResponse (naming rules
    identical to run).
  - `orcr.scope(path, fn, { killOnThrow? })` — AsyncLocalStorage path scope (not
    process-global); nests with inherited context; killOnThrow = barrier-kill of
    `<scope>/**`.
  - `orcr.watch({ pattern?, agent?, status?, managed?, sinceSeq? })` —
    snapshot-then-subscribe AsyncIterable of typed events.
  - `orcr.loop.*` (create/pause/resume/rm/ls/logs + `loop.run.start/stop/ls`),
    `orcr.loopNameFrom()`,
    `orcr.server.*`, `orcr.api.*`.
  - Typed errors: one class per §13 code (`NotFound`, `InvalidRequest`,
    `StateConflict`, `Blocked`, `Timeout`, `IntegrationMissing`,
    `TranscriptUnavailable`, `EnvironmentError`, `ServerError`); force-required is
    `StateConflict` details.reason.
- Data conventions surfaced: `a.dataDir` mirrors `ORCR_AGENT_DATA_DIR`
  (path-mirrored, uuid leaf); `loop.run.start()` returns the run's `dataDir`;
  `context.fromEnv()` exposes both.

### `orcr scaffold` (spec §6.6)
- `orcr scaffold [<dir>] [--json]` — generates exactly three files
  (`package.json` with `@orchestratr/sdk` pinned to the CLI's own version + `tsx` +
  `typescript`, `tsconfig.json`, `workflow.ts` with the ~15-line runnable example +
  one skill-reference comment) into `<dir>` (default `.`, created if missing), then
  runs `npm install`.
- Preflight: Node ≥ 20 + npm present, else `environment_error` with install
  pointer and **nothing created**; any of the three files already present →
  `state_conflict`, nothing touched.
- Purely local: no server auto-start, no store row.
- Placement is convention only (taught by the skill, §8/§10): one-time →
  `$ORCR_AGENT_DATA_DIR/workflows/`; reusable + every loop script →
  `~/.orcr/workflows/<name>/`.

### Recipes (spec §9)
- The §9 examples (9.1–9.7) as **self-contained tested fixtures** in the repo
  (illustrative helpers like `stillCheap()` replaced by real stubs; run in CI
  against the mock provider; smoke-tested against real providers):
  fix-until-green, fan-out-and-merge, classify-and-act, adversarial verification,
  generate-and-filter, tournament, loop-until-done + durable handoff.

### The skill (spec §10)
- `skill/SKILL.md` (≤ ~150 lines, per §10: trigger-phrase frontmatter, the
  decision ladder, hot path (every example carries --name/--path), specific
  workflow roots + {rand}, the open-top-beside-yourself rule, file/data
  conventions, provider routing table, numeric discipline, guard rails, output
  checklist, reference pointers).
- `skill/references/`: `cli.md`, `sdk.md` (SDK surface + scaffold workflow-project
  guidance: when to scaffold, where projects live, adding npm deps), `patterns.md`
  (the §9 recipes), `loops.md`, `files.md` — loaded by agents on demand.
- Validation: a real agent given only SKILL.md completes the hot path (spawn → wait →
  read → kill) unassisted; references stay in sync with `--help` output via doc
  tests.

### Packaging & docs
- npm publish (`@orchestratr/sdk`, final name decided at publish); README quickstart;
  `orcr --help` polish pass across every verb; docs site source (orchestratr.dev)
  from the spec.

## Acceptance

- SDK covers 100% of schema methods (generated-client CI check).
- Every §9 recipe runs end-to-end against the mock provider in CI; fix-until-green
  and fan-out-merge smoke-tested against real claude + codex.
- `orcr.scope` nesting: SDK-in-loop-in-agent composes the same effective paths as
  the CLI path (property test).
- Skill drill: fresh agent + SKILL.md only → completes the hot path; reference files
  contain no stale flags (doc-test against `--help`); doc-test rejects any run/ask
  sample missing --name/--path.
- Concurrency fixtures: two copies of fan-out-and-merge and tournament started
  concurrently (distinct top scopes) run clean.
- Scaffold: on a clean checkout, `orcr scaffold tmpdir && cd tmpdir && npx tsx
  workflow.ts` runs the boilerplate green against the mock provider; re-running
  scaffold on the same dir fails `state_conflict`; missing-node preflight fails
  `environment_error` with nothing created; pinned SDK version equals the CLI
  version.
- Package installs clean on a fresh machine; quickstart works as written.

## Out of scope

Python SDK, presets, declarative workflows, notifications — future work (§17).
