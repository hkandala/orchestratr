# Future work

This folder holds work that orcr does not do. Nothing here is part of the specification in
`docs/design/`. Each file states what was left out, why, and the cost to add it.

| File | Subject |
|---|---|
| `permissions.md` | Moving off permission bypass onto each kind's auto mode |
| `api-send.md` | Delivering a message without typing it |
| `headless/overview.md` | Running agents without a terminal |
| `headless/claude.md` | The Claude control protocol |
| `headless/codex.md` | The Codex app server |
| `headless/pi.md` | The Pi RPC mode |
| `headless/opencode.md` | The OpenCode server and ACP |
| `tmux-backend.md` | Running on tmux instead of herdr |
| `external-agents.md` | Reading and driving agents orcr did not start |
| `dropped.md` | Smaller items cut from the first version |

## Why these are separate

The first version answers one question: can orcr start agents of four kinds, drive them the
same way, and report what happened. Everything in this folder widens that question before it
has been answered once.
