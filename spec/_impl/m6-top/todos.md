# M6 · top — todos

Ships: view-only TUI tree, live statuses, filters, navigation, snapshot+event rendering.

## Setup
- [x] Read master-prompt.md + full spec.md (§6.3, §7, §11.6) + this milestone + herdr-driver-reference.md
- [x] Add ratatui + crossterm deps

## Snapshot enrichment (data path, §11.6)
- [x] Enrich `agent_row_json` with model, move_state, herdr_session, last_status_change_at, starting_at, idle_since (top needs these)
- [x] Enrich `build_snapshot` loops with their active runs (run_id, uuid, status, due_at, started_at, kind)
- [x] Keep `api.snapshot` + `watch.open` shape backward-compatible (additive only)

## Pure model (unit-testable) — `src/top/model.rs`
- [x] Parse a snapshot document into typed `Snapshot` (agents, loops, runs)
- [x] `TopFilter` (pattern, provider, status, managed, loops_only) applied to agents
- [x] `build_tree`: path-prefix tree; level-1 segments as top nodes; loops+runs as nodes; parked → `Idle` node; unmanaged grouped under `unmanaged/<session>`
- [x] Lineage annotation: `↖ <parent path>` when parent lives elsewhere; never a second placement / re-root
- [x] `render_lines`: deterministic textual render for golden diffs (glyphs, name, status, provider·model, blocked kind, age; loop-run due/elapsed)
- [x] Node set == `ls` node set for the same filter (pattern grammar §5.1, no implicit prefix)

## TUI app — `src/top/app.rs`
- [x] watch.open → snapshot → build tree → render (ratatui + crossterm)
- [x] Event-driven refresh with coalescing frame budget (burst → one redraw); background reader thread + channel
- [x] Reconnect + re-snapshot on cursor_expired / server_stopping / disconnect
- [x] Keys: `/` filter (§5.1 grammar), arrows collapse/expand, `q` quit; navigation only (no action keys)
- [x] Graceful degradation on narrow terminals

## CLI wiring — `src/cli.rs`
- [x] `orcr top [<pattern|uuid>] [-a <provider>] [--status <s>] [--managed|--unmanaged] [--loops]`
- [x] live-only (`--all` unsupported); pattern resolved against caller scope; `/` uses same grammar
- [x] register `top` in dispatch

## Tests
- [x] Unit: filters (review, review/*, review/**, reviewer/**, absolute `/` from scoped ctx) == ls node sets
- [x] Unit: lineage golden (fix_build/fixer → /verify/checker under `verify` with `↖ fix_build/fixer`)
- [x] Unit: parked→Idle, unmanaged grouping, loop+run subtree placement
- [x] e2e (`ORCR_E2E`): scripted storm rendered from snapshot+stream matches store final state (golden tree diff)
- [x] e2e: mid-storm server restart → watch reconnect + re-snapshot still matches
- [x] e2e: 100-agent tree builds/renders under frame budget without dropped events
- [x] e2e: filter node sets match `agent ls` equivalents

## Acceptance criteria (from milestone)
- [x] Correctness: storm (snapshot+stream) matches store final state (golden tree diff); repeated with mid-storm restart
- [x] Scale: 100-agent tree renders/updates under frame budget without dropped events
- [x] Filters: each CLI filter and `/` filter produce same node sets as equivalent `ls`
- [x] Lineage golden: cross-scope child placed once with `↖` annotation, no duplication/re-root

## Quality gates
- [x] cargo build / cargo test (unit) green
- [x] cargo clippy clean, cargo fmt applied
- [x] ORCR_E2E top_e2e green against live herdr + mock
- [x] CODEBASE.md + notes.md updated

## Deferred / out of scope
- Action keys (attach/send/kill/logs from TUI) + per-agent live activity feed — §17 future work.
