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

- **herdr agent `name` = the full effective path (deviation from §5.2's tab-label sketch).**
  §5.2 sketches the herdr tab label as "the path after the first segment"
  (`review/worker` → tab `worker` in workspace `review`). herdr 0.7.2 requires the agent `name`
  (which is also the pane/tab label) to be **session-globally unique**, so the path-after-first
  form collides across distinct top-level scopes. orcr therefore names/labels each agent with
  its **full path** (`path::herdr_name`), which is session-unique by construction. The only
  visible effect is that the tab shows the full path rather than the path-after-first-segment;
  identity/routing (by `terminal_id`) and the workspace-per-level-1 model are unchanged. Forced
  by herdr reality; recorded per master-prompt §4.

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

- **RESOLVED (verify round 2) — the "concurrent-burst" failure was a herdr agent-`name`
  collision, not a launch race.** Diagnosing the ignored 4-way fixture with server-log
  instrumentation showed the real error: `agent.start` returned `agent_name_taken`
  ("agent name `fanout/file_0` is already used") for the *second* copy of a scope-parameterized
  recipe. herdr 0.7.2 enforces that an agent's `name` is **unique across the whole session**,
  but `tab_label` (the §5.2 path-after-first-segment) dropped the level-1 scope, so
  `review_a/fanout/file_0` and `review_b/fanout/file_0` both mapped to herdr name
  `fanout/file_0` → the second start was rejected, its row failed, and the follow-on
  `agent.wait` returned while the promised `response.md` was missing (ENOENT). The earlier
  "command never launches / fast-turn false-completes" reading was a **misdiagnosis** (a
  downstream effect of the rejected start; a rejected pane also cascades an
  `agent_placement_not_found` for its sibling as the empty workspace is reclaimed). **Fix:**
  `path::herdr_name(path)` returns the **full effective path** as the herdr agent name/label
  (orcr already guarantees active paths are unique), used at `agent.start`
  (`server/engine.rs`), in reconcile orphan-matching (`server/engine.rs`), and in the GC
  park/un-park pane-move labels (`server/gc.rs`). No throttle/serialization was needed — the
  4-way `e2e_concurrent_burst_high` now passes 3/3 and is **un-`#[ignore]`d**.

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

### Verify round 1 → FAIL; revised

- **CRITICAL (fixed) — `tests/server_protocol.rs` had two `impl Drop for TestHome`**
  (E0119 conflicting implementations), so `cargo test` never compiled the server_protocol
  target and the whole default suite exited 101. Merged into a single `Drop` that first
  `reap_server()`s, then stops+deletes the disposable herdr session. `cargo test` now compiles
  and passes; `server_protocol` 6/6 with no session leak (only `default` remains). Commit
  `d13bdf9`.
- **(fixed, adjacent) — flaky `lock::tests::second_acquire_is_blocked_then_freed_on_drop`.**
  Once `cargo test` compiled again, this M0 unit test flaked under heavy parallel load: the
  `flock` release on `drop`'s `close(2)` occasionally isn't reflected to an immediately-following
  `flock` on a fresh fd, so the re-acquire returned `None`. The assertion demanded an
  *instantaneous* re-acquire, stricter than reality (the real auto-start reaper already tolerates
  release latency via its stable-dead probe window). Now polls briefly for the re-acquire.
  `cargo test` green across 3 consecutive full runs. Commit `15e3159`.
- **LOW (fixed) — standalone `npm test` crashed ENOENT.** The codegen drift test fell back to
  `orcr` on PATH. Now resolves the binary `$ORCR_BIN → target/{debug,release}/orcr → PATH` and
  skips the two live-schema tests cleanly when none exists, so `npm test` is reproducible on a
  fresh checkout after `cargo build` (the drift check still runs — verified `skipped 0`, 20/20 —
  and CI still sets `ORCR_BIN`). Commit `6104870`.
- **MEDIUM (knowingly deferred) — concurrency acceptance proven at 2-concurrent, not the strict
  4-way.** Attempted the verifier's option (a) engine follow-up "require evidence the command
  ran before fast-completing": gating `fast_ok` on a reported herdr `agent_session`. This
  **empirically broke** the mock completion contract (5/8 `completion_e2e` failed) because the
  **mock deliberately reports no herdr session** — it uses the data-dir transcript adapter (see
  "Mock transcript adapter" above), so `agent_session_value`/`info.agent_session` are always
  `None` for a launched mock. Gating on transcript-locatable instead breaks the legitimate
  no-transcript case (`ORCR_MOCK_NO_TRANSCRIPT`, an agent that *did* launch). There is no clean
  engine-level signal distinguishing a never-launched pane from a launched-no-transcript one
  (both sit `idle`; the only difference is the mock-specific `mock_env.json` in the data dir).
  Serializing spawn side-effects was already tried and reverted (the failure is herdr not
  launching the command). Reverted the completion change — `src/server/completion.rs` is
  byte-identical to the verified baseline. **Root cause is a herdr concurrent-burst
  `agent.start` limitation, not an orcr defect, and real providers (seconds to init, reliably
  report `working`) don't hit it.** Note also that of the two recipes, only fan-out bursts
  (`Promise.all` of N `agent.run`); **tournament is fully sequential** (awaits each `orcr.ask`),
  so the strict "two copies of fan-out-and-merge" is what pushes ≥4 simultaneous `agent.start`
  into the failing burst window. Resolution: the automated gate
  (`e2e_concurrent_fanout_and_tournament`) proves scope isolation between concurrent
  distinct-scope workflows (rock-solid across runs); the literal 4-way
  (`e2e_concurrent_burst_high`) stays `#[ignore]`d as a documented engine follow-up. This
  acceptance item is **knowingly deferred**, pending milestone-owner sign-off, rather than
  claimed as fully met.

  **SUPERSEDED in verify round 2** — the deferral was based on a misdiagnosis; the real cause
  (herdr session-global agent-`name` uniqueness vs a scope-dropping `tab_label`) is fixed and
  the fixture now passes. See below.

### Verify round 2 → FAIL; revised

- **HIGH + MEDIUM (fixed) — concurrency fixture now genuinely passes; the completion "false
  done" was a symptom of a herdr `agent_name_taken` collision, fixed at the source.** The
  ignored 4-way fixture was reproduced and instrumented: the failing step was `agent.start`
  returning `agent_name_taken` for the second copy of a scope-parameterized recipe, because the
  herdr agent `name` was `tab_label` (path-after-first-segment), which is not unique across
  top-level scopes, while herdr 0.7.2 enforces session-global name uniqueness. Fixed by
  `path::herdr_name` = the full session-unique path, applied at `agent.start`, reconcile
  orphan-matching, and GC park/un-park labels. No throttle/serialization needed (both were
  tried in this round and reverted — pure serialization did *not* help because the failure was
  never a launch race). `e2e_concurrent_burst_high` (two copies each of fan-out + tournament)
  un-`#[ignore]`d, passes 3/3; `agent_e2e`/`gc_e2e`/`loop_e2e`/`recipe_e2e` all re-verified
  green against live herdr with no `orcr`/`orcr_test_*` session leak. Test label assertions
  that hard-coded the old `tab_label` (`agent_e2e`, `completion_e2e::pane_of`) were updated to
  the full path; the crash-recovery orphan test now creates its orphan pane with the full-path
  name so the reconciler matches it.
- **LOW (fixed) — `orcr.ask()` now throws `TranscriptUnavailable`** when the `agent.ask`
  response carries no text, sharing one contract with `AgentHandle.lastResponse()` (was a
  silent `""`). `sdk/ts/src/client.ts`; SDK `npm test` 20/20 green.
- **(adjacent) — `recipe_e2e` teardown hardened** to retry `session_stop`/`session_delete`
  until `find_session` reports the disposable session gone (herdr can transiently reject a
  stop/delete right after the owning server is `kill -9`'d), after one observed transient
  `orcr_test_*` leak. No leak across subsequent full-suite runs.

- **PRE-EXISTING, out of scope — `completion_e2e::e2e_logs_transcript_unavailable_for_mock`
  fails in this environment** (verified: fails identically on the pre-change baseline via
  `git stash`). The test sets `ORCR_MOCK_NO_TRANSCRIPT` on the *orcr server* process, but the
  disposable herdr session is bootstrapped by the test harness's own `ensure_session` **before**
  the orcr server starts, so the herdr server (and the mock panes it spawns) never inherit that
  var; the mock writes a transcript and `logs --last-response` returns text instead of
  `transcript_unavailable`. This is an M3 test-harness/env-inheritance issue that predates this
  round and is unrelated to the M7 findings — noted here, not fixed (would be scope creep into
  M3).
