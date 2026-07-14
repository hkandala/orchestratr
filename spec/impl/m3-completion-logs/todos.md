# M3 · Completion & logs — todos

Ships: turns + completion, wait, transcript adapters, logs, ask, gc immediate.

## Setup
- [x] Read master-prompt.md + full spec.md + this milestone file + herdr-driver-reference.md
- [x] Learn claude + codex native transcript formats (read-only inspection)

## Turn completion (§5.6)
- [ ] Integration tuning params (fast_turn_grace_ms, idle_stable_ms, transcript_settle_ms,
      transcript_freshness_timeout_ms, shutdown_grace_ms) — defaults + `integrations.<p>.*` config overrides
- [ ] turns DAL: open turn lookup, working_seen, idle_since, complete_turn, blocked, transcript cursor
- [ ] Completion monitor thread: poll herdr status, drive per-turn state machine
- [ ] Completion rule: working-after-delivery (or fast-turn grace) → stable idle → transcript settled → flip working→idle
- [ ] First idle w/o input-scoped working never completes; old idle never satisfies newer send
- [ ] External turns: working w/ no pending delivery → synthetic turn (source=external, input_seq bumped)
- [ ] blocked turn-scoped (question|limit|login|unknown best-effort), cleared by send
- [ ] send bumps input_seq + resets to working (re-arm)
- [ ] Restart safety: turn progress persisted; missing fields → wait for fresh transition
- [ ] Events: agent.turn_completed / agent.response_captured with snapshot-updating payloads

## wait (§6.1)
- [ ] handler: patterns+uuids resolved to active agents, membership snapshotted at invocation
- [ ] snapshot-then-subscribe (bus wait) — all targets simultaneously settled at one decision_seq
- [ ] uniform `<path> <reason>` lines; reason tokens from status×exit_reason table
- [ ] JSON per target {uuid,path,status,ok,reason,exit_reason?,next} + all_ok + timed_out + decision_seq
- [ ] structured `next` enum (logs_last_response|attach|logs_history|none)
- [ ] idempotent (already-settled returns immediately); wait --timeout → ok:true, timed_out, exit 3
- [ ] no match → exit 6; blocked → exit 4; dead → exit 5

## Transcript adapters: claude + codex (§11.4)
- [ ] claude adapter: locate `~/.claude/projects/<slug>/<session_id>.jsonl`, parse JSONL → entries
- [ ] codex adapter: locate `~/.codex/sessions/**/rollout-*-<session_id>.jsonl`, parse → entries
- [ ] common shape: ordered messages, roles, tool calls, token counts where available
- [ ] identity gate: select by agent_session + created_at; multiple candidates → transcript_unavailable (ambiguous)
- [ ] freshness gate: final response only once transcript advanced past completion (timeout) else transcript_unavailable
- [ ] record transcript_locator/transcript_cursor on completion; no response copies

## logs (§6.1)
- [ ] agent.logs handler: entries, --tail, --last-response (fails loudly), history via uuid
- [ ] --follow (CLI polling under the hood)
- [ ] transcript_unavailable / integration_missing error paths

## ask (§6.1)
- [ ] agent.ask handler: run(gc immediate) → settle wait → last-response
- [ ] naming enforcement: no name/path → invalid_request; both → invalid_request; parity with run

## gc immediate (§5.4, §11.2)
- [ ] completion monitor: stable idle → transcript settled → response captured → graceful kill + pane closed → ended(completed)

## CLI + API + SDK groundwork
- [ ] CLI verbs: wait, logs, ask (+ --follow, --last-response, --tail)
- [ ] api.rs: flip implemented flags (ask/logs/wait), agent.ask params/result, next schema
- [ ] Rust Client convenience: wait/logs/last_response (used by tests)

## Tests
- [ ] Unit: completion state machine, reason mapping, next hint, transcript parse + ambiguity/freshness fixtures
- [ ] e2e (mock, live herdr): fast turn, slow tool-heavy (idle gaps), blocked mid-turn, external input,
      two consecutive sends (stale idle), gc immediate → ended(completed), restart mid-turn re-arm
- [ ] mock agent knobs: blocked, tool-gap toggles
- [ ] cargo build / fmt / clippy / test green; e2e green

## Acceptance criteria (prove each)
- [ ] send → wait → --last-response round-trips (claude/codex real; mock proves completion/wait/stale-idle)
- [ ] restart server mid-turn → wait re-arms conservatively and completes
- [ ] external-input → synthetic turn → subsequent wait settles
- [ ] gc immediate race: response readable before pane dies (real); mock → ended(completed)
- [ ] transcript ambiguity/freshness gates hit error paths in fixtures
- [ ] mock matrix: fast / slow tool-heavy / blocked

## Deferred / out of scope
- GC auto parking, attach, loops, pi/opencode adapters (later milestones / future)
