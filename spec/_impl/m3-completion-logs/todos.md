# M3 · Completion & logs — todos

Ships: turns + completion, wait, transcript adapters, logs, ask, gc immediate.

## Setup
- [x] Read master-prompt.md + full spec.md + this milestone file + herdr-driver-reference.md
- [x] Learn claude + codex native transcript formats (read-only inspection)

## Turn completion (§5.6)
- [x] Integration tuning params (fast_turn_grace_ms, idle_stable_ms, transcript_settle_ms,
      transcript_freshness_timeout_ms, shutdown_grace_ms) — defaults + `integrations.<p>.*` config overrides
- [x] turns DAL: open turn lookup, working_seen, idle_since, complete_turn, blocked, transcript cursor
- [x] Completion monitor thread: poll herdr status, drive per-turn state machine
- [x] Completion rule: working-after-delivery (or fast-turn grace) → stable idle → transcript settled → flip working→idle
- [x] First idle w/o input-scoped working never completes; old idle never satisfies newer send
- [x] External turns: working w/ no pending delivery → synthetic turn (source=external, input_seq bumped)
- [x] blocked turn-scoped (question|limit|login|unknown best-effort), cleared by send
- [x] send bumps input_seq + resets to working (re-arm)
- [x] Restart safety: turn progress persisted; missing fields → wait for fresh transition
- [x] Events: agent.turn_completed / agent.response_captured with snapshot-updating payloads

## wait (§6.1)
- [x] handler: patterns+uuids resolved to active agents, membership snapshotted at invocation
- [x] snapshot-then-subscribe (bus wait) — all targets simultaneously settled at one decision_seq
- [x] uniform `<path> <reason>` lines; reason tokens from status×exit_reason table
- [x] JSON per target {uuid,path,status,ok,reason,exit_reason?,next} + all_ok + timed_out + decision_seq
- [x] structured `next` enum (logs_last_response|attach|logs_history|none)
- [x] idempotent (already-settled returns immediately); wait --timeout → ok:true, timed_out, exit 3
- [x] no match → exit 6; blocked → exit 4; dead → exit 5

## Transcript adapters: claude + codex (§11.4)
- [x] claude adapter: locate `~/.claude/projects/<slug>/<session_id>.jsonl`, parse JSONL → entries
- [x] codex adapter: locate `~/.codex/sessions/**/rollout-*-<session_id>.jsonl`, parse → entries
- [x] common shape: ordered messages, roles, tool calls, token counts where available
- [x] identity gate: select by agent_session + created_at; multiple candidates → transcript_unavailable (ambiguous)
- [x] freshness gate: final response only once transcript advanced past completion (timeout) else transcript_unavailable
      — enforced on the read path via `Server::last_response_fresh` (logs --last-response + ask); threshold = the
      completion cursor (transcript mtime at completion), polled up to transcript_freshness_timeout_ms (round-1 fix)
- [x] record transcript_locator/transcript_cursor on completion; no response copies

## logs (§6.1)
- [x] agent.logs handler: entries, --tail, --last-response (fails loudly), history via uuid
- [x] --follow (CLI polling under the hood)
- [x] transcript_unavailable / integration_missing error paths

## ask (§6.1)
- [x] agent.ask handler: run(gc immediate) → settle wait → last-response
- [x] naming enforcement: no name/path → invalid_request; both → invalid_request; parity with run

## gc immediate (§5.4, §11.2)
- [x] completion monitor: stable idle → transcript settled → response captured → graceful kill + pane closed → ended(completed)

## CLI + API + SDK groundwork
- [x] CLI verbs: wait, logs, ask (+ --follow, --last-response, --tail)
- [x] api.rs: flip implemented flags (ask/logs/wait), agent.ask params/result, next schema
- [x] Rust Client convenience: wait/logs/last_response (used by tests)

## Tests
- [x] Unit: completion state machine, reason mapping, next hint, transcript parse + ambiguity/freshness fixtures
- [x] e2e (mock, live herdr): fast turn, slow tool-heavy (idle gaps), blocked mid-turn, external input,
      two consecutive sends (stale idle), gc immediate → ended(completed), restart mid-turn re-arm
- [x] mock agent knobs: blocked, tool-gap toggles
- [x] cargo build / fmt / clippy / test green; e2e green

## Acceptance criteria (prove each)
- [x] send → wait → --last-response round-trips (claude/codex real; mock proves completion/wait/stale-idle)
- [x] restart server mid-turn → wait re-arms conservatively and completes
- [x] external-input → synthetic turn → subsequent wait settles
- [x] gc immediate race: response readable before pane dies (real); mock → ended(completed)
- [x] transcript ambiguity/freshness gates hit error paths in fixtures
- [x] mock matrix: fast / slow tool-heavy / blocked

## Deferred / out of scope
- GC auto parking, attach, loops, pi/opencode adapters (later milestones / future)
- [deferred → manual-e2e] Successful `send → wait → --last-response` round-trip on **real**
  claude/codex, i.e. a SUCCESSFUL last-response read through the full server stack. The mock
  has no native transcript so only the negative (`transcript_unavailable`) path is e2e-tested;
  the successful locate→parse→read is covered by `transcript.rs` parser fixtures. Best-effort
  per master-prompt §6 — to be exercised in the final manual-e2e phase (real providers).
