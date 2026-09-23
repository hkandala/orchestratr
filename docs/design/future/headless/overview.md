# Headless mode

## What it means

A headless agent runs without a terminal. The controlling program speaks a protocol to the
agent process directly, and reads structured events instead of screen text.

Every one of the four kinds supports this. Each supports it differently.

## Why the first version does not use it

`purpose.md` states the price and control reasons. A third reason belongs to this file: the
four kinds share nothing here, and interactive terminals are the one surface all four have in
common.

## What it would buy

- No screen reading of any kind, anywhere.
- Structured state instead of inferred state. Three of the four protocols report turn state
  as typed events.
- No startup screens. They belong to the terminal user interface.
- No pane, tab or workspace management.

## The shape it would take

orcr gains a launch mode, not a second product. The socket API does not change. The same
methods route to a different driver per kind.

The state vocabulary stays the same. Headless agents report state more precisely, not
differently.

Two commands have no meaning in headless mode: `orcr agent attach`, because there is
no terminal, and `orcr agent interrupt`, which becomes a protocol message rather than a key.

## What must be decided first

- Whether a single agent can be started headless while others run interactive, and what
  `orcr agent list` shows when both exist.
- Whether the transcript reader stays the same. Three kinds write the same transcript in
  both modes. Confirm the fourth.
- How model and effort arguments map, since some protocols set them per request rather than
  at launch.

## Per kind

| Kind | Protocol | File |
|---|---|---|
| Claude | newline JSON control protocol on stdio | `claude.md` |
| Codex | JSON-RPC app server on stdio | `codex.md` |
| Pi | newline JSON duplex on stdio | `pi.md` |
| OpenCode | HTTP with server sent events, or ACP | `opencode.md` |

Note the shape difference. Three kinds run one process per agent. OpenCode runs one process
for all agents. That difference alone changes process supervision, and it is the main reason
a headless design needs its own pass rather than a flag.
