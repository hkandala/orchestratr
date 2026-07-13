# M6 · top

The live dashboard (spec §7): one **view-only** TUI showing the whole path tree —
agents, lineage annotations, loops and their runs, statuses in real time. A status display, not a
control surface: acting on agents stays in the CLI (a detail panel with actions is
future work).

## Scope

### Rendering
- Tree = path tree (level-1 path segments as top nodes, matching herdr workspaces) +
  parent→child edges from `ORCR_PARENT_*` lineage; loops as nodes with active runs as
  subtrees (`run <run_id>`); parked agents collapsed into an `Idle` node; unmanaged
  agents grouped under `unmanaged.<session>`.
- Display transform for headings (machine path shown alongside); status glyphs
  (`●` working · `○` idle · `◐` blocked, pulsing + floated upward · `⟳` running loop
  run · dimmed queued/starting with queue position).
- Rows: name, status glyph + status, provider·model, blocked kind when relevant,
  age; loop-run nodes show due_at + elapsed.
- Layout per the §7 mock (single tree, no detail panel); graceful degradation on
  narrow terminals.

### Data path (spec §11.6)
- Strict snapshot-then-subscribe: one consistent snapshot (agents, loops, runs, queue
  positions, GC clocks, parent edges) at `snapshot_seq`, then the event stream from
  that sequence; reconnect with re-snapshot on `cursor_expired` or server restart —
  the tree can never miss or double-apply an update.
- No polling; renders are event-driven with a coalescing frame budget (a 100-event
  burst is one redraw).

### Interaction
- Navigation only: `/` filter using the same §5.1 pattern grammar as `ls` (no
  implicit prefix matching) · arrows collapse/expand · `q` quit. No action keys in
  this milestone.
- CLI filters pre-scope the tree: `orcr top [<pattern|uuid>] [-a <provider>]
  [--status <s>] [--managed|--unmanaged] [--loops]`; live-only by design (`--all` is
  `ls --all`'s job).

## Acceptance

- Correctness: a scripted storm (spawns, sends, completions, parks, loop fires,
  kills — hundreds of events) rendered from snapshot+stream matches the store's final
  state exactly (golden tree diff); repeated with a mid-storm server restart.
- Scale: 100-agent tree renders and updates under the frame budget without dropped
  events.
- Filters: each CLI filter and the `/` filter produce the same node sets as the
  equivalent `ls` query (golden cases: `review`, `review/*`, `review/**`,
  `reviewer/**`, absolute `/` input from a scoped context).
- Lineage golden: agent `fix_build/fixer` creates `/verify/checker` → tree places
  it under `verify` with the `↖ fix_build/fixer` annotation; selection highlights
  lineage without duplicating or re-rooting any node.

## Out of scope

Action keys (attach/send/kill/logs from the TUI) and the per-agent live activity
feed — future work (§17).
