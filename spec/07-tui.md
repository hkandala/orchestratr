# 07 · TUI: top, tree, history

## `orcr top`

ratatui + crossterm. Reads sqlite; refreshes by tailing `events` (poll the max seq —
cheap) at ~500ms. Tree pane left, detail pane right.

```
┌ orcr · 6 agents · 2 jobs ──────────────┬─ a7 codex "review" ────────────────┐
│ ▼ w4  parallel-review.ts    ●running   │ status   working        12m04s    │
│   ├─ a5 claude  impl        ●working   │ harness  codex · gpt-5.2 · high   │
│   ├─ a7 codex   review      ●working   │ host     local · herdr orcr/w3:p2 │
│   └─ a8 pi      docs        ◐blocked ⚠ │ tokens   41.2k in · 8.7k out      │
│ ▼ l2  loop "fix one test"   ⟳ 4/20     │ turns    3                        │
│   └─ a9 claude  fixer       ○idle      │ last ►  "Found 3 issues in the    │
│ s1  sched triage next 09:00 ─ ·        │  error-handling path, the retry…" │
│                                        │ [Enter]attach [s]end [k]ill [o]ut │
└─ [/]filter [K]ill-tree [g]c [q]uit ────┴────────────────────────────────────┘
```

- Status glyphs + color; `blocked` rows sort upward and pulse — the needs-a-human queue.
- Keys: Enter attach (suspend TUI, hand terminal to pane, resume on detach) · `s` inline
  send prompt · `k` kill (confirm) · `K` kill-tree (confirm) · `o` open latest response
  in `$PAGER` · `/` filter · `g` gc · `q` quit.
- Detail pane: identity, timings, tokens (subtree rollup when the node is collapsed),
  last-response snippet (first ~300 chars of latest response file).

## Auto-viewer (v1)

When a spawn happens from inside herdr (`HERDR_ENV=1`) and config `viewer.auto = true`:
open `orcr top` once per herdr session as a `--no-focus` split beside the invoking pane
(`pane split --direction right --ratio 0.35` + `pane run "orcr top"`). Never steals
focus; never opens twice (guard: a named pane label `orcr-top`).

`orcr top --pane` does the same explicitly from any herdr pane.

## `orcr tree`

One-shot render of the same tree (no TUI), `--watch` re-renders on change, `--json`
emits the full tree structure. Agents and humans see the same picture.

## `orcr history`

Finished agents/jobs, newest first: id, name, harness/model, status+exit_reason,
duration, turns, tokens, run-dir path. Filters: `--since 7d`, `--name`, `--harness`.
`--json` for scripts. Backed by the same store — history is just `ended_at IS NOT NULL`.
