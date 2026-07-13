# 01 · Overview

## Problem

The agent ecosystem has fragmented into harnesses — Claude Code, Codex, Pi, OpenCode, and
more — each good at different things, with orchestration capability scattered unevenly:
Claude Code has subagents/loops/schedules/dynamic workflows, Codex has some, Pi-class
agents have none, and no harness can orchestrate another harness as a first-class citizen.

The patterns people actually want are cross-harness: Codex reviewing Claude Code's work;
fanning one prompt across three harnesses and merging; routing trivial tasks to a cheap
agent; running a subagent on a remote VPS; one harness conducting a workflow over others.

Existing escape hatches are brittle:

- **Shelling out with `-p`**: the session is a black box — can't attach, steer, or answer
  its prompts. Headless runs are also first in line to lose subsidized-plan pricing
  (Claude Code has announced that restriction for `-p`; not yet enforced, but the
  direction is clear).
- **Harness-specific plugins**: N×N integration problem, nothing generalizes.
- **Dynamic workflows**: exist only inside Claude Code.

And nobody owns the **tree**: no unified view of what's running, who spawned whom, or one
place to steer/kill/watch everything.

## Solution

**orchestratr** (CLI `orcr`) is a single Rust binary on top of **herdr** (the terminal
workspace manager for agents) with three faces:

1. **A CLI** — spawn, steer, await, inspect, kill agents on any herdr-supported harness;
   plus jobs: `loop`, `schedule`, `goal`, `workflow`.
2. **A TUI** (`orcr top`) — the live tree of agents and jobs; drill into any node to
   attach to its real session.
3. **A skill** — one markdown file teaching any harness the CLI vocabulary, giving every
   harness the orchestration powers only Claude Code has today.

```
you (or any harness, or cron)
  └─ orcr ────────────────────────► sqlite state + event log + run dirs (md files)
       │  spawn / send / wait / kill
       ▼
     herdr (named sessions, local + remote)
       ├─ pane: claude  "impl"      working   ◄── steer: orcr send impl "…"
       ├─ pane: codex   "review"    idle      ◄── attach: orcr attach review
       └─ pane: orcr top (viewer, --no-focus)
```

## Principles

1. **Real TUIs by default.** Every subagent runs as a full interactive harness in a
   background herdr pane: plan-pricing-safe, attachable, steerable, rescuable. Headless
   `exec` mode is an explicit opt-in.
2. **Files are the API between agents.** Prompts in, responses out, as markdown files in
   a fixed layout (04). Transcript parsing is a fallback, never the contract.
3. **The tree assembles itself.** An env contract (ORCR_ID/PARENT/DEPTH) recorded at spawn
   time gives full lineage with zero harness cooperation.
4. **herdr is discovered, never embedded.** Config `herdr.bin` → `$ORCR_HERDR_BIN` →
   `$PATH`; if missing, print install pointer (https://herdr.dev) and exit 2.
5. **CLI is the contract.** Human text on TTY, stable `--json` envelopes for agents and
   SDKs. No socket protocol in v1.
6. **Every integration is a module with the same contract** (05). Adding a harness never
   touches the engine. A plugin mechanism for out-of-tree integrations is future work (10).

## Naming

Project **orchestratr** (orchestratr.dev). Binary names `orchestratr` and **`orcr`**
(identical); daemon `orcr serve`; store `~/.orcr/`; env prefix `ORCR_`; owned herdr
session `orcr`; SDK packages `orchestratr` (npm/PyPI), imported as `orcr`.
