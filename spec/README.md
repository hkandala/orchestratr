# orchestratr — Design Specification

This folder is the authoritative design contract for orchestratr. It supersedes the old
root `SPEC.md`. Files are numbered in reading order; implementation phases map to
[10-roadmap.md](10-roadmap.md).

| Doc | Contents |
| --- | --- |
| [01-overview.md](01-overview.md) | Problem, solution shape, principles, naming |
| [02-architecture.md](02-architecture.md) | Components, daemon-on-demand, store, schema, status model, exit codes |
| [03-cli.md](03-cli.md) | Full CLI surface: verbs, flags, JSON envelopes, examples |
| [04-execution.md](04-execution.md) | Run modes, env contract, run dirs, steer semantics, herdr driver rules |
| [05-agents.md](05-agents.md) | Agent integration contract — one module per harness, how to add one |
| [06-jobs.md](06-jobs.md) | loop / schedule / goal / workflow |
| [07-tui.md](07-tui.md) | `orcr top`, auto-viewer, tree/history rendering |
| [08-skill-sdk.md](08-skill-sdk.md) | The skill + TS/Python SDK |
| [09-testing.md](09-testing.md) | Unit strategy, fake-herdr shim, mock agent, e2e rules |
| [10-roadmap.md](10-roadmap.md) | Milestones M0–M4 with acceptance criteria, future work |

Conventions used throughout: the CLI is written `orcr` (the `orchestratr` binary is
identical); "harness" means an agent CLI product (Claude Code, Codex, Pi, OpenCode);
"agent" means one running instance orcr manages; paths like `~/.orcr/` follow the store
layout in 02.
