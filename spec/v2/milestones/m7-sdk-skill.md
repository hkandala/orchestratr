# M7 · SDK & skill

Make orcr programmable and teachable: the TypeScript SDK (a first-class client of the
socket API), the tested workflow recipes, and the skill that teaches any agent the
vocabulary. Ends with publishable packages and docs.

## Scope

### TS SDK (spec §8)
- **Generated protocol client**: every socket method from `api schema`, 1:1, typed —
  regenerated in CI so drift fails the build.
- **Convenience layer** (each helper documents its underlying calls):
  - `orcr.agent.run()` → handle (`uuid`, `fqn`, `name`, `group`, `dataDir`,
    `wait()`, `send()`, `logs()`, `followLogs()` (AsyncIterable), `lastResponse()`,
    `kill()`).
  - Collections with CLI-identical subtree semantics: `orcr.agent.wait/ls/kill`.
  - `orcr.ask()` — run(gc: immediate) → wait(idle) → lastResponse.
  - `orcr.group(prefix, fn, { killOnThrow? })` — AsyncLocalStorage-scoped grouping
    (not process-global); nests with inherited context.
  - `orcr.watch({ prefix?, agent?, status?, managed?, sinceSeq? })` —
    snapshot-then-subscribe AsyncIterable of typed events.
  - `orcr.loop.*` (create/run/stop/ls/logs/pause/resume/rm), `orcr.loopNameFrom()`,
    `orcr.server.*`, `orcr.api.*`.
  - Typed errors from the §13 enum (`TranscriptUnavailable`, `IntegrationMissing`,
    `StateConflict`, `NotFound`, `ForceRequired`, …).
- Data conventions surfaced: `a.dataDir` (`~/.orcr/data/agents/<uuid>/`), run
  `dataDir` for loops.

### Recipes (spec §9)
- The §9 examples as **tested** code in the repo (run in CI against the mock
  provider; smoke-tested against real providers): fix-until-green (worker +
  cross-provider verifier), fan-out-and-merge, classify-and-act, adversarial
  verification, generate-and-filter, tournament, loop-until-done + durable handoff.

### The skill (spec §10)
- `skill/SKILL.md` (≤ ~150 lines: when to reach for orcr, the hot path, identity &
  grouping in three sentences, the file/data conventions, provider routing table,
  discipline, guard rails, reference pointers).
- `skill/references/`: `cli.md`, `sdk.md`, `patterns.md` (the §9 recipes), `loops.md`,
  `files.md` — loaded by agents on demand.
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
- `orcr.group` nesting: SDK-in-loop-in-agent composes the same effective groups as
  the CLI path (property test).
- Skill drill: fresh agent + SKILL.md only → completes the hot path; reference files
  contain no stale flags (doc-test against `--help`).
- Package installs clean on a fresh machine; quickstart works as written.

## Out of scope

Python SDK, presets, declarative workflows, notifications — future work (§17).
