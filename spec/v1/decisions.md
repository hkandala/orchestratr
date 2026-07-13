# Design review adjudication — CLI/API (July 2026)

A full external design review (codex, gpt-5.5) of the CLI surface produced 24 findings.
Full text: not stored in-repo (throwaway); this file records the accepted changes and the
rejections with rationale. Spec files 02/03/04/06 reflect the accepted outcomes.

## Accepted (changes applied to spec)

| # | Change |
| --- | --- |
| 2 | New exit code `7` + JSON error code `state_conflict` for wrong-lifecycle operations (steering an idle agent, resuming a fired one-shot without `--at`, …). `6` = unknown id only. |
| 3 | Per-command JSON result schemas specified in 03. Under `--json`, stdout is exactly one JSON object; logs/progress → stderr; `events --follow --json` is the NDJSON exception. |
| 4 | Canonical automation path is `run` → `wait` → `out` (the skill teaches this). `--wait` stays as convenience with a rich stable JSON shape (never a bare string). |
| 6 | Long flag is `--harness` (self-evident); `-a` kept as the short alias. Docs/skill prefer `--harness` on first use. |
| 7 | `run --reuse` REMOVED from v1. Multi-turn = `send` to a kept idle agent. |
| 8 | `out --path/--paths` replaced by `--format body|path|json` (default body; `--format path` + `--recursive` gives `id<TAB>name<TAB>path` lines). |
| 9 | `a7:t2` stays as interactive sugar only; skill teaches `--turn N`. |
| 10 | Typed-id patterns (`[alsgw]\d+`, `*:t\d+`) reserved — invalid as names; ambiguous positional → error with hint. |
| 11 | `wait` gains `--tree <id>` (all descendants); timeout JSON includes completed/pending/blocked arrays. `--count/--fail-fast` deferred. |
| 12 | Durations accept suffix strings everywhere (`20m`, `1200s`, `30d`); bare number = seconds. Milliseconds never appear in the user CLI. |
| 13 | Lifecycle verbs defined: `kill` stops execution (graceful→forceful, agents + jobs); `job rm`/`schedule rm` delete definitions. Documented distinction. |
| 14 | Uniform job management surface: `orcr job ls|show|pause|resume|rm <id>` across l/s/g/w. Short creation verbs stay (`loop`, `goal`, `schedule add`, `workflow run`). |
| 15 | Jobs are ALWAYS daemon-supervised (creation auto-starts `orcr serve`). `--detach` dropped; `loop --foreground` for experimental non-durable loops. |
| 16 | Recurring schedules default `--forever` (cron expectation); `--expires` opt-in. One-shots end after firing (re-armable via `schedule resume --at`). |
| 17 | `schedule resume <id> [--at <time>]` spelled out in synopsis; fired one-shot without `--at` → state_conflict. |
| 18 | `--judge-agent` renamed `--judge-harness` (+ `--judge-model`). Same-harness default stays (user decision) but output labels it `self-check` and JSON carries `judge_independent:false`. |
| 19 | New verb: `orcr show <id> [--json]` — canonical single-object state (status, turns, children, paths, timings, model, cwd, last error). Skill teaches it as THE state query. |
| 20 | `history` gains `--status`, `--parent`, `--limit`. |
| 21 | `top --follow` dropped (top is always live); `--pane` documented as "open inside a herdr split". |
| 22 | `serve --install` → `orcr serve install` subcommand (M4), documented as installing the orcr user service, never herdr. |
| 23 | `--prompt-file -` reads stdin. Exactly one of `-p` / `--prompt-file` enforced. |
| 24 | `events --follow --json` = NDJSON (one event object per line: `type`, `id`, `time`, `payload`); one-shot `events --json` = single envelope. |

## Partially accepted

| # | Outcome |
| --- | --- |
| 1 (state-dependent `send`) | `send` remains ONE verb (deliberate design: mirrors how harnesses treat mid-run input). Determinism added for scripts/agents: `send --steer` and `send --turn` intent flags → `state_conflict` (exit 7) on mismatch; bare `send` resolves by live state and its JSON reports `"mode":"steer"|"turn"`. The skill teaches intent flags. |
| 5 (bypass-all default) | Kept for v1 — explicit user decision (subagents typically run in worktrees; friction kills the core use case). Mitigations: every spawn's JSON carries `"permissions":"bypass"`; first-run TTY notice; `--read-only` remains next-in-line future work. Revisit at v1.1. |

## Rejected

| # | Rationale |
| --- | --- |
| Rename/remove `send` entirely | Conflicts with the product's steering-first identity; intent flags give scripts the same safety. |
| Require `--judge-harness` on `goal` | Extra required flag for the common case; `self-check` labeling makes the tradeoff visible instead. |
