# orchestratr

`orcr` — a cross-harness agent orchestrator built on [herdr](https://herdr.dev).

Spawn, steer, await, and supervise coding agents (Claude Code, Codex, Pi, OpenCode) running
as real interactive TUIs in background herdr panes — plus loops, schedules, goals, dynamic
workflows, and a live tree TUI. Any harness can drive any other harness through one small CLI.

Status: pre-alpha, under active development. See [spec/](spec/) for the design contract.

## Requirements

- [herdr](https://herdr.dev) installed and on `$PATH` (or set `herdr.bin` in `~/.orcr/config.toml`)
- Rust toolchain (to build from source)

## Development

```
cargo build
cargo test                       # unit tests (no herdr required)
ORCR_E2E=1 cargo test --test '*' # e2e tests (require installed herdr)
```
