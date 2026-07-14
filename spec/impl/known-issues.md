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
