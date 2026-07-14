# Open issues (post manual-e2e)

Status as of the final manual-e2e phase. The build is complete and green (M0–M7 +
comprehensive review + spec-completeness audit all PASS; `cargo build`/`clippy -D
warnings`/`fmt` clean; 164 lib tests + full e2e suites green). Manual e2e ran E01–E22
(15 PASS · 4 PARTIAL · 3 BLOCKED · 0 FAIL). This file tracks the remaining findings and
their disposition.

## Fixed during manual-e2e

- **E01 — `agent ask` real-provider failure (was known-issue #2).** Two real orcr bugs,
  both fixed + regression-tested:
  1. Premature gc-immediate teardown — `completion.rs` tore the pane down before the
     transcript was readable; now gated on a settled/readable transcript (§5.6/§11.2).
  2. Dropped submit-Enter on real-provider boot — the single Enter was silently dropped
     mid-boot; `engine.rs::confirm_submit` now re-sends Enter until the pane leaves idle
     or `submit_confirm_ms` elapses (per-provider tuning).

## Fixed after manual-e2e

### E07 — pane leak on the kill-during-promotion race  ·  medium  ·  REAL BUG  ·  **FIXED**
`handle_agent_kill` classified each matched agent from the snapshot taken when the kill
begins. If an agent transitioned `queued → starting` (and its herdr pane was spawned by
the queue worker's `promote_and_dispatch`) in the window between that snapshot and the
kill's per-agent action, the kill ended the row via the pane-less "queued → canceled"
path and never closed the just-spawned pane → a live zombie pane (orcr row `ended`,
herdr pane alive; row even carried a stale `pane_id`). Only session teardown reaped it.
- Repro: cap the provider low, queue many, `agent kill "<scope>/**" -y` while a slot is
  freeing (promotion in flight).
- **Fix (commits `f856951` test, `c811a0b` fix):** in the kill's per-agent loop, re-read
  each row **under the store lock** at action time. A still-`queued` row is dequeued
  atomically via `store.end_if_status(queued → canceled)` — promotion moves `queued →
  starting` under the same lock *before* it ever spawns a pane, so a successful guarded
  end proves no pane exists (the pane-less cancel is safe). If it lost the race to
  promotion (or the fresh read already shows `starting`/running), it routes through the
  shared `kill_live_agent` helper, which sets the cancel interlock and closes the pane;
  `pipeline_inner`'s post-`agent.start` re-check + `bail_if_cancelled` close any pane
  created-but-not-yet-recorded. Best-effort `pane_close` tolerates the double-close.
- **Regression:** `tests/gc_e2e.rs::e2e_kill_during_promotion_no_pane_leak` (mock, global
  cap 1) forces the race deterministically via the `ORCR_TEST_KILL_ITER_DELAY_MS` hook and
  asserts no live pane survives for the just-promoted agent after the bulk kill. Fails
  before `c811a0b`, passes after. Green: build/clippy(-D warnings)/fmt/164 unit +
  `gc_e2e` (16) + `agent_e2e` (10).

## Open — deferred by owner (not being fixed now)

### E02 — intermittent codex submit-confirm flake  ·  medium  ·  DEFERRED
Plain `agent ask -a codex` passes (PONG, ~22s); one `--json` ask timed out at 8s
(`submit-confirm … still idle after 8000ms`) and passed clean on retry. Same root as the
E01 submit-Enter fix — the re-send window/attempts aren't fully robust on a slow boot.
- **This is also the true cause of the "BLOCKED" real-claude paths below.**
- Fix direction (when picked up): more re-send attempts, adaptive `submit_confirm_ms`,
  and/or pane-read acceptance verification (confirm the prompt left the input box).

### E21 — `wait` exit code for an agent's-own `--timeout`  ·  low  ·  DEFERRED (spec nit)
Impl returns exit **5** for a target that ended via its own `--timeout` when observed
through `wait` (self-consistent with §6's wait-reason table: `ended + timeout → exit 5`).
§13's error-code table maps the `timeout` code to exit **3**. This is an internal *spec*
inconsistency (the two sections disagree), not an impl defect. Resolve by clarifying the
spec: `wait`-aggregate outcomes follow §6's table (a dead target → 5); the `3`/`timeout`
code is for a direct command's own deadline. No code change expected.

## Not orcr bugs — environment-limited (BLOCKED)

### E01 / E03 / E05 (real-claude paths) + E18 (claude leg) — BLOCKED on this box
**Corrected diagnosis (the earlier "claude persists no transcript" conclusion was
WRONG — verified false):**
- `~/.claude/projects/` holds **526 JSONL transcripts**; claude persists transcripts
  normally for orcr/herdr-launched panes. Proven: a transcript from manual testing
  (`86f20020-…jsonl`) contains an assistant row with **`PONG`** — exactly what
  `agent logs --last-response` returns. The adapter locates it by `<session_id>.jsonl`
  across `~/.claude/projects/**` and parses it correctly.
- So `agent logs` / `agent ask` **do work** with real claude on this box (confirmed by
  the owner's earlier manual testing). The E01/E03/E05 "BLOCKED" results in the parallel
  run were the **intermittent submit-Enter flake (E02 root)**: when the boot-time Enter
  is dropped, the prompt never submits → the turn never runs → no fresh response →
  `ask`/`wait` time out. When the Enter lands, it returns `PONG` and everything works.
- **How to "fix" the blocked paths:** harden the submit-confirm (E02) so the prompt
  reliably submits on a slow real-provider boot. That single robustness fix unblocks
  E01/E03/E05/E18. (No transcript-discovery change is needed — the adapter is correct.)
- Independent validation on a non-enterprise claude box is still worthwhile, but the
  enterprise wrapper does **not** break transcript persistence (confirmed).
