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

## Open — real orcr issues

### E07 — pane leak on the kill-during-promotion race  ·  medium  ·  REAL BUG
`handle_agent_kill` classifies each matched agent from the snapshot taken when the kill
begins. If an agent transitions `queued → starting` (and its herdr pane is spawned by
the queue worker's `promote_and_dispatch`) in the window between that snapshot and the
kill's per-agent action, the kill ends the row via the pane-less "queued → canceled"
path and never closes the just-spawned pane → a live zombie pane (orcr row `ended`,
herdr pane alive; row even carries a stale `pane_id`). Only session teardown reaps it.
- Repro: cap the provider low, queue many, `agent kill "<scope>/**" -y` while a slot is
  freeing (promotion in flight).
- Fix direction: in the kill path, re-read each target's row **under the write lock**
  and, if it now has a pane (promoted since the snapshot), close that pane (treat as a
  running kill) instead of a pane-less dequeue; and/or a promotion↔kill admission
  barrier so a scope being killed doesn't dispatch new panes. Single-writer server, so
  this is a contained CAS fix.
- Disposition: **fixing now** (real resource leak, contained fix).

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
