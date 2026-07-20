# Spec-completeness audit — round 1 (consolidated)

**Question:** is the *entire* `spec/spec.md` design implemented for this release?
**Verdict:** ✅ **COMPLETE — zero real gaps.** Every spec requirement for the first
release is implemented with cited evidence; everything not built is either spec §17
"Future work" or a milestone `notes.md` documented deferral (listed at the end).

Method: five parallel auditors swept the spec by span (CLI, socket protocol,
identity/store, execution/lifecycle, config/TUI/SDK/skill), each confirming presence
by `file:symbol` / `--help` / `api schema` line rather than "looks implemented".
Evidence cross-checked against the pre-dumped implemented surface
(`/tmp/orcr_surface/*`), `src/`, and `spec/impl/CODEBASE.md`. Static only (no cargo).

State audited: **through M7 + the comprehensive spec-vs-impl review phase**.
Surface baseline: 26 socket methods (all `implemented=true`), 21 event kinds,
9 error codes, config sections `defaults/herdr/concurrency/timings/logs/integrations`.

---

## Span 1 · CLI surface (§6)

`agent` / `loop` / `server` / `api` nouns + `top` / `scaffold` verbs — every command,
sub-command, flag, and documented behavior.

| spec_item | ref | status | evidence |
| --- | --- | --- | --- |
| 4 nouns + `top`/`scaffold`; global `--json`/`--version`/`--help` | §6 | implemented | `help.txt` root; `cli-verbs.txt` header |
| Every command `--json` envelope (top is TUI exception) | §6 | implemented | `--json` on every sub-command in `help.txt`; `cli.rs` emit_success/emit |
| Exit-code map 0/1/2/3/4/5/6/7 | §6/§13 | implemented | `error.rs:59-72` `ErrorCode::exit_code`; exit 5 from `cli.rs` wait/kill classification |
| Durations carry units | §6 | implemented | `src/duration.rs` (units required) |
| Confirmation contract (kill / run stop / rm --kill-active; `-y`; non-TTY/`--json` proceed) | §6 | implemented | `cli.rs:1695 confirm()`; CODEBASE L104-107 |
| Wait-timeout → `ok:true, timed_out:true`, exit 3; `timeout` code reserved for agent/run `--timeout` | §6 | implemented | `cli.rs:688,714`; `error-codes.txt` timeout→3 |
| `agent run` — (`--name`\|`--path`) exactly one, `-a`/`-p`/`-p -`/`--gc`/`--model`/`--effort`/`--cwd`/`--timeout`; prints `<path> <uuid>` + TTY hint | §6.1 | implemented | `help.txt`; `cli.rs:604 cmd_agent_run`, :620 hint |
| `agent ask` — run --gc immediate → wait → last-response; prints response | §6.1 | implemented | `cli.rs:655 cmd_agent_ask`; `engine.rs handle_agent_ask` |
| `agent send` — un-parks; `delivered_while`+`input_seq`; prompt required; ended→not_found | §6.1 | implemented | `cli.rs:627`; `engine.rs:763,806`; `gc.rs unpark_for_send` |
| `agent logs` — `--last-response`/`--tail`/`--follow`; both-layers gate | §6.1 | implemented | `cli.rs:722`; `engine.rs handle_agent_logs` |
| `agent wait` — settle semantics, `<path> <reason>` lines, JSON `next` enum + `decision_seq`/`all_ok`/`timed_out` | §6.1 | implemented | `engine.rs:1552-1592 settle_of/next_hint`; snapshot-then-subscribe |
| `agent attach` — `--takeover`; lease-first prepare → exec herdr → heartbeat → release | §6.1 | implemented | `cli.rs:849 cmd_agent_attach`; engine attach handlers |
| `agent kill` — `--force`/`-y`; killed/skipped/all_killed; no-match→6, all-skipped→7; barrier kill | §6.1 | implemented | `cli.rs:916,944-963`; barrier kill CODEBASE L752-756 |
| `agent ls` — filters, path tree, TTY columns, flat JSON rows | §6.1 | implemented | `cli.rs:970`; `server agent_row_json` |
| `loop create` — cadence exactly one, `--max-concurrency`/`--overlap`/`--timeout`/`-- <cmd>`; echoes argv+cadence+cancel | §6.2 | implemented | `cli.rs:1154`; `cron::describe` |
| `loop pause`/`resume` | §6.2 | implemented | `cli.rs:1204 cmd_loop_set`; `loops.rs set_loop_status` |
| `loop rm` — `--kill-active`/`-y`; removed/removed_by_run | §6.2 | implemented | `cli.rs:1225`; `loops.rs:852-877` |
| `loop ls` | §6.2 | implemented | `cli.rs:1284`; store list_loops/all_loops |
| `loop logs` — `--run`/`--source {orcr,command}`/`--tail`/`--follow`; interleaved, per-run tagged | §6.2 | implemented | `cli.rs:1310`; `events_for_refs` |
| `loop run start` — always allocates; prints `<loop>/<run_id> <uuid>`; pending at cap | §6.2 | implemented | `cli.rs:1354`; store `allocate_run` |
| `loop run stop` — stopping barrier → TERM→grace→KILL→glob-kill; canceled if pending | §6.2 | implemented | `cli.rs:1365`; `loops.rs:369-437 enter_stop_barrier/finish_stop` |
| `loop run ls` | §6.2 | implemented | `cli.rs:1417`; store `runs_for_loop` |
| Loop cadence (5-field cron DST-correct / `--once-at`, missed→skip, run ids r+5, own pgroup + §5.3 env, statuses, overlap) | §6.2 | implemented | `src/cron.rs` + DST tests; `loops.rs fire_loop/setsid/recover_loops_on_start` |
| `top` — filters incl. `--loops`, view-only, active-only, `--all` unsupported | §6.3 | implemented | `help.txt`; `cli.rs:423`; `top/app.rs run_top` |
| `server start` — idempotent, `--foreground`, `already_running` | §6.4 | implemented | `cli.rs:1442`; `client.rs:33 AlreadyRunning` |
| `server stop` — graceful control-plane, never touches panes | §6.4 | implemented | `cli.rs:1462`; `server/mod.rs` graceful stop |
| `server status` — version/protocol/paths/herdr/counts/loops_firing/schedule/drift | §6.4 | implemented | `cli.rs:1479`; server.status schema |
| `server logs` — `--tail`/`--follow` | §6.4 | implemented | `cli.rs:1489` |
| `server enable`/`disable` — launchd/systemd, echoes unit path, `unsupported_platform` elsewhere | §6.4 | implemented | `cli.rs:1516/1529`; `service.rs launchd_plist/systemd_unit/build_unit` |
| `api schema` — versioned JSON, `--output` | §6.5 | implemented | `cli.rs:1543`; `api.rs schema_document` |
| `api snapshot` — live state + `snapshot_seq` | §6.5 | implemented | `cli.rs:1570`; `server/mod.rs build_snapshot` |
| `scaffold` — 3 files + npm install; Node≥20 preflight/environment_error (nothing created); no-overwrite→state_conflict; local | §6.6 | implemented | `cli.rs:406`; `src/scaffold.rs` |

---

## Span 2 · Socket protocol — methods, events, error codes, JSON shapes (§6.5, §11.6, §13)

| spec_item | ref | status | evidence |
| --- | --- | --- | --- |
| Every CLI verb maps 1:1 to a socket method; `api schema` publishes all | §11.6 | implemented | `api-schema.json`: 26 methods; `api.rs methods()` single registry; all routed in `server/mod.rs handle_request` |
| `agent.run/ask/send/logs/wait/kill/ls` registered + implemented (not stub) | §11.6/§13 | implemented | `api-schema.json` all `implemented=true`; `engine.rs` builds results |
| Attach split methods `agent.attach.prepare/heartbeat/release` (terminal-mediated exception) | §6.1/§11.6 | implemented | `api-schema.json` 3 attach methods; `engine.rs` attach handlers |
| `loop.create/pause/resume/rm/ls/logs` + `loop.run.start/stop/ls` | §11.6 | implemented | `api-schema.json` all `implemented=true`; `loops.rs handle_loop_*` |
| `server.handshake/status/stop`, `api.schema/snapshot`, `events.subscribe`, `watch.open` | §6.4/§6.5/§11.6 | implemented | `api-schema.json`; `server/mod.rs` |
| Local-only verbs (`server start/logs/enable/disable`, `scaffold`) correctly have NO socket method | §6.4/§6.6 | implemented | `service.rs`/`scaffold.rs` run CLI-side; not in registry (by design) |
| 18 spec'd event kinds present (+ additive) | §11.6 | implemented | `api-schema.json events`: 21 kinds incl. all §11.6 set; additive `server_stopping`/`loop.ended`/`loop_run.stopping` |
| Intent/applied pairs; events written in same txn; `watch.open` cursor pin; `cursor_expired` → re-snapshot | §11.6 | implemented | `store append_event_tx`; `events.rs EventBus oldest_retained_seq`; `top/app.rs` reconnect |
| 9 error codes + correct exit mapping | §13 | implemented | `error-codes.txt`; `error.rs:15-25/59-72`; published under `errorCodes` in `api-schema.json` |
| `environment_error` causes (herdr_unreachable, server_start_failed, store_locked, config_invalid, unsafe_home, unsupported_platform, unsupported_version) | §13 | implemented | `error.rs:140-146` |
| Load-bearing §13 JSON result shapes (run/ask/send/logs/wait/kill/ls; loop create/run start/stop/ls/ls/logs; server status) | §13 | implemented | schema result fragments in `api-schema.json`; `agent_row_json`/`loop_row_json`/status builder |
| Version negotiation (`unsupported_version`), max frame, additive unknown-field tolerance | §11.6 | implemented | `wire.rs ORCR_PROTOCOL/MAX_FRAME/unsupported_version` |
| Transport: `~/.orcr/orcr.sock` umask 077 mode 0600; symlink/lstat rejects; single-instance flock | §11.6 | implemented | `home.rs` safety; `lock.rs InstanceLock`; `client.rs ensure_running` |

---

## Span 3 · Identity, paths, status model & store (§5.1, §5.6, §5.7, §12)

| spec_item | ref | status | evidence |
| --- | --- | --- | --- |
| Grammar in one place: segment/path/abs_path/pattern/`{rand}`/loop name/run id | §5.1 | implemented | `src/path.rs` (validation, depth, `{rand}` expand, `Pattern`) |
| uuid = UUIDv7 PK; ≥8-hex prefix resolution; path-first then uuid-prefix; `resolved: active\|latest_ended` | §5.1 | implemented | `store find_by_path`→`Resolution`, `find_by_uuid_or_prefix`→`UuidLookup` |
| Glob `*`=one segment, `**`=any depth, anchored; no SQL LIKE | §5.1/§12 | implemented | `path.rs Pattern`; `store list_agents` glob applied in Rust |
| Path uniqueness among active (partial unique index) in one `BEGIN IMMEDIATE` txn; `path_in_use` state_conflict | §5.1/§12 | implemented | `schema.rs` partial unique index; `store enqueue_agent` |
| Reserved level-1 names (`idle`, `unmanaged`, active-loop) + enforcement order (parse→resolve→grammar/depth→reserved→loop→uniqueness) | §5.1 | implemented | `path.rs` reserved/depth checks; `engine.rs check_loop_namespace` |
| `{rand}` creation-only placeholder | §5.1 | implemented | `path.rs expand_rand` (creation resolvers only) |
| Managed status vocabulary (queued/starting/working/idle/blocked/parked/ended/lost) | §5.6 | implemented | `schema.rs agents.status`; `store transition_status` |
| Unmanaged lifecycle (working/idle/blocked/unknown/ended) | §5.6/§5.7 | implemented | `discovery.rs`; `store` unmanaged upsert |
| `exit_reason` full set (completed/reaped/killed/timeout/canceled/failed/lost) | §5.6 | implemented | `schema.rs agents.exit_reason`; set across engine/gc/loops |
| Turn/completion bookkeeping (`turns` table, input_seq, source orcr\|external, restart-conservative) | §5.6/§12 | implemented | `schema.rs turns`; `completion.rs`; `store deliver_input/complete_turn` |
| Managed vs unmanaged behavior contract (kill needs --force; no GC/queue/lineage for unmanaged) | §5.7 | implemented | `discovery.rs` read-only rows; `engine.rs` force-required kill |
| Store schema §12 (agents/turns/attaches/loops/loop_runs/events + indexes + meta schema_version) | §12 | implemented | `store/schema.rs`; `store_version_mismatch` refusal |
| Derived-never-stored (name/home workspace/queue_position/age/agents count/data dirs) | §12 | implemented | `path.rs name_of/home_workspace`; `store queue_position` |
| launch.json / loop.json / run.log as files (no prompt/response blobs) | §12 | implemented | `engine.rs LaunchPayload`; `loops.rs LoopPayload`/run.log JSONL |

---

## Span 4 · Execution — spawn, queue, GC, reconciliation, loops, integrations (§5.4, §5.5, §11.1–11.5, §11.7)

| spec_item | ref | status | evidence |
| --- | --- | --- | --- |
| Spawn pipeline durable-before-side-effects (row+uuid+data dir before herdr; launch token; cancel checks) | §11.1 | implemented | `engine.rs run_pipeline`; `store enqueue_agent`; `ORCR_LAUNCH_TOKEN` |
| Queue: global + per-provider caps, FIFO `queue_seq` atomic promotion | §5.5 | implemented | `store promote_queued` (global+per-provider, one txn); `config concurrency.*` |
| Stuck-start guard (`max_starting`, progress-marker reset) → failed, releases slot | §5.5 | implemented | `engine.rs` queue worker stuck-start sweep; `store stuck_starting` |
| kill on queued→canceled, on starting→cancel_requested interlock | §5.5 | implemented | `store request_cancel/is_cancel_requested`; engine checks |
| GC modes auto/immediate/never; no default timeout; explicit `--timeout` → timeout | §5.4/§11.2 | implemented | `gc.rs` (park/reap/timeout); `completion.rs` gc-immediate teardown |
| Two-phase crash-safe park/un-park (move_state lease, home workspace, roll-forward/back) | §5.4/§11.2 | implemented | `gc.rs begin_move/finish_park/finish_unpark/rollback_move`; `store` move CAS |
| Interlocks: send cancels pending park before delivery; completion ordered before kill; attach-lease GC guard survives restart | §5.4 | implemented | `gc.rs unpark_for_send/lease_fresh`; `store` attach leases |
| GC engine tick + timeout enforcement across all gc modes | §11.2 | implemented | `gc.rs` tick (`timings.gc_tick`), `timed_out_agents` |
| Loop scheduler: tz cron `next_fire_at`, run rows, process groups (pgid+start-time guard), overlap coalesce/skip, restart recovery | §11.3 | implemented | `loops.rs start_loop_scheduler/fire_loop/recover_loops_on_start`; `store allocate_run/claim_pending_run/record_run_start` |
| Loop stop/timeout: stopping barrier → TERM `-pgid` → grace → KILL → glob-kill until clean | §11.3 | implemented | `loops.rs enter_stop_barrier/finish_stop/glob_kill_run_agents` |
| Missed fires skipped-and-logged (cron + once), never replayed | §11.3 | implemented | `loops.rs recover_loops_on_start` emits `loop.skipped`; loop_e2e `e2e_missed_cron_fire_skipped` |
| Integrations both-layers-required; `integration_missing` naming missing layer + install cmd; discovery skips unsupported; status reports per-provider state | §11.4 | implemented | `driver/integration.rs ensure_supported`; `api.rs server.status integrations{}` |
| Transcript adapters: identity gate (agent_session + created_at, ambiguous→transcript_unavailable), freshness gate, no response copies | §11.4 | implemented | `driver/transcript.rs locate_transcript/transcript_fresh` |
| Reconciliation: lost (path reserved until confirmed), unknown-marked/unmarked panes reported-never-touched, move repair, unmanaged discovery keyed by (session, terminal_id) | §11.5 | implemented | `gc.rs periodic_reconcile`; `discovery.rs`; `engine.rs reconcile_on_start` |
| herdr driver contract pinned to named methods + conformance fixture (version drift fails CI) | §11.7 | implemented | `driver/contract.rs`; `tests/conformance_live.rs` |

---

## Span 5 · Config, env, TUI, SDK, skill, scaffold, data conventions (§5.3, §7, §8, §9, §10, §14)

| spec_item | ref | status | evidence |
| --- | --- | --- | --- |
| Env contract injected into panes + loop-run commands (ORCR_ID/PATH/PARENT_*/AGENT_DATA_DIR/LOOP_DATA_DIR; all absolute) | §5.3 | implemented | `env-vars.txt`; `engine.rs spawn ~L586-606`; `loops.rs spawn_run ~L167-178` |
| Caller-scope derivation (agent=path minus name; loop-run=full run path) | §5.3 | implemented | `path.rs scope_of_agent`; `engine.rs caller_context`; `cli.rs:557-558` reads back |
| Config §14: defaults/herdr/concurrency/timings/logs; unknown keys warn (nearest suggestion); strict validation; CLI>config>default | §14 | implemented | `config-keys.txt`; `src/config.rs` (Levenshtein suggestion, clamp, units) |
| Per-provider completion tuning is integration logic (`integrations.<p>.*` optional override) | §14 | implemented | `config.rs IntegrationTuning`; `driver/integration.rs tuning_for` |
| `ORCR_HOME` relocates home; `ORCR_HERDR_BIN`/`ORCR_HERDR_SESSION` overrides | §14 | implemented | `home.rs:24`; `config.rs:52,156` |
| `top` TUI (§7): path tree, lineage `↖` annotation (once), glyphs, blocked floats up, filters parity with ls, snapshot+event render | §7 | implemented | `top/model.rs build_tree/agent_matches`; `top/app.rs run_top` |
| SDK: generated protocol client (100% methods) + convenience helpers; `scope()`/`ask()`/`watch()`; `prepareAttach`; typed errors per §13; `killOnThrow` | §8 | implemented | `sdk/ts/src/generated.ts` (+ codegen drift check), `client.ts`, `scope.ts`, `errors.ts` |
| SDK path/scope parity with `path.rs` (client-side resolution → absolute selectors) | §8 | implemented | `sdk/ts/src/path.ts` (1:1 port); parity test |
| `context.fromEnv()` env-derivation helper | §8 | implemented | `sdk/ts/src/context.ts` |
| Data-dir convention (`~/.orcr/data` mirrors path tree; loop/run nesting; existence+uniqueness guaranteed) | §8 | implemented | `store`/engine data-dir creation; `env-vars.txt` ORCR_*_DATA_DIR |
| §9 workflow examples shipped as tested fixtures | §9 | implemented | `sdk/ts/recipes/` (9.1–9.7); `tests/recipe_e2e.rs` |
| Skill: SKILL.md + references (cli/sdk/patterns/loops/files); doc-tested (no stale flags; naming mandatory) | §10 | implemented | `skill/SKILL.md` + `references/*.md`; `tests/skill_docs.rs` |
| Scaffold TS project (pinned SDK via ORCR_SDK_SPEC), workflow-code homes convention | §6.6/§8 | implemented | `src/scaffold.rs`; `recipe_e2e` |

---

## Real gaps (required for this release, missing/partial)

**None.** Every spec requirement for the first release is implemented with cited
evidence above. `complete = true`.

---

## Deferred (expected — NOT gaps)

Explicitly out of scope for this release; documented in spec §17 "Future work" or a
milestone `notes.md`. Reported here so the audit is exhaustive, not as defects.

| item | source |
| --- | --- |
| pi/opencode built-in `AgentIntegration` modules | §17; §11.4 table |
| Degraded no-integration (single-layer) modes | §17; §11.4 |
| `top` actions (detail panel: attach/send/kill/logs from TUI) + live activity feed | §17; §7 ("view-only in first release") |
| `send` steer/stop options (interrupt/graceful-stop per provider) | §17; §6.1 |
| Background-subagent detection for claude (don't park/reap in-flight) | §17; §5.4 |
| Structured per-provider blocked-reason classification + rate-limit policies | §17; §5.6 (`blocked_kind` is best-effort today) |
| Cross-host orchestration from local CLI (socket tunnel, remote transcripts/pgroups) | §17; §11.8 |
| Permission policies (`--read-only`, profiles); today everything runs bypass | §17; §11.4 |
| Notifications beyond terminal (herdr notify, webhook/ntfy) | §17 |
| Python SDK | §17; §8 ("Python deferred") |
| Coordination primitives (inboxes, decision gates, task boards) | §17 |
| Git worktree provisioning | §17 |
| **Windows** (named-pipe transport, path conventions, Task Scheduler `enable`) — `service.rs` returns `unsupported_platform` | §17; §6.4; `m5-loops/notes.md` |
| TCP/HTTP listener for the socket API | §17; §11.6 |
| Data-dir lifecycle / retention GC for `~/.orcr/data` | §17; §8 |
| Presets (`orcr agent run @review …`) | §17 |
| `orcr scaffold <lang>` (Python etc.); TS-only this release | §17; §6.6 |
| herdr plugin packaging (plugin pane, context actions, `herdr plugin install`) | §17 |
| Declarative YAML workflows + run replay | §17 |
| **npm publish** of `@orchestratr/sdk` — package is unpublished (`0.0.0`); `ORCR_SDK_SPEC` tarball used for offline/scaffold install | `m7-sdk-skill/notes.md` |
| **Real-provider (claude/codex) live smoke** of recipes/logs — mock-against-live-herdr is the automated gate; real-provider is best-effort in the manual-e2e phase | `master-prompt.md §6` (L154-155); `m3`/`m7` notes |
| Real launchd/systemd login-session `enable` round-trip — golden unit-file tests cover content; live registration left to manual e2e | `m5-loops/notes.md` |
