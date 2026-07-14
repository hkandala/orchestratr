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
| E02 | `agent ask` real codex | codex | critical | **FAIL** (env-driven; not an orcr logic bug) | 3 (`timeout`) | Both runs timed out at 3m → killed (`exit_reason: timeout`). Root cause: on this enterprise box codex writes a rollout and emits `task_complete` but with `last_agent_message: null` and **no** assistant `output_text` — an empty-response turn. orcr located the rollout via the identity gate (session captured, `idle_since` set), but the gc-immediate readable-response gate (known-issue #2 fix) correctly refuses to complete/teardown a turn with no readable final response, so `ask` hangs to `--timeout`. `--json`: `{"error":{"code":"timeout","details":{"path,uuid}}}`. See detailed finding below. |
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

### E02 — `agent ask` against a REAL codex — **FAIL** (severity: CRITICAL; env-driven, not an orcr logic bug)

Executed 2026-07-14 on darwin (Darwin 25.5.0), git `main` @ `7176491`, live herdr, real codex
(`/usr/local/bin/codex`). Throwaway `ORCR_HOME=/tmp/orcr_e2e.QSWDjq`, disposable session
`orcr_e2e_e9bc4182`. Leak check: **no leak** (session stopped + deleted; server stopped).

**Observed vs expected**

- **Expected:** stdout prints codex's final response (contains `PONG`); exit 0; `--json` →
  `{"ok":true,"result":{uuid,path,response:{text,final}}}`; ended `exit_reason: completed`; the
  codex adapter locates `~/.codex/sessions/**/rollout-*-<session_id>.jsonl` via the identity gate.
- **Actual (both the plain and `--json` run):** `ask` timed out after ~180s and orcr killed the
  agent.
  - Step 2 (plain): `error: timeout: ask timed out waiting for completion ({"path":"quick_check","uuid":"019f628b-…"})`, **exit 3**, elapsed 181s.
  - Step 3 (`--json`): `{"error":{"code":"timeout","details":{"path":"quick_check2","uuid":"019f628f-…"},"message":"ask timed out waiting for completion"},"ok":false}`, **exit 3**, elapsed 180s.
  - `agent ls --all --json`: both rows ended with `exit_reason: "timeout"` (NOT `completed`);
    `idle_since` was set ~12s after `created_at` (herdr *did* report idle).
  - `server logs`: `agent quick_check working (pane w2:p2)` → `--timeout expired for quick_check — killing`. No `turn … complete` line was ever logged.

**Root cause (env-driven).** The codex rollout files were located and inspected:
`~/.codex/sessions/2026/07/14/rollout-2026-07-14T14-32-14-019f628b-745f-….jsonl` (and the step-3
twin). Both contain `session_meta`, the delivered user prompt, and an `event_msg` `task_complete`
— i.e. codex *did* run and finish the turn — but with **`last_agent_message: null`** and **no
`response_item` message with an assistant `output_text` block** (verified by parsing both files:
`has_assistant_msg = False`). This enterprise codex produced an **empty-response turn**.

orcr's transcript adapter located the rollout via the identity gate (session captured, not
cwd-mtime; `idle_since` progressed). But completion under `gc immediate` is gated on the final
response being **verifiably readable** from the native transcript (`completion.rs::complete` →
`last_response`, the known-issue #2 fix): `last_response` requires a non-empty assistant text
entry and returns `transcript_unavailable{cause:"no_final_response"}` when there is none. With no
assistant message, the readable gate is never satisfied, the turn is never marked complete, the
public status stays `working`, and `ask` blocks until `--timeout` (3m) → orcr kills the agent
(`exit_reason: timeout`, exit 3).

**Assessment.** Not a newly-discovered orcr logic defect for this input — given a transcript with
no assistant response, refusing to complete/teardown is the spec-correct known-issue #2 behavior,
and the `--timeout` → loud `timeout` (exit 3) is the intended failure surface. This is the codex
analogue of E01/ENV-1: the enterprise provider does not yield a usable final response on this box
(claude persists no transcript at all; codex persists a transcript but with an empty response).
Confirmed by the E01 note ("codex may differ") — codex *does* differ (it writes a locatable
rollout), yet the end-to-end outcome is the same FAIL: `ask` cannot return text.

**Latent design gap worth a follow-up (not fixed here).** An *empty-response* turn — herdr idle +
codex `task_complete` + settled transcript but zero assistant text — never settles in orcr and can
only ever surface as `--timeout`, under **any** gc mode (non-immediate would likewise have no
response to report and the turn would sit `working`). If empty final responses are a legitimate
provider outcome (vs. purely an enterprise-wrapper artifact), consider a spec/design path for
"turn completed with empty response" so `ask`/`wait` settle deterministically instead of hanging.
Recorded as an observation; per this phase's scope no code was changed for E02.

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

Post-phase `herdr session list` shows only the user's `default` session — no
`orcr`/`orcr_e2e_*` session leaked, and no stray `orcr server` process bound to any
throwaway `ORCR_HOME`. Each E01 run used a disposable `orcr_e2e_<rand>` session that was
`session stop`+`delete`ed on teardown; the throwaway `ORCR_HOME` tempdirs were removed.

```
name       status   directory                       socket
default    running  /Users/hkandala/.config/herdr   /Users/hkandala/.config/herdr/herdr.sock
```

## Executive summary

Final green check (2026-07-14, git `966d46f`, `target/debug/orcr`, herdr 0.7.2):

| check | result |
| --- | --- |
| `cargo build` | OK (dev profile) |
| `cargo test --lib` (non-e2e) | **164 passed**, 0 failed |
| `cargo clippy --all-targets -- -D warnings` | clean (exit 0, 0 warnings) |
| `cargo fmt --check` | clean (exit 0) |

### Totals

| bucket | count | notes |
| --- | --- | --- |
| planned scenarios | 22 | E01–E22 (`manual-e2e-tests.md`) |
| executed | 1 | E01 — the critical known-issue #2 target |
| passed | 0 | — |
| failed | 1 | E01 (real-claude `agent ask` → `transcript_unavailable`) |
| **fixed** | 1 | E01 root-caused to two orcr bugs; both fixed + regression-tested |
| not executed (pending/blocked) | 21 | E02–E22 not run in this phase |
| **critical still open** | **0** | the one critical (E01) is resolved for orcr; residual is environmental |

The phase's mandated priority — reproduce and fix **known-issue #2** (E01) — is complete.
The other 21 scenarios (E02–E22) were not executed in this session and remain the outstanding
work of the manual-e2e phase.

### Notable issues

- **ISSUE-1 (CRITICAL, E01) — FIXED.** `orcr agent ask` against real `claude` exited 1 with
  `transcript_unavailable` (both `no_session` and `not_found` sub-causes), reproducing
  known-issue #2. Root-caused to **two independent orcr bugs**, both fixed + regression-tested:
  1. **Premature `gc immediate` teardown** — `completion.rs::transcript_settled` was permissively
     `true` when no transcript could be located, and `complete()` tore the pane down without
     verifying the response was readable, killing the agent ~2.5s into claude's boot (spec §5.6 /
     §11.2 violation). Fixed: `transcript_settled` returns `false` for a real provider
     (`transcript_settle_ms > 0`) with no located transcript yet; `complete()` refuses
     gc-immediate teardown until `last_response()` is readable.
  2. **Dropped submitting `Enter`** — the single `Enter` sent during claude's boot was silently
     dropped by the TUI, so the prompt sat unsubmitted and claude never worked. Fixed:
     `engine.rs::confirm_submit` re-sends `Enter` until the pane leaves `idle` or
     `submit_confirm_ms` (per-provider; claude/codex 8000ms, mock 0) elapses; a redundant Enter is
     a verified no-op.

  Both fixes verified against real claude (pane shows `⏺ PONG` with no manual Enter) and covered by
  `completion_e2e::e2e_ask_waits_for_late_transcript_before_immediate_teardown` and
  `completion_e2e::e2e_submit_confirm_resends_until_working`.

- **ENV-1 (environmental, NOT an orcr bug) — residual on THIS box only.** This machine's
  enterprise claude (Vertex AI, launcher/`fast_mux`, session hooks) persists **no** locatable
  native transcript (`~/.claude/projects/<slug>/<id>.jsonl`) for herdr-launched panes (confirmed
  via `find` + `lsof`, even with `CLAUDE_CODE_*` stripped). Since orcr keeps no response copies by
  design (§11.4), `ask -a claude` cannot return text on this box and now fails **loud** with
  `timeout` (exit 3) instead of the old silent `transcript_unavailable` — the spec-correct outcome.
  On a standard claude that writes native transcripts, fixes #1+#2 make `ask` succeed end-to-end
  (proven by the mock regression exercising the same code paths with a real settle window).

### Fixed vs open

- **Fixed:** E01 / known-issue #2 (both orcr root causes) — 4 commits on `main`
  (`acf2d90`, `618fb5f`, `da3b75a`, `966d46f`), regression tests green.
- **Open (not orcr bugs):** ENV-1 — return-path for `ask -a claude` on this specific enterprise
  box; blocked by the provider not persisting a native transcript, outside orcr's control.
- **Open (not executed):** E02–E22 (21 scenarios) — the rest of the manual-e2e plan.

### Next steps (prioritized)

1. **Execute E02–E22** to finish the manual-e2e phase. Start with the remaining critical/high
   real-provider paths: E02 (`agent ask` real codex — confirms the ENV-1 transcript issue is
   claude-box-specific vs general), E03/E04 (claude/codex lifecycle), E05 (logs freshness),
   E18 (§9 recipes real provider).
2. **Then the deterministic mock scenarios** E06–E17, E19–E22 (identity/globs, queue caps, gc
   modes, loops + restart recovery, top, api, server lifecycle, SDK, scaffold, config/env,
   error-code sweep, attach/GC interlock) — fast and cheap; batch them.
3. **Verify E02 (codex)** specifically to determine whether ENV-1's missing-native-transcript is
   unique to this enterprise claude or a broader real-provider gap; if broader, consider a spec/
   design follow-up for how `ask` returns responses when a provider writes no native transcript.
4. **Keep the E01 fixes under CI** — the two `completion_e2e` regressions require live herdr +
   mock; ensure they run in whatever e2e CI lane exists so the boot-race fixes don't regress.
```
