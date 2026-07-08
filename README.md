# orchestratr

`orcr` — a cross-harness agent orchestrator built on [herdr](https://herdr.dev).

Spawn, steer, await, and supervise coding agents (Claude Code, Codex, Pi, OpenCode)
running as **real interactive TUIs** in background herdr panes — plus loops, schedules,
goals, dynamic workflows, a live tree TUI, and a skill that teaches any harness to drive
any other harness through one small CLI.

```sh
# a better `-p` for every harness — spawn, block, print the response
orcr run --harness codex -p "review this diff" --cwd ~/proj --wait

# fan-out, steer mid-flight, fan-in
orcr run -a claude -p "implement the parser" --name impl --keep
orcr run -a pi     -p "write docs for the parser" --name docs
orcr send impl --steer "also handle escaped quotes"
orcr wait impl docs --timeout 20m
orcr out impl --recursive --format path

# jobs
orcr loop -a claude --prompt-file fix-tests.md --max 20 --until "ALL PASS"
orcr schedule add "0 9 * * 1-5" -a claude -p "triage new issues"
orcr goal -a claude -p "make the test suite pass" --judge-harness codex
orcr workflow run ./parallel-review.ts
orcr top        # live tree TUI (auto-opens beside you when inside herdr)
```

Why real TUIs instead of headless `-p`: sessions are attachable (`orcr attach`),
steerable mid-turn (`orcr send`), rescuable when blocked — and plan-pricing-safe as
providers begin restricting headless usage.

Every prompt and response is a markdown file under `~/.orcr/runs/<id>/` — the exchange
history of an entire agent tree is a directory you can grep, diff, and archive.

## Requirements

- [herdr](https://herdr.dev) installed and on `$PATH` (or set `herdr.bin` in
  `~/.orcr/config.toml`). orchestratr discovers herdr; it never embeds it.
- The harnesses you want to orchestrate (`claude`, `codex`, `pi`, `opencode`), installed
  and authenticated.

## Install

```sh
cargo install --path .        # builds `orcr`, `orchestratr`, `orcr-mock-agent`
```

## Give your agents the skill

Install `skill/SKILL.md` into any harness (e.g. as a Claude Code skill or equivalent) —
that harness can then spawn, steer, and supervise subagents on every other harness.

## SDKs

Thin typed wrappers over `orcr … --json` for dynamic workflows:
[`sdk/ts`](sdk/ts) (npm, Node ≥ 18) and [`sdk/python`](sdk/python) (Python ≥ 3.9).

## Documentation

- [spec/](spec/) — the full design contract (architecture, CLI, execution model,
  agent-integration contract, jobs, testing)
- [spec/todo.md](spec/todo.md) — live implementation tracker

## Development

```sh
cargo test                                       # unit + fake-herdr suites
ORCR_E2E=1 cargo test --tests -- --test-threads=1 # e2e (needs real herdr; uses the
                                                  # bundled mock agent, isolated sessions)
cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check
```

Adding a harness integration: one module in `src/profile/` implementing the `Profile`
trait — see [spec/05-agents.md](spec/05-agents.md).

Status: v1 feature-complete (M0–M3); remote hosts, presets, and a plugin system for
out-of-tree integrations are on the [roadmap](spec/10-roadmap.md).
