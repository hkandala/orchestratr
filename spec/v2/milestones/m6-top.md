# M6 · top

The live dashboard (spec §7): one TUI that shows the whole tree — groups, lineage,
loops and their runs, statuses in real time — and lets a human act (attach, send,
kill, logs) without leaving it.

## Scope

### Rendering
- Tree = group hierarchy (level-1 groups as top nodes, matching herdr workspaces) +
  parent→child edges from `ORCR_PARENT_*` lineage; loops as nodes with active runs as
  subtrees (`run <run_id>`); parked agents collapsed into an `Idle` node; unmanaged
  agents grouped under `unmanaged.<session>`.
- Display transform for headings (machine fqn shown alongside); status glyphs
  (`●` working · `○` idle · `◐` blocked, pulsing + floated upward · `⟳` running loop
  run · dimmed queued/starting with queue position).
- Detail panel for the selected node: uuid, status + age, provider/model/effort,
  parent, cwd, turn/input_seq, gc mode + clocks, `last ►` response snippet (from the
  captured `final_response` or the transcript adapter).
- Layout per the §7 mock; graceful degradation on narrow terminals.

### Data path (spec §11.6)
- Strict snapshot-then-subscribe: one consistent snapshot (agents, loops, runs, queue
  positions, GC clocks, parent edges) at `snapshot_seq`, then the event stream from
  that sequence; reconnect with re-snapshot on `cursor_expired` or server restart —
  the tree can never miss or double-apply an update.
- No polling; renders are event-driven with a coalescing frame budget (a 100-event
  burst is one redraw).

### Interaction
- `enter` attach (hand the terminal to the pane; return on detach) · `s` inline send
  prompt · `k` kill (same confirmation contract as the CLI) · `l` logs view
  (tail + follow) · `w` wait-marker on a node (visual) · `/` fqn-prefix filter ·
  collapse/expand · `q` quit.
- CLI filters pre-scope the tree: `orcr top [<fqn-prefix|uuid>] [-a <provider>]
  [--status <s>] [--managed|--unmanaged] [--loops]`; live-only by design (`--all` is
  `ls --all`'s job).

## Acceptance

- Correctness: a scripted storm (spawns, sends, completions, parks, loop fires,
  kills — hundreds of events) rendered from snapshot+stream matches the store's final
  state exactly (golden tree diff); repeated with a mid-storm server restart.
- Scale: 100-agent tree renders and updates under the frame budget without dropped
  events.
- Keys drive real agents e2e: attach round-trip, send from the TUI lands a turn, kill
  confirms and reaps, logs view follows.
- Filters: each CLI filter and the `/` filter produce the same node sets as the
  equivalent `ls` query.

## Out of scope

Per-agent live activity feed (tool calls / response summaries in the tree) — future
work (§17).
