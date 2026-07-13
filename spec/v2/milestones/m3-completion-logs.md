# M3 · Completion & logs

The hard-won part: knowing when a turn is actually done, and reading what the agent
said. M3 ships the turns machinery, `wait`, the claude/codex transcript adapters,
`logs`, and `gc immediate`.

## Scope

### Turn completion (spec §5.6)
- `turns` table: one row per delivered input (`input_seq` incremented before
  delivery); `delivered_at`, `working_seen_at`, `completed_at`, `blocked_kind`,
  `transcript_cursor`, `source (orcr|external)`.
- Completion rule: working observed after this input's delivery began
  (`fast_turn_grace_ms` for fast turns) → stable idle (`idle_stable_ms`) → transcript
  settled (`transcript_settle_ms`: no new transcript writes). Only then does public
  status flip `working → idle`.
- A first idle without input-scoped working is never completion; an old idle can
  never satisfy a newer send.
- **External turns**: a `working` transition with no pending orcr delivery → synthetic
  turn (`source: external`, `input_seq` bumped). User interrupts settle at the next
  stable idle and record what the transcript shows.
- `blocked` turn-scoped (categories best-effort: question|limit|login|unknown),
  cleared by `send`.
- Restart safety: turn progress persisted; missing fields after restart → wait for a
  fresh transition, never trust a stale idle.

### wait (spec §6.1)
- Targets: patterns + uuids (§5.1 — relative to the caller's scope, `/` absolute,
  `*` wildcard); membership snapshotted at invocation; active agents only.
- Uniform results: one `<path> <reason>` line per agent (single-token reasons),
  identical for single-agent and pattern waits; idempotent (already-settled
  targets report immediately); JSON per target `{uuid, path, status, ok, reason,
  exit_reason?, next}` + `all_ok`, `timed_out`, `decision_seq` (§6.1 exactly).
- Implementation: snapshot-then-subscribe on the event stream — provably no missed
  transitions.

### Transcript adapters: claude + codex (spec §11.4)
- Locate + parse native session files → common shape (ordered messages, roles, tool
  calls, token counts where available).
- Identity gate: select by `agent_session` + `created_at`; multiple candidates →
  `transcript_unavailable` with `details.cause: "ambiguous"` + candidates (never a
  silent pick).
- Freshness gate: final response reported only once the transcript has advanced past
  the observed completion (`transcript_freshness_timeout_ms`) → else
  `transcript_unavailable`.
- On completion: the final response is captured to `<data dir>/response.md`;
  `response_captured_at`, `transcript_locator`, `transcript_cursor` recorded in the
  store.

### ask (spec §6.1)
- `orcr agent ask` — the CLI one-liner: run (gc immediate) → settle wait →
  last-response on stdout; exactly the documented sugar, no extra semantics.
- Naming enforcement tests: ask without --name/--path → `invalid_request`; both
  together → `invalid_request`; --name / relative --path / absolute /path resolve
  identically to run.

### logs (spec §6.1)
- `agent logs <path|uuid>`: structured entries; `--tail <n>`; `--follow` (subscription
  under the hood); `--last-response` (fails loudly: `transcript_unavailable` /
  `integration_missing`); history via uuid.
- Works on any agent with a reported `agent_session` + an orcr integration.

### gc immediate (spec §5.4, §11.2)
- Two-phase: stable idle → transcript settled → response captured → graceful kill +
  pane closed; ends `ended (completed)`.

### Also owned by M3
- Integration tuning defaults + config overrides (`integrations.<provider>.*`,
  spec §14); the structured `next` hint enum (`logs_last_response|attach|
  logs_history|none`); event kinds `turn_completed` / `response_captured` with
  snapshot-updating payloads; wait `decision_seq` consistency (all targets settled
  at one event sequence — un-settled targets re-waited).

### SDK groundwork
- Internal typed client grows `wait`/`logs`/`lastResponse` (used by tests); public
  SDK packaging remains M7.

## Acceptance

- send → wait → `--last-response` round-trips on claude and codex (real providers),
  including two consecutive sends (the second wait never satisfied by the first
  idle).
- Fault: restart the server mid-turn → wait re-arms conservatively and completes.
- External-input test: type into the pane via herdr directly → synthetic turn
  recorded; a subsequent orcr `wait` settles correctly on the external turn.
- `gc immediate` race: response is always captured before the pane dies (fault
  injection between capture and kill).
- Transcript ambiguity/freshness gates hit their error paths in fixtures; no silent
  wrong-transcript reads.
- Mock-provider matrix: fast turn (< grace), slow tool-heavy turn (idle gaps shorter
  than settle window), blocked mid-turn.

## Out of scope

GC auto parking (M4), attach (M4), loops (M5), pi/opencode adapters (future).
