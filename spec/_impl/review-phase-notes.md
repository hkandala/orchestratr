# Comprehensive review phase — consolidating verifier notes

Summary of what the final consolidating-verifier pass changed after the multi-dimension
review loop, and the tests added. All static gates (build, `clippy --all-targets -D
warnings`, `fmt --check`) are clean; the full unit suite, all seven e2e suites (against
live herdr 0.7.2 + the mock provider), the TS SDK unit tests, the codegen drift check,
live driver conformance, and `orcr scaffold` all pass.

## Correctness fix (from the open "execution/medium" finding)

**`store::deliver_input` could silently revive an ended agent.** Its UPDATE had no status
guard (unlike its siblings `complete_turn`/`mark_working`/`mark_blocked`), so a concurrent
`kill`/reconcile/discovery that had just moved a row to `ended`/`lost` could be undone
(ended→working) by a racing spawn/send delivery, re-opening a turn and emitting a bogus
`status_changed(working)` on an agent with a closed pane. Fixes:

- `deliver_input` now guards `WHERE uuid=?1 AND status NOT IN ('ended','lost')` and returns
  `Option<(input_seq, event_seq)>` — `None` means the row was terminal and delivery was
  refused. `open_external_turn` propagates the `Option`.
- Added `store::settle_primed_idle(uuid, at) -> Option<i64>`, guarded on `status='starting'`,
  for the no-prompt spawn branch (replacing an unguarded `transition_status(idle)` +
  `set_idle_since` that had the same revive-an-ended-row defect).
- Callers updated: the spawn pipeline (`engine.rs`) bails via `bail_if_cancelled` when
  delivery/settle returns `None` (the kill already ended it); `handle_agent_send` returns
  `not_found_target` ("ended concurrently"); the completion monitor skips a `None` external
  turn.

## Simplicity / anti-slop cleanups (from the open low-severity findings)

- **`AgentSessionRefKind::as_str()`** added in `driver/protocol.rs`; the three copy-pasted
  `Id=>"id"/Path=>"path"` matches in `engine.rs`, `completion.rs`, `discovery.rs` now call it.
- **`server::params::str_array(&Value)`** hoisted (pub(crate)); `str_array_param` delegates to
  it and `cli.rs` uses it, dropping the duplicate `cli::json_str_array`.
- **Typed `store::status_counts() -> StatusCounts`** replaces the four inline raw-SQL COUNTs in
  `server::counts()`; the now-unused public `Store::conn()` accessor was removed (restoring the
  single-writer/typed-DAL convention — the store's own `self.conn` field access in tests is
  unaffected).
- **`engine::resolve_targets_where(scope, targets, allow_patterns, keep)`** extracts the shared
  walk behind `resolve_targets` / `resolve_ended_targets` (~30 lines of duplicated branch logic
  removed; behavior preserved).

Finding "gate `NewAgent::queued` under `#[cfg(test)]`" was **not applied**: it is used by the
external integration test `tests/agent_e2e.rs`, so it must stay public.

## CLI `--json --follow` envelope consistency (from the open "cli/low" finding)

`agent logs --json --follow` used to print one JSON envelope then switch to plain-text human
lines, violating §6 ("exactly one envelope object on stdout"; only `orcr top` is exempt). Now
follow-mode `--json` emits each poll batch as its own NDJSON `{ok:true,result:{entries:[…]}}`
envelope (`print_entries_ndjson`). `server logs --json --follow` similarly wraps each streamed
line as `{ok:true,result:{line:…}}` NDJSON (`stream_follow` gained a `json` flag). The whole
follow stream is now a sequence of valid envelopes.

## Test-hygiene: baseline-aware shared-`orcr`-session leak check (known-issues #1)

The e2e harnesses assert, on teardown, that neither the disposable `orcr_test_*` session nor
the literal `orcr` session leaked. Running the suite here surfaced that the literal-`orcr`
assertion **false-fails whenever a developer runs orcr concurrently on the same machine** —
orcr's *default* home is `~/.orcr` and its *default* herdr session is literally `orcr`, so any
`orcr` command run with a clean env (no `ORCR_HOME`/`ORCR_HERDR_SESSION`) legitimately creates
that session. This was confirmed by process ancestry: an interactive `orcr agent run --name
test` / `orcr agent attach test` / `orcr agent logs test --follow` running in the **user's**
herdr server (not descended from any cargo-test process, spawning a real `claude` agent — the
mock-only suite never does) created + kept the `orcr` session alive.

Root cause of the *original* known-issues #1 (a leaked test child bootstrapping `orcr`) is
already fixed: every e2e server and every loop-run child pins `ORCR_HERDR_SESSION` +
`ORCR_HOME`, so a leaked test child can only ever create its **disposable** session. That
disposable-session check (kept **unconditional**) is therefore the real per-test guarantee.
The literal-`orcr` check is now **baseline-aware**: `orcr_session_preexisted(bin)` records once
(per test binary) whether an `orcr` session already existed and skips the shared-session
assertion in that case — so the suite no longer false-fails on concurrent developer use, while
still catching a regression in a clean CI environment. Applied to all seven harnesses
(`e2e`, `agent_e2e`, `completion_e2e`, `gc_e2e`, `loop_e2e`, `top_e2e`, `recipe_e2e`).

**No session was ever leaked by the test suite:** after the full run, `herdr session list`
shows `0` `orcr_test_*` sessions. The `orcr` session present is the user's live instance and
was left untouched (safety rule).

## Tests added

- `store::tests::location_session_cancel_and_turns` extended: `deliver_input` on an `ended`
  row returns `None` and does not flip it back to `working`.
- `store::tests::settle_primed_idle_is_guarded_on_starting`: settles from `starting`, no-ops
  otherwise, never revives an `ended` row.
- `store::tests::status_counts_reports_managed_and_unmanaged`: typed counts exclude `ended`.

## Gate results (this pass)

- `cargo build` / `clippy --all-targets -- -D warnings` / `fmt --check`: clean.
- `cargo test` (unit + lightweight): 164 lib + all integration green.
- e2e (ORCR_E2E=1, live herdr + mock, --test-threads=1): e2e 5, agent 10, completion 8,
  gc 15, loop 9, top 5, recipe 8 — all green. conformance_live green.
- SDK: `npm test` 20/20; `codegen:check` (ORCR_BIN set) up-to-date; `npm run build` (tsc) via
  recipe_e2e green; `orcr scaffold` + `npx tsx workflow.ts` green (in recipe_e2e).
- Post-suite `herdr session list`: no `orcr_test_*` leak. The `orcr` session present is the
  user's concurrent live instance (external), not a suite leak.
