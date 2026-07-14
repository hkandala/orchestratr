# M3 · Completion & logs — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts (especially about herdr). Capture *decisions
and deviations*, not a play-by-play.

## Deviations from spec

- **Integration completion tuning is overridable via config `integrations.<provider>.*`
  (ms).** §14 says this tuning is "integration logic, not user config", but the M3
  milestone explicitly lists "integration tuning defaults + config overrides
  (`integrations.<provider>.*`)". Reconciled: defaults ship inside each integration
  (`driver::tuning_for`), and an optional `integrations` config section overrides any of
  `fast_turn_grace_ms / idle_stable_ms / transcript_settle_ms /
  transcript_freshness_timeout_ms / shutdown_grace_ms`. `integrations` is a known top-level
  config key (no unknown-key warning). Tests use it to run the mock matrix fast.
- **`agent.logs --follow` is a CLI-side poll, not a server-streamed subscription.** The
  handler is request/response (`streaming:false` in the schema); `--follow` re-requests the
  transcript every 500ms and prints new entries. The spec says "subscription under the
  hood"; the observable behavior (a live-updating tail) is identical and the acceptance
  (round-trips) doesn't require server push. Recorded as a simplification.
- **Freshness gate is now enforced on the read path** (`Server::last_response_fresh`, used by
  both `agent.logs --last-response` and `agent.ask`). The threshold is the transcript file
  mtime **captured at the observed completion** (`turns.transcript_cursor`, an mtime string) —
  *not* orcr's wall-clock `completed_at`. Using `completed_at` would be wrong: a real provider
  writes its response to the transcript *before* going idle, and orcr only marks
  `completed_at` after `idle_stable_ms + transcript_settle_ms` more elapse, so
  `mtime < completed_at` is the normal case and would make every read stale. Instead the read
  path requires `current mtime >= recorded-completion cursor` (via the existing
  `transcript_fresh` helper), polling up to `transcript_freshness_timeout_ms`; a file that
  never reaches it (rotated/truncated to an older state, or vanished) →
  `transcript_unavailable{cause:"stale"}` rather than a not-yet-advanced/stale read. This
  matches §11.4's "advanced past the observed completion" and the documented rotation
  limitation, while staying immediate (mtime == cursor right after completion) for the happy
  path. Agents with no recorded completion cursor (the mock has no native transcript) never
  reach the gate — `agent_transcript`/`locate_transcript` fails first. (Round-1 verifier fix.)

## Round-1 verifier fixes

- **Fast-turn-grace stale-idle race (high).** The completion monitor could falsely complete a
  freshly re-armed turn via the fast-turn-grace path before the provider ever reported
  `working`: `fast_ok` only required that idle *began* within `fast_turn_grace_ms` of delivery,
  so once `idle_stable_ms` (< grace) elapsed on a still-stale idle the turn completed —
  letting an old idle satisfy a newer send (violates §5.6/§6.1). This was load-sensitive
  (`e2e_two_sends_no_stale_idle` failed ~1-in-4 under full-suite load). Fix: `fast_ok` now
  additionally requires the **full grace window** to have elapsed since delivery
  (`now - delivered_at >= fast_turn_grace_ms`) with continuous idle. Any provider that starts
  working within the grace window sets `working_seen_at` first, so the fast path never applies
  to it; a genuinely fast turn (working never observed) still completes, just after the full
  grace window. Verified deterministic: `e2e_two_sends_no_stale_idle` + `e2e_fast_turn_completes`
  green across 5 runs under saturated CPU load, plus a full 8/8 completion suite run under load.
- **`wait` exit-code precedence (low).** §6.1 ranks the outcomes `4 blocked · 5 dead · 3
  timeout`; the CLI checked `timed_out` (3) before blocked/dead, so a mixed wait that both
  timed out (a target still working) and had an already-settled blocked/dead target returned
  3. Reordered `cmd_agent_wait` to `all_ok(0) → any_blocked(4) → any_dead(5) → timed_out(3)`
  (dead = a target settled non-ok whose reason is neither `blocked*` nor `wait_timeout`).

## Decisions on under-specified points

- **Completion state machine** runs as one background monitor thread (200ms tick) that
  polls the owned session's herdr `agent.list` once per tick and drives each monitorable
  agent (status ∈ working/idle/blocked/parked with a pane). Per open turn: `working` after
  delivery sets `turns.working_seen_at` (and clears `idle_since`); `idle` sets/holds
  `idle_since` and completes the turn once (working-seen OR fast-turn-grace) AND stable idle
  (`idle_stable_ms`) AND transcript settled. `blocked` → status blocked (turn-scoped kind).
  A herdr `working` with **no open turn** → synthetic external turn (`source=external`).
- **`send` re-arms to `working`** (`Store::deliver_input`): bumps `input_seq`, opens a turn,
  clears `idle_since`/`blocked_kind`, sets status `working`, emits `status_changed`. This is
  what makes "an old idle can never satisfy a newer send" hold — a `wait` issued after a
  `send` blocks on the new turn. The pipeline's first-prompt delivery uses the same path.
- **gc immediate goes `working → ended (completed)` with no transient public `idle`**
  (`Store::complete_turn_row` marks the turn without flipping status; then graceful shutdown
  + `end_agent(completed)`). Otherwise a racing `wait`/`ask` would settle on the transient
  `turn_complete` instead of `completed`. Non-immediate modes flip `working → idle`
  (`Store::complete_turn`).
- **Conservative restart re-arm**: on server start the reconciler clears `idle_since` for all
  active managed agents (`Store::clear_active_idle_since`), so completion re-measures a
  fresh idle streak rather than trusting a pre-crash one (§5.6 restart safety). `turns`
  rows (incl. `working_seen_at`) persist, so a turn still resumes correctly.
- **External-turn detection needs an observable herdr `working` transition.** With the mock
  at `turn_ms=0` a turn is too fast for the 200ms monitor to catch `working`; the e2e uses a
  non-zero turn for external input. Real providers report `working` for long enough.
- **`wait` implementation**: snapshot the active target set at invocation, then loop reading
  all target rows + the event cursor under one store lock and blocking on the event bus
  (`bus.wait_for`) until all are simultaneously settled → that cursor is `decision_seq`. A
  target that un-settles is simply re-read. `settle_of` maps `status × exit_reason` to the
  §6.1 reason tokens; `next_hint` renders the structured enum.
- **`blocked_kind` classification is coarse best-effort** (from any pane title/name text →
  login|limit|question|unknown); herdr exposes no structured reason (§5.6, detailed
  per-provider parsing is future work §17). The mock's blocked turns classify as `unknown`.

## Discovered facts / gotchas

- **claude transcript**: `~/.claude/projects/<cwd-slug>/<session_id>.jsonl` where the slug
  is the absolute cwd with every non-alphanumeric char → `-` (e.g.
  `-Users-hkandala-code-orchestratr`) and `<session_id>` = the pane's `agent_session.value`
  (kind `id`). Lines: `{type:"assistant"|"user", message:{role, content:[…], usage:{…}}}`;
  assistant content blocks are `text`/`thinking`/`tool_use`; tokens in `message.usage`. The
  final response is the last non-empty assistant `text` block. Verified against a real file.
- **codex transcript**: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<session_id>.jsonl`;
  rows `{type:"response_item", payload:{type:"message", role, content:[{type:"input_text"|
  "output_text", text}]}}`; session id is the filename suffix + `session_meta.payload.id`.
- **herdr surfaces reported idle-after-working as `done`** (from M0) — `normalize_done`
  maps it to `idle` for the completion check; the monitor treats `done` == `idle`.

## Verifier & reviewer history

- **Implementation** (this pass, on `main`): config `integrations.*` overrides + tuning
  params → transcript adapters (claude/codex) + fixtures → turns/completion store DAL →
  completion monitor (verified idle, fast turn, external turns, blocked, gc immediate) →
  `wait`/`ask`/`logs` handlers + settle mapping/next-hint → CLI verbs (wait/ask/logs +
  --follow/--last-response/--tail) → api schema (implemented flags) → mock blocked/tool-gap
  directives → Client wait/logs/last_response → unit + e2e tests. Green gates: `cargo build`,
  `cargo fmt --check`, `cargo clippy --all-targets -D warnings` clean, `cargo test` (111
  unit + non-e2e suites), `ORCR_E2E=1 cargo test --test completion_e2e` 8/8 and
  `--test agent_e2e` 9/9 against live herdr 0.7.2 with the mock provider. Real-provider
  (claude/codex) live round-trip is deferred to the manual-e2e phase (best-effort); the
  claude parser was cross-checked against a real on-disk transcript.
- **Revision round 1** (this pass, on `main`): fixed the four verifier findings —
  (high) fast-turn-grace stale-idle race in `completion.rs`; (medium) §11.4 freshness gate
  now enforced on the `last_response` read path (`Server::last_response_fresh`); (low) `wait`
  exit-code precedence reordered to blocked/dead > timeout in `cli.rs`; (low) the successful
  last-response read *through the full server stack* against real claude/codex is explicitly
  tracked for the manual-e2e phase (see below) — the successful locate→parse→read path is
  otherwise covered by the `transcript.rs` parser fixtures, and only the negative
  (`transcript_unavailable`) path is exercised in the mock e2e (the mock has no native
  transcript, so a successful server-stack read cannot be faked without a real provider).
  Gates after the round: `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets`
  clean, `cargo test --lib` 111 green, `ORCR_E2E=1 … --test completion_e2e` 8/8 and
  `--test agent_e2e` 9/9 under saturated CPU load; the previously-flaky
  `e2e_two_sends_no_stale_idle` is now deterministically green (5/5 focused runs + a full
  suite run, all under load). A leaked `orcr_test_*` session from an earlier interrupted run
  was stopped+deleted; the user's `default` session was never touched.
