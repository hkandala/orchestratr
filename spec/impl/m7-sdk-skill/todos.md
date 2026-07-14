# M7 · SDK & skill — todos

Ships: TS SDK, orcr scaffold, tested recipes, SKILL.md + references, packaging + docs.

## Tasks

- [x] Read master-prompt.md + full spec.md + this milestone file + herdr-driver-reference.md

### `orcr scaffold` (Rust, §6.6)
- [x] `src/scaffold.rs` — generate exactly 3 files (package.json, tsconfig.json, workflow.ts)
- [x] package.json pins `@orchestratr/sdk` to CLI version + tsx + typescript
- [x] workflow.ts ~15-line runnable example (scope → run --name → wait → last-response) + skill-ref comment
- [x] Preflight: Node ≥ 20 + npm present else `environment_error` + install pointer, nothing created
- [x] Never overwrite: any of the 3 files present → `state_conflict`, nothing touched
- [x] `<dir>` default `.`, created if missing; runs `npm install`
- [x] Purely local (no server auto-start, no store row)
- [x] `ORCR_SDK_SPEC` override for local/CI install (see notes)
- [x] Wire `Scaffold` into cli.rs Command enum + dispatch + `--json`
- [x] Unit tests (file generation, preflight, state_conflict, version pin)
- [x] e2e: scaffold + `npx tsx workflow.ts` green against mock provider

### TS SDK (§8)
- [x] `sdk/ts/` package skeleton (package.json, tsconfig.json, exports, build)
- [x] `wire.ts` — unix socket client: framing, handshake, request, subscribe, auto-start
- [x] `path.ts` — §5.1 grammar ported (resolve_create/resolve_selector/Pattern/expand_rand)
- [x] `scope.ts` — AsyncLocalStorage path scope; nesting; killOnThrow barrier
- [x] `context.ts` — `context.fromEnv()` (agent/loopRun/root, dataDir, loop)
- [x] `errors.ts` — one class per §13 code + fromWire mapping
- [x] `generated.ts` + codegen (`codegen.ts` reads `orcr api schema`) — 1:1 all methods
- [x] convenience: `agent.run()` handle (uuid/path/name/dataDir/wait/send/logs/followLogs/lastResponse/kill)
- [x] collections: `agent.wait/ls/kill` (patterns, CLI-identical)
- [x] `orcr.ask()`
- [x] `orcr.scope(path, fn, {killOnThrow})`
- [x] `orcr.watch({...})` AsyncIterable
- [x] `orcr.loop.*` (create/pause/resume/rm/ls/logs + run.start/stop/ls) + `loopNameFrom()`
- [x] `orcr.server.*`, `orcr.api.*`, `agent.prepareAttach`
- [x] typed errors surfaced from protocol
- [x] SDK unit tests (path, scope property test SDK==CLI, codegen coverage 100%, context)
- [x] CI check: generated client covers 100% schema methods (drift fails build)

### Recipes (§9)
- [x] 9.1 fix-until-green
- [x] 9.2 fan-out-and-merge
- [x] 9.3 classify-and-act
- [x] 9.4 adversarial-verification
- [x] 9.5 generate-and-filter
- [x] 9.6 tournament
- [x] 9.7 loop-until-done + durable handoff
- [x] stubs for illustrative helpers (stillCheap/queueSize/workOneItem)
- [x] CI harness: run each recipe against mock provider (live herdr)
- [x] concurrency fixtures: 2× fan-out + 2× tournament concurrently (distinct scopes) clean

### Skill (§10)
- [x] `skill/SKILL.md` (≤ ~150 lines, all 11 sections, trigger frontmatter)
- [x] `skill/references/cli.md`
- [x] `skill/references/sdk.md`
- [x] `skill/references/patterns.md` (the §9 recipes)
- [x] `skill/references/loops.md`
- [x] `skill/references/files.md`
- [x] doc-test: references contain no stale flags (vs `--help`)
- [x] doc-test: reject any run/ask sample missing --name/--path

### Packaging & docs
- [x] README quickstart
- [x] `orcr --help` polish pass (verify verbs)
- [x] docs site source note (deferred — see notes)

## Acceptance criteria

- [x] SDK covers 100% of schema methods (generated-client CI check)
- [x] Every §9 recipe runs e2e against mock provider in CI (`recipe_e2e::e2e_recipes_run_against_mock` + loop-until-done); fix-until-green + fan-out-merge real-provider smoke DEFERRED to manual-e2e (best-effort, master-prompt §6)
- [x] `orcr.scope` nesting composes same effective paths as CLI (property test: `sdk/ts/test/scope.test.ts` + live `e2e_sdk_scope_matches_cli`)
- [x] Skill drill: fresh agent + SKILL.md → hot path (validated by structure + doc-test; live drill best-effort)
- [x] reference files contain no stale flags (doc-test vs --help: `tests/skill_docs.rs`)
- [x] doc-test rejects run/ask sample missing --name/--path
- [x] Concurrency fixtures: **two copies each** of fan-out-and-merge + tournament, started
  concurrently under distinct top scopes, run clean — `e2e_concurrent_burst_high` (the full
  4-way, now un-`#[ignore]`d, 3/3 green) + `e2e_concurrent_fanout_and_tournament` (2-way). Root
  cause was **not** a herdr concurrent-burst limit: two copies of the same scope-parameterized
  recipe (`review_a/fanout/file_0` vs `review_b/fanout/file_0`) collided on the herdr agent
  `name`, because `tab_label` dropped the level-1 scope and herdr 0.7.2 enforces session-global
  name uniqueness (`agent_name_taken`). Fixed by using the **full effective path** as the herdr
  agent name/label (`path::herdr_name`). See notes.md → "Verify round 2".
- [x] Scaffold: clean checkout → scaffold + npx tsx workflow.ts green vs mock; re-run → state_conflict; missing-node → environment_error nothing created; pinned SDK version == CLI version
- [x] Package installs clean (via file: spec / tarball in CI); quickstart works

## Deferred / out of scope

- Python SDK, presets, declarative workflows, notifications (§17)
- npm publish to public registry (version 0.0.0; final name+publish is a release step) — SDK is installable locally via file: spec
- docs site (orchestratr.dev) hosting — README is the shipped quickstart; see notes
- real-provider smoke of recipes — best-effort (mock is the automated gate per master-prompt §6)
