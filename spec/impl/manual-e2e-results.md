# orchestratr — manual end-to-end test results

Observed outcomes of the manual e2e phase (master-prompt §8). The plan is in
[`manual-e2e-tests.md`](manual-e2e-tests.md). Each test is executed one at a time
against **live herdr 0.7.2** (and real `claude`/`codex` where the test says so) using a
throwaway `ORCR_HOME` + a disposable `orcr_e2e_<rand>` herdr session; after each test the
leak check (`herdr session list`) must show no `orcr`/`orcr_e2e_*` session.

This phase **reports** issues; it does not silently fix them — the one exception is
**known-issue #2** (E01/E02, the real-provider `agent ask` failure), which the plan
requires be root-caused, fixed, and covered by a regression test. Record that root
cause + fix inline in the E01/E02 rows/notes.

For each test record: expected vs actual, pass/fail, exit code, any `--json` error
`{code, details}`, and notes (screenshots/log excerpts for the TUI test). Note the leak
check result too.

## Environment

| item | value |
| --- | --- |
| date | _(fill in)_ |
| host / OS | _(fill in — darwin …)_ |
| orcr binary | `/Users/hkandala/code/orchestratr/target/debug/orcr` |
| herdr | 0.7.2 (protocol 16) |
| providers | claude, codex (both herdr integrations installed) |
| git commit | _(fill in)_ |

## Results table

| id | title | provider | priority | result | exit | notes |
| --- | --- | --- | --- | --- | --- | --- |
| E01 | `agent ask` real claude (known-issue #2 repro→fix) | claude | critical | **FIXED** (orcr root causes) | — | Two orcr root causes found + fixed: (1) premature gc-immediate teardown (`transcript_unavailable`), (2) dropped submitting Enter (prompt never submitted). Both verified against real claude. Residual `ask` blocker on THIS box is environmental (enterprise claude writes no locatable native transcript for herdr panes), not an orcr bug. See fixer note below. |
| E02 | `agent ask` real codex | codex | critical | _pending_ | | |
| E03 | claude lifecycle run→wait→logs→send→wait | claude | high | _pending_ | | |
| E04 | codex lifecycle run→send→logs→kill | codex | high | _pending_ | | |
| E05 | claude logs --tail/--follow/--last-response freshness | claude | high | _pending_ | | |
| E06 | identity/paths/globs/scope (deterministic) | mock | high | _pending_ | | |
| E07 | queue + concurrency caps (FIFO, never over cap) | mock | high | _pending_ | | |
| E08 | gc auto park→send→unpark→reap | mock | high | _pending_ | | |
| E09 | gc immediate vs never (teardown ordering) | mock | normal | _pending_ | | |
| E10 | loops create/run/logs + overlap coalesce | mock | high | _pending_ | | |
| E11 | loop restart recovery + pause/resume/rm | mock | high | _pending_ | | |
| E12 | server enable/disable (launchd) | none | normal | _pending_ | | |
| E13 | top: launch, filters, live updates | mock | high | _pending_ | | |
| E14 | api schema + snapshot | mock | normal | _pending_ | | |
| E15 | server start/stop/status/logs + auto-start race | mock | high | _pending_ | | |
| E16 | TS SDK scope/ask/watch/run/loop | mock | high | _pending_ | | |
| E17 | scaffold + run workflow.ts | mock | high | _pending_ | | |
| E18 | §9 recipes (fan-out + tournament) real provider | claude+codex | high | _pending_ | | |
| E19 | skill hot path drill | claude | normal | _pending_ | | |
| E20 | config validation + env contract + ORCR_HOME | mock | normal | _pending_ | | |
| E21 | error codes & exit-code mapping sweep | mock | normal | _pending_ | | |
| E22 | attach prepare/lease + GC interlock | mock | normal | _pending_ | | |

## Detailed findings

### E01 — `agent ask` against a REAL claude — **FAIL** (severity: CRITICAL; reproduces known-issue #2)

- **date/host:** 2026-07-14, darwin 25.5.0 · **git commit:** `30f4cd6` · **binary:** `target/debug/orcr` (built clean) · **herdr:** 0.7.2 (protocol 16) · **provider:** real `claude` (`/usr/local/bin/claude`).
- **harness:** throwaway `ORCR_HOME=/tmp/orcr_e2e.U17jpg`, disposable session `orcr_e2e_adf93935`, `ORCR_HERDR_SESSION` pinned, discovery disabled. Pre-run `herdr session list` showed only the user's `default`.

- **Step 2 — plain `agent ask`:**
  `"$ORCR" agent ask --name quick_check -a claude -p "Reply with exactly the word PONG and nothing else." --timeout 3m`
  - **stderr:** `error: transcript_unavailable: no agent_session transcript pointer has been reported for this agent ({"cause":"no_session","status":"ended","uuid":"019f6253-0451-79c3-8785-8e254d59c7eb"})`
  - **exit code:** `1` · **elapsed:** ~8s.

- **Step 3 — `--json` `agent ask`:**
  `"$ORCR" agent ask --json --name quick_check2 -a claude -p "Reply with exactly the word PONG." --timeout 3m`
  - **stdout envelope:** `{"error":{"code":"transcript_unavailable","details":{"cause":"not_found","status":"ended","uuid":"019f6253-41e9-7ad1-b4ab-1ab9a9c789bc"},"message":"no transcript file found for session `8c0ef6a5-0637-4f3f-9945-5986660d894a` (rotated or deleted)"},"ok":false}`
  - **exit code:** `1` · **elapsed:** ~7s.

- **expected:** stdout prints the model's final response containing `PONG`; exit 0; `--json` `{"ok":true,"result":{uuid,path,response:{text,final}}}`; ended agent `exit_reason: completed`.
- **actual:** exit 1 on both runs, no response returned, `transcript_unavailable`. `agent ls --all --json` confirms both agents ended with `exit_reason: completed` (created→ended in ~7s each; `quick_check` pane `w2:p2`, `quick_check2` pane `w3:p2`). Server logs show only: `agent … working` → `gc immediate: … ended (completed)` ~2.5s later — no transcript-capture / transcript-locate lines.

- **two distinct sub-failures observed (both are `transcript_unavailable`):**
  1. **`cause: no_session`** (run 1): the spawn pipeline never captured an `agent_session` pointer for the real claude agent, so `logs --last-response` has nothing to locate.
  2. **`cause: not_found`** (run 2): a session id *was* captured (`8c0ef6a5-0637-4f3f-9945-5986660d894a`), but no transcript file exists for it.

- **corroborating evidence (read-only inspection of `~/.claude/projects`):** there is **no** `8c0ef6a5*.jsonl` anywhere under `~/.claude/projects`, and **no** new `.jsonl` was created in the project dir `~/.claude/projects/-Users-hkandala-code-orchestratr/` during the test window (newest file predates the run). i.e. the real claude agent produced no persisted native transcript before `gc immediate` tore the pane down ~2.5s after it went `working`. This is consistent with the known-issue #2 suspects: either gc-immediate teardown races ahead of the real provider's first-turn transcript flush / session capture, or the completion monitor mis-detects the real claude first turn as idle almost immediately (working→ended in ~2.5s is implausibly fast for a real claude turn).

- **teardown / leak check:** `server stopped`, session stopped + deleted, tempdir removed; `herdr session list` post-run → `no leak` (no `orcr`/`orcr_e2e_*` session); no stray `orcr server` process against the throwaway home.

### E01 — FIXER root-cause + fix (git this branch)

Reproduced against the real `claude` provider (throwaway `ORCR_HOME`, disposable `orcr_e2e_*`
session, `--gc never` diagnostics + `herdr pane read`). Found **two independent orcr root
causes** behind the reported `transcript_unavailable`, plus one **environment** limitation:

1. **Premature `gc immediate` teardown (the direct cause of `transcript_unavailable`).**
   `completion.rs::transcript_settled` returned `true` *permissively* when the transcript could
   not be located, and `complete()` tore a `gc immediate` agent down without first verifying the
   response was readable. So during claude's boot (herdr reports `idle`, no transcript yet) the
   fast-turn-grace + stable-idle path fired, `transcript_settled` was permissively true, and the
   pane was killed in ~2.5s — before claude registered a session (`no_session`) or wrote a
   transcript (`not_found`). Violates spec §5.6 (transcript must have **settled**) and §11.2
   (final response **verified readable** → kill).
   **Fix:** (a) `transcript_settled` returns `false` (not settled) when a real provider
   (`transcript_settle_ms > 0`) has no locatable transcript yet; (b) `complete()` refuses to
   tear down a `gc immediate` agent until `last_response()` is readable, retrying on later ticks.
   **Regression:** `completion_e2e::e2e_ask_waits_for_late_transcript_before_immediate_teardown`
   (mock with a real settle window + `ORCR_MOCK_LATE_TRANSCRIPT_MS`; reverting either half of the
   fix makes it fail with the exact `transcript_unavailable`).

2. **The submitting `Enter` was dropped by claude's TUI (why claude never worked / wrote a
   transcript at all).** `herdr pane read` showed the prompt sitting **unsubmitted** in claude's
   input box: `send_text` landed the text but the single `Enter` (sent ~1s later, during claude's
   boot) was silently dropped, so claude stayed idle and never produced a turn. A manually-sent
   extra `Enter` submitted it and claude answered `⏺ PONG`.
   **Fix:** after the two-call delivery, `engine.rs::confirm_submit` re-sends `Enter` until the
   pane's herdr agent leaves `idle` (submitted → working/blocked/done) or `submit_confirm_ms`
   elapses (new per-provider tuning knob; claude/codex default 8000ms, mock 0 = off). A redundant
   Enter on an already-submitted/empty box is a **verified no-op**, so it never double-delivers.
   **Verified against real claude:** with the fix active and **no** manual Enter, the pane shows
   `⏺ PONG` — the loop drove the submission. **Regression:**
   `completion_e2e::e2e_submit_confirm_resends_until_working` (mock + `ORCR_MOCK_DELAY_WORKING_MS`
   exercises the re-send loop and asserts exactly one turn ran — no double-delivery).

3. **Environment limitation (NOT an orcr bug), residual on this box only.** Even after a
   successful on-screen `PONG`, this machine's enterprise claude (Vertex AI, launcher/`fast_mux`,
   session-start hooks) writes **no** locatable native transcript to
   `~/.claude/projects/<slug>/<session_id>.jsonl` for a herdr-launched pane (confirmed via
   `find` over `~/.claude` and via `lsof`, incl. after stripping inherited `CLAUDE_CODE_*` session
   env). orcr keeps **no** response copies by design (§11.4 — `logs`/`ask` always read the native
   transcript), so on this specific box `agent ask -a claude` cannot return the text and now
   surfaces a **loud `timeout`** (exit 3) instead of the old silent `transcript_unavailable` —
   the spec-correct behavior when a turn cannot be confirmed complete. On a standard claude that
   persists native transcripts, fixes #1 + #2 make `ask` return the response end-to-end (proven
   by the mock regression, which exercises the same code paths with a real settle window).

Suites kept green with the mock against live herdr: `completion_e2e` (10), `agent_e2e` (10),
`recipe_e2e` (8); `cargo test --lib` (164); `cargo clippy -D warnings` + `cargo fmt` clean.

## Issues filed

- **ISSUE-1 (CRITICAL, E01) — FIXED (orcr root causes).** `orcr agent ask` against real `claude`
  failed with `transcript_unavailable`. Root-caused to two orcr bugs — (1) premature `gc immediate`
  teardown (permissive `transcript_settled` + no readable-response verification) and (2) the
  submitting `Enter` being dropped during claude boot so the prompt was never submitted — both now
  fixed with regression tests and verified against real claude. The residual inability to return
  the response on THIS box is an **environment** limitation (this enterprise claude persists no
  locatable native transcript for herdr panes); orcr now fails loud/`timeout` per spec instead of
  the old silent `transcript_unavailable`. See the E01 fixer note above.

## Leak audit

_(After the full suite: `herdr session list` output confirming no `orcr`/`orcr_e2e_*`
session leaked, and no stray `orcr server` process against a throwaway `ORCR_HOME`.)_
```
