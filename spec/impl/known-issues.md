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
