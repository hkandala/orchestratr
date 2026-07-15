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

## Fixed after manual-e2e (cont.)

### E02 — intermittent submit-confirm flake  ·  medium  ·  REAL BUG  ·  **FIXED**
Plain `agent ask -a codex` passed (PONG, ~22s) but one `--json` ask timed out at 8s
(`submit-confirm … still idle after 8000ms`) and passed on retry; E01 timed out both times in
one run. Root weakness: the old `confirm_submit` relied on "pane left idle" and only re-sent a
bare `Enter` — it couldn't recover a dropped `send_text` (empty input box), and could fire before
the TUI was ready to accept input at all.
- **Fix (hardened submit-confirm, §5.6):** `engine.rs::deliver_prompt` now, for a managed
  real-provider agent, (1) **waits for readiness** (`await_input_ready`, bounded by
  `submit_ready_ms`) — herdr reports a real `agent_status` or the pane's rendered content settles
  (via new driver `pane_read`) — before the first send; (2) **verifies submission** by reading the
  pane (`pane_shows_prompt`): if a turn isn't underway and the input box is empty (the `send_text`
  was dropped) it re-sends the **FULL** delivery, else it nudges with `Enter`; (3) uses a longer
  **adaptive** window (`submit_confirm_ms` 8000→20000 default) across up to `submit_attempts` (6)
  full re-deliveries. Applied to **both** the first-prompt delivery (`pipeline_inner`) and the
  `send` path. Per-provider tuning in `integration.rs` + config overrides; the mock is off
  (`submit_confirm_ms 0`).
- **Regression:** `tests/completion_e2e.rs::e2e_submit_confirm_redelivers_dropped_prompt` (mock,
  `ORCR_MOCK_DROP_FIRST_SENDS=1` + tty-echo-off so `pane.read` faithfully shows a dropped vs
  accepted send) asserts the prompt is re-delivered and the turn completes with exactly one
  response. Fails with a bare-Enter-only loop (`submit_attempts:0`), passes after. Green:
  build/clippy(-D warnings)/fmt/164 unit + `conformance_live` + `completion_e2e` (11) + `agent_e2e`
  (10) + `recipe_e2e` (8).
- **Real-provider validation (disposable home + `orcr_sc_*` session, teardown clean, no leaks):**
  - **codex — FULL end-to-end works.** `agent ask -a codex` (plain **and** `--json`) returns `PONG`
    in ~12s, exit 0, 3/3 runs (the intermittent `--json` 8s timeout is gone with the adaptive
    window).
  - **claude — submission is now FIXED.** A clean baseline (submit-confirm disabled) proved the bug
    is a **dropped `Enter`** (not a dropped `send_text`): `send_text` lands the prompt in claude's
    input box, but the single `Enter` is silently dropped, so the prompt **sits unsubmitted in the
    box indefinitely** (herdr `agent_status` stays `idle`). With the fix the prompt submits
    **exactly once** (no duplicate copies, no spurious re-delivery) and claude starts processing.
  - **Newly-found, SEPARATE blocker for claude on this box (NOT submit, out of scope):** herdr's
    claude integration (v7) does **not** report `working` for the enterprise Avocado/MetaCode-wrapped
    claude — `agent_status` stays `idle` even while claude is visibly `Schlepping…`. So orcr's
    completion monitor can't detect the turn, `ask`/`wait` still time out, and `logs` hits
    `transcript_unavailable` (the wrapped claude's session id maps to no transcript file). This — not
    the submit flake — is the real remaining cause of the "BLOCKED" real-claude paths (E01/E03/E05/E18)
    **on this specific box**; codex proves the pipeline itself is correct. Fix direction (future,
    separate task): herdr claude-integration screen-detection for the Avocado wrapper + transcript
    location.
- **Final validation (PASS) — before/after flake rate.** `agent ask -p "reply with exactly: PONG"
  --timeout 3m`, 6× per provider, each on a fresh disposable `ORCR_HOME` + disposable
  `orcr_sc_<rand>` herdr session, torn down + leak-verified after each. **Before:** the submit-Enter
  flake was intermittent — E02 timed out at 8s on one `--json` ask (passed on retry) and E01 timed
  out both times in one run; a clean baseline showed claude's prompt sitting unsubmitted in the input
  box for 60s. **After:** **0 submit-Enter flakes across all 12 runs** — every run submitted the
  prompt and started the turn (all codex panes went `working`; a monitored claude run showed the box
  cleared and `⏺ PONG` produced in-pane). codex returned `PONG` 4/6 (exit 0, ~15–18s); the 2 non-PONG
  codex runs and all 6 claude runs failed only for the two *downstream, non-submit* reasons below
  (turn started, completion never detected). Recovery logic (re-deliver dropped `send_text`, re-send
  dropped `Enter`) fired as designed. Only the user's `default` herdr session remained afterward.

### Remaining downstream issues (NOT the submit-confirm flake, separate follow-ups)
1. **claude completion-detection blocker on this enterprise box** — herdr's claude integration returns
   empty `agent list` / no `agent_status: working` for the Avocado/MetaCode-wrapped claude, so orcr
   never detects the turn completing and `logs` can't resolve the transcript; the prompt submits and
   claude answers `PONG` in-pane (proven), but `ask`/`wait` time out. Needs a herdr-side integration
   fix or validation on a non-enterprise claude box. This is the sole remaining cause of the claude
   E01/E03/E05/E18 legs.
2. **codex intermittent completion timeout** — 2/6 codex runs went `working` (submit confirmed) but
   `turn 1 complete` was never observed within 3m; a downstream completion-detection / codex-slowness
   intermittency worth a separate look, unrelated to submit.

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
- **UPDATE (submit-confirm hardening / E02 above):** the submit-Enter flake is now fixed and
  proven — claude's prompt submits reliably (dropped-`Enter` recovery via bounded bare-Enter
  re-send). BUT real-claude `ask`/`wait` on THIS enterprise box remains blocked by a **separate**
  issue found during that work: herdr's claude integration does not report `agent_status: working`
  for the Avocado/MetaCode-wrapped claude, so completion is never detected and `logs` can't resolve
  the wrapped session's transcript. This is a **herdr-integration / transcript-location** problem,
  not an orcr submit or completion-logic defect (codex runs fully end-to-end with the same orcr
  pipeline — `agent ask -a codex` → `PONG`). See the E02 entry's real-provider validation for the
  evidence. Independent validation on a non-enterprise claude box (where herdr's claude integration
  reports state normally) is the way to confirm the claude leg end-to-end.

## Deferred infra / roadmap (parked by owner)

Not bugs — release/distribution polish parked for later. The repo is now **public**, so the
`curl … | sh` install one-liner and release binaries are downloadable without auth.

- **Host the install script at `orchestratr.dev/install.sh`** — `install.sh` is in the repo; it
  still needs to be served at that URL. Recommended: a **Cloudflare Worker** on the route (ready
  snippet in `docs/RELEASING.md`), or Cloudflare Pages, or a redirect rule to the raw GitHub URL.
- **Automate releases further** — today: `scripts/release.sh` (one-command bump+tag+push).
  Consider **release-please** (auto version + `CHANGELOG.md` from Conventional Commits — the repo
  already uses `feat:`/`fix:`/`chore:` prefixes) or **cargo-dist** (generates the release workflow
  *and* the installer). Either would replace the hand-rolled `release.yml` + `install.sh`.
- **Publish to registries** — add `CARGO_REGISTRY_TOKEN` (crates.io) + `NPM_TOKEN` (npm) repo
  secrets to enable the gated publish jobs; claim the `orchestratr` crate name + `@orchestratr`
  npm scope first (see `docs/RELEASING.md`).
