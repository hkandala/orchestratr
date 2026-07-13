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
- Targets: subtree selectors + uuids; membership snapshotted at invocation; active
  agents only.
- `--status idle|working|blocked|ended` (default idle) with the full outcome matrix:
  parked counts as turn-complete; `ended` = any terminal outcome; exit codes
  0/4/5/3/6.
- Implementation: snapshot-then-subscribe on the event stream — provably no missed
  transitions.

### Transcript adapters: claude + codex (spec §11.4)
- Locate + parse native session files → common shape (ordered messages, roles, tool
  calls, token counts where available).
- Identity gate: select by `agent_session` + `created_at`; multiple candidates →
  `transcript_ambiguous` (never a silent pick).
- Freshness gate: final response reported only once the transcript has advanced past
  the observed completion (`transcript_freshness_timeout_ms`) → else
  `transcript_unavailable`.
- On completion: `final_response`, `response_captured_at`, `transcript_locator`,
  `transcript_cursor` captured into the store.

### logs (spec §6.1)
- `agent logs <fqn|uuid>`: structured entries; `--tail <n>`; `--follow` (subscription
  under the hood); `--last-response` (fails loudly: `transcript_unavailable` /
  `integration_missing`); history via uuid.
- Works on any agent with a reported `agent_session` + an orcr integration.

### gc immediate (spec §5.4, §11.2)
- Two-phase: stable idle → transcript settled → response captured → graceful kill +
  pane closed; ends `ended (completed)`.

### SDK groundwork
- Internal typed client grows `wait`/`logs`/`lastResponse` (used by tests); public
  SDK packaging remains M7.

## Acceptance

- send → wait → `--last-response` round-trips on claude and codex (real providers),
  including two consecutive sends (the second wait never satisfied by the first
  idle).
- Fault: restart the server mid-turn → wait re-arms conservatively and completes.
- External-input test: type into the pane via herdr directly → synthetic turn
  recorded; a subsequent orcr `wait --status idle` behaves correctly.
- `gc immediate` race: response is always captured before the pane dies (fault
  injection between capture and kill).
- Transcript ambiguity/freshness gates hit their error paths in fixtures; no silent
  wrong-transcript reads.
- Mock-provider matrix: fast turn (< grace), slow tool-heavy turn (idle gaps shorter
  than settle window), blocked mid-turn.

## Out of scope

GC auto parking (M4), attach (M4), loops (M5), pi/opencode adapters (future).
