# M7 · SDK & skill — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral choices
worth knowing, and discovered facts.

## Deviations from spec

- **Scaffold `workflow.ts` omits an explicit `agent`.** The spec sketch shows `agent: "claude"`;
  the generated boilerplate instead relies on `config defaults.agent` (agent is optional per §8),
  so the scaffolded project runs green against whatever provider the user configured (and the
  mock in CI). Everything else (scope → run --name → wait → last-response + skill-ref comment)
  matches §6.6.
- **SDK dependency spec override (`ORCR_SDK_SPEC`).** `orcr scaffold` writes
  `@orchestratr/sdk` pinned to the CLI's own version by default (satisfies "pinned version ==
  CLI version"). Because the package is unpublished (version `0.0.0`), `ORCR_SDK_SPEC` overrides
  the dependency value with an installable spec (a `file:`/tarball path) so `npm install` +
  `npx tsx workflow.ts` run green locally/CI. Recorded here as under-specified-by-spec.

## Decisions on under-specified points

- **SDK resolves §5.1 paths client-side, sends absolute selectors.** `path.ts` is a 1:1 port of
  `src/path.rs`; the SDK composes the effective absolute path from the AsyncLocalStorage scope
  (base = `context.fromEnv().scope`) and sends it as an absolute selector (`/…`), so the server
  never double-applies scope while lineage (`caller_id`/`caller_path` from the process env) is
  preserved. Property-tested (`test/scope.test.ts`) against an oracle, and cross-checked against
  the live server (`e2e_sdk_scope_matches_cli`).
- **`orcr.ask()` uses the `agent.ask` protocol method** (one round trip) rather than composing
  run→wait→lastResponse client-side; semantics are identical and it's what the spec documents
  the sugar as.
- **Generated client is committed + drift-checked** (`npm run codegen:check` in CI): `generated.ts`
  is generated from `orcr api schema`; the codegen test asserts 100% method coverage AND a
  callable method per protocol method.
- **`loop.run.start` `dataDir`** is computed SDK-side (`<home>/data/<loop>/<run_id>`) since the
  protocol result doesn't carry it.
- **Skill live-drill** (fresh agent + SKILL.md → hot path) is validated structurally + by the
  doc-tests (no stale flags; run/ask samples carry --name/--path); a real-agent drill is
  best-effort (master-prompt §6 makes real-provider validation best-effort).
- **Real-provider smoke of recipes** (claude+codex) deferred to the manual-e2e phase; the mock
  against live herdr is the automated gate (all §9 recipes pass in `recipe_e2e`).

## Discovered facts / gotchas

- **Mock transcript adapter.** To let recipe/SDK e2e exercise `logs`/`ask`, the mock agent now
  writes a claude-format `transcript.jsonl` into its data dir and reports a normal `id`-kind
  session; `driver/transcript.rs::locate_transcript` gained a `data_dir` param + a `mock`-provider
  branch that reads `<data_dir>/transcript.jsonl` directly (self-contained — never touches the
  user's `~/.claude`). Tried reporting a herdr `agent_session_path` (path-kind) instead, but
  herdr does **not** surface a path-only pointer via `pane.get` (agent stays `starting`), and
  reporting a path-shaped value as the `id` isn't surfaced either — hence the data-dir adapter.
  New mock directives: `@say=<word>` (exact response text), `@write=<relpath>` (file
  convention), and `ORCR_MOCK_NO_TRANSCRIPT` (opt out — used by the M3 `transcript_unavailable`
  test).

- **OPEN ISSUE — concurrent-burst `agent.start` reliability (engine/herdr, not M7 SDK).** When
  many *instantaneous* agents spawn in a simultaneous burst against one owned herdr session
  (e.g. 4 fan-outs each issuing a `Promise.all` of `agent.run` at the same instant), herdr's
  `agent.start` intermittently returns a pane whose **command never actually launches** (data
  dir has only `launch.json` — no `mock_env.json`/`transcript.jsonl`), and orcr's fast-turn
  completion path then falsely marks that never-started agent `ended (completed)`. Reproduces at
  ~4+ concurrent instant-agent workflows; 2 concurrent (e.g. one fan-out + one tournament) is
  rock-solid across many runs. Real providers take seconds to initialize and reliably report
  `working`, so this mock-exposed race is unlikely to bite real usage — but two engine
  follow-ups are worth considering: (1) serialize/throttle `agent.start` on the owned session;
  (2) make the fast-turn path require *some* evidence the command ran (e.g. a reported session)
  before concluding completion. Serializing the spawn side-effects under `spawn_lock` was tried
  and did **not** help (the failure is herdr not launching the command), so it was reverted.
  The automated gate proves scope isolation at 2-concurrent (`e2e_concurrent_fanout_and_tournament`);
  the literal 4-way fixture is kept as `#[ignore]`d `e2e_concurrent_burst_high` for the engine
  follow-up.

- **FIXED — `server_protocol` (M1) leaked the real `orcr` herdr session.** Its throwaway home
  had no config, so `server start` bootstrapped the **default** owned session `orcr` on every
  `cargo test` (a safety-rule violation, present since M1). `TestHome` now writes a config with a
  disposable `orcr_test_<rand>` session and a `Drop` guard stops+deletes it. Verified: a full
  `cargo test` now leaves only the user's `default` session.

## What shipped

- `src/scaffold.rs` + `orcr scaffold` CLI verb (§6.6); unit + e2e tested.
- `sdk/ts/` — `@orchestratr/sdk`: wire transport, §5.1 path grammar, AsyncLocalStorage scopes,
  `context.fromEnv`, typed errors, generated protocol client + codegen, convenience layer
  (agent handle/collections/ask/scope/watch/loop/server/api/attach). Unit tests: path, scope
  property, context, codegen coverage.
- `sdk/ts/recipes/` — the §9.1–9.7 fixtures (+ `_common` stubs, loop-until-done project).
- `skill/SKILL.md` + `references/{cli,sdk,patterns,loops,files}.md`; doc-tests in
  `tests/skill_docs.rs`.
- `tests/recipe_e2e.rs` — recipes/scaffold/loop/scope e2e against live herdr + mock.
- `README.md` quickstart.

## Verifier & reviewer history

_(record each verify/review round's verdict + how issues were resolved)_
