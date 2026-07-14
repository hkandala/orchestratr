# Known issues carried into the comprehensive review + final verify phase

These are issues the orchestrator observed across milestones that MUST be root-caused
and fixed during the comprehensive spec-vs-impl review phase (task: dimension
"correctness/robustness/test-hygiene") and confirmed by the final consolidating verifier.

## 1. Recurring leaked `orcr` herdr session (test hygiene) — MUST FIX

Symptom: after M4 and M5 runs, a running herdr session literally named `orcr` (the
DEFAULT config `herdr.session`) was repeatedly left behind — empty (a bare default
workspace/shell, no managed agent), and `~/.orcr` never existed, so it is a leaked TEST
session, not production state. The orchestrator had to `herdr session stop orcr` +
`herdr session delete orcr` each time.

Root cause to find & fix: some e2e test and/or dev/manual code path bootstraps the owned
herdr session using the DEFAULT name `orcr` instead of a disposable `orcr_test_<rand>`
name, or a run/agent process outlives the test's drop-guard teardown so the session's
last pane never closes (herdr keeps the session while a pane/process lives).

Required fix:
- EVERY e2e test (and any dev/smoke helper) must force a disposable `herdr.session`
  (e.g. write `{"herdr":{"session":"orcr_test_<rand>"}}` into the throwaway ORCR_HOME
  config, or otherwise override it) so nothing ever bootstraps the literal `orcr`
  session.
- The e2e harness teardown must be robust: kill any spawned run process groups / mock
  agents BEFORE `herdr session stop/delete`, and guarantee teardown even on test
  failure/panic (drop guard).
- Add/keep an assertion that after the suite, `herdr session list` shows NO `orcr` or
  `orcr_test_*` session.
- The final consolidating verifier must run the full e2e suite and then verify no
  session leaked.

### Resolution (comprehensive-review round 1)
Root cause + belt-and-suspenders fixes applied:
- **Root cause closed:** a loop-run's orcr child that outlived teardown read `Config::default()`
  (session = literal `orcr`) once its throwaway `ORCR_HOME` was deleted, then auto-started a
  server bootstrapping the shared `orcr` session. Fixes: (a) `server/loops.rs` pins
  `ORCR_HERDR_SESSION = <owned session>` on every loop-run command's env; (b) `Config::load`
  honors an `ORCR_HERDR_SESSION` override (empty → file/default) so even a config-less orphan
  uses the disposable name.
- **Teardown hardened (pgid race):** the `loop_e2e`/`recipe_e2e` drop guards now loop —
  re-reading pgids and killing process groups — until no run is running/stopping/pending before
  stopping the server and deleting the tempdir, closing the `allocate_run → record_run_start`
  window where `pgid` is briefly NULL.
- **No-leak assertion added:** every e2e harness sets `ORCR_HERDR_SESSION` on the servers it
  spawns and, on drop (skipped mid-panic to avoid masking a real failure via double-panic),
  asserts `find_session(disposable) == None` **and** `find_session("orcr") == None`.
- **Confirmed:** full e2e suite (agent/completion/loop/gc/recipe/top) run green against live
  herdr 0.7.2 with the mock; `herdr session list` showed only the user's `default` session
  before and after — no `orcr`/`orcr_test_*` leak.

## 2. `agent ask` reported FAILING in manual testing (real provider) — MUST reproduce + fix in manual-e2e

The user, testing orcr by hand via `cargo run` against a real `claude` provider, observed
`orcr agent ask` FAILING. The automated e2e gate exercises `ask` only against the MOCK
provider (which writes a self-contained transcript), so a real-provider-specific failure
in the `ask` path would not have been caught.

`ask` = `run --gc immediate` → settle `wait` → `logs --last-response` (spec §6.1). Likely
suspects to investigate against a REAL claude/codex agent:
- transcript adapter fails to locate/parse the real claude/codex native transcript
  (identity gate by `agent_session` + `created_at`; freshness gate) → `transcript_unavailable`;
- gc-immediate tears down the pane before the final response is captured/readable
  (the §11.2 "response verified readable before kill" ordering);
- the settle/completion detection for a real provider's first turn (fast-turn grace,
  transcript settle) mis-fires so `wait` never settles or settles too early;
- CLI exit/`--json` path for `ask` surfacing the error unhelpfully.

Manual-e2e MUST: reproduce `orcr agent ask --name <n> -a claude -p "..."` end-to-end
against a REAL claude agent (and codex), capture the exact failure (stderr, exit code,
`--json` error code + details), root-cause it, FIX it, and add a regression test
(real-provider smoke where feasible; otherwise a mock-based test that covers the same
code path the real failure exercised). Record findings in manual-e2e-results.md.

### Resolution (manual-e2e fixer round)

Reproduced against the real `claude` provider; two independent **orcr** root causes fixed, plus
one **environment** limitation documented.

- **Root cause A — premature `gc immediate` teardown → `transcript_unavailable`.**
  `server/completion.rs::transcript_settled` was permissively `true` when the transcript could not
  be located, and `complete()` tore a `gc immediate` agent down without verifying the response was
  readable. During claude's boot (herdr `idle`, no transcript yet) the fast-turn-grace + stable-idle
  path completed and killed the pane in ~2.5s — before claude captured a session (`no_session`) or
  wrote a transcript (`not_found`). **Fix:** `transcript_settled` returns `false` for a real
  provider (`transcript_settle_ms > 0`) whose transcript isn't located yet, and `complete()`
  refuses gc-immediate teardown until the final response is **verified readable** (§5.6, §11.2).
- **Root cause B — the submitting `Enter` was dropped during claude's boot.** `herdr pane read`
  proved the prompt sat unsubmitted in claude's input box; a single `Enter` sent ~1s after
  `send_text` (mid-boot) was silently dropped, so claude never worked and never wrote a transcript.
  **Fix:** `server/engine.rs::confirm_submit` re-sends `Enter` after the two-call delivery until the
  pane leaves `idle` or the new per-provider `submit_confirm_ms` window elapses (claude/codex 8s,
  mock 0). A redundant Enter on a submitted/empty box is a verified no-op. Verified against real
  claude: with the fix and no manual Enter, the pane shows `⏺ PONG`.
- **Environment limitation (not orcr).** On the test box, the enterprise claude persists **no**
  locatable native transcript (`~/.claude/projects/<slug>/<id>.jsonl`) for herdr-launched panes,
  so `ask -a claude` still can't return the text there — it now fails loud (`timeout`, exit 3)
  instead of the old silent `transcript_unavailable`. On a standard claude that writes native
  transcripts, fixes A+B make `ask` succeed end-to-end.
- **Regression tests** (`tests/completion_e2e.rs`, mock + live herdr):
  `e2e_ask_waits_for_late_transcript_before_immediate_teardown` (A) and
  `e2e_submit_confirm_resends_until_working` (B). Full mock e2e suites stay green.
