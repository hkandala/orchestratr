# orchestratr — Implementation Spec (v1)

orchestratr (CLI alias **`orcr`**) is a Rust CLI that turns **herdr** into a cross-harness
agent orchestration layer: spawn, steer, await, inspect, and kill coding agents (Claude Code,
Codex, Pi, OpenCode) running as real interactive TUIs inside background herdr panes, plus
higher-order jobs: `loop`, `schedule`, `goal`, `workflow`, a live tree TUI (`orcr top`), and a
skill that teaches any harness the vocabulary.

One binary, two names: `orchestratr` and `orcr` (identical). Daemon mode is `orcr serve`.

## Hard requirements

- **herdr is an external dependency, never embedded.** Discovery order: config `herdr.bin` →
  `$ORCR_HERDR_BIN` env → `$PATH`. If not found: print a friendly error pointing to
  https://herdr.dev install instructions and exit with code 2. Never attempt to install it.
- All herdr interaction is via shelling the `herdr` CLI and parsing its JSON output
  (envelope: `{"id":…,"result":{…}}` or `{"id":…,"error":{"code":…,"message":…}}`).
  No socket protocol in v1.
- Rust 2021+, rustfmt default style, `cargo clippy --all-targets -- -D warnings` must pass.
- Popular crates only: clap 4 (derive), serde/serde_json, toml, rusqlite (bundled feature),
  uuid (v4), anyhow + thiserror, chrono, dirs, tracing + tracing-subscriber, tempfile /
  assert_cmd / predicates (dev), ratatui + crossterm (M3), a maintained cron crate (M2).

## Store layout

Everything under `~/.orcr/` — overridable via `ORCR_STORE` env (tests rely on this):

```
~/.orcr/
  config.toml        # user config (herdr-style TOML)
  orcr.db            # sqlite, WAL mode
  runs/<uuid>/       # FLAT run dirs, one per agent (no nesting; lineage in db)
    meta.json
    001-prompt.md    001-prompt.2.md (steer addendum)   001-response.md
    002-prompt.md    002-response.md
  logs/
```

### config.toml (all keys optional, sane defaults)

```toml
[defaults]
agent = "claude"          # default harness
model = ""                # empty = harness default
effort = ""
timeout_s = 600
keep = false

[limits]
max_depth = 3
max_agents_per_tree = 10
max_concurrent = 4
idle_reap_min = 15

[herdr]
bin = ""                  # empty = $PATH lookup
session = "orcr"          # the single owned herdr session name

[viewer]
auto = true               # auto-open `orcr top --pane` when spawning from inside herdr
```

### sqlite schema (schema_version pragma/user_version = 1; refuse mismatched versions)

```
agents:  id TEXT PK (uuid), name, parent_id, kind ('tui'|'exec'), harness, model, effort,
         host, herdr_session, pane_id, terminal_id, cwd, worktree,
         status, exit_reason, keep INT, timeout_s, created_at, ended_at,
         run_dir, agent_session_kind, agent_session_value
jobs:    id TEXT PK (uuid), type ('loop'|'schedule'|'goal'|'workflow'), spec_json, status,
         tz, next_run_at, expires_at, runs_count, created_at, ended_reason
turns:   agent_id, n, prompt_paths TEXT (json array), response_path, response_source
         ('file'|'transcript'|'scrape'), started_at, ended_at, tokens_in, tokens_out
events:  seq INTEGER PK AUTOINCREMENT, ts, kind, ref_id, payload_json
```

## Status model

`queued → starting → working → (idle ⇄ working)* → done | failed | timeout | killed | lost`
plus `blocked` (from herdr) reachable from starting/working. Auto-close default: after the
first completed turn (no `--keep`), capture response, close pane, status `done`.

## Exit codes (CLI-wide)

0 ok · 2 environment/config error (e.g. herdr missing) · 3 timeout · 4 blocked ·
5 killed · 6 not found · 1 anything else.

## Env contract (injected via `herdr agent start --env`)

```
ORCR_ID=<uuid>  ORCR_PARENT=<uuid|unset>  ORCR_DEPTH=<n>  ORCR_STORE=<path>
ORCR_OUT=<store>/runs/<uuid>/001-response.md
```

Any `orcr run` executed inside such an environment auto-records lineage (`--parent` flag
overrides; depth/agents-per-tree limits enforced at admission → clear error).

## Run-dir + prompt/response contract

- Prompt input: `-p/--prompt` inline XOR `--prompt-file <path>`. Canonical copy always
  persisted as `NNN-prompt.md` (inline text written; file copied).
- orcr appends a short preamble (≤2 sentences) to every prompt it delivers:
  "When you are completely finished, write your full final answer as markdown to the file:
  <absolute response path>. Do not consider the task done until that file is written."
- **Send semantics:** `orcr send <id> <prompt>` — if the agent is `working`, this is a STEER:
  persist as `NNN-prompt.K.md` (K=2,3…) for the current turn N; still exactly ONE
  `NNN-response.md` expected; the turn tracker must treat completion as the next stable
  working→idle after the LAST input. If the agent is `idle` (kept), it starts turn N+1 with
  fresh `NNN+1-prompt.md`.
- **Response guarantee:** on turn completion, if the agent didn't write the response file,
  orcr fills it via the harness transcript adapter, else via pane scrape
  (`herdr pane read <pane> --source recent-unwrapped --lines 1000`), and records
  `response_source` in db + meta.json. After `done`, the file ALWAYS exists.
- `orcr out <id>`: print latest response; `--turn N`; `--path` (print path only); `--json`;
  `--recursive [--paths]` walks descendants depth-first printing each agent's id/name header
  and response (or path).

## CLI surface (v1 target; M-phases below)

```
orcr run   -a <harness> [-p <text> | --prompt-file <f>] [--name] [--model] [--effort]
           [--cwd] [--timeout <s>] [--keep] [--mode tui|exec] [--worktree]
           [--parent <id>] [--reuse <id>] [--session <name>] [--wait] [--json]
orcr send <id> <text | --prompt-file f> [--wait] [--json]
orcr wait <id...> [--any] [--timeout <s>] [--json]
orcr out  <id> [--turn N] [--path] [--recursive] [--paths] [--json]
orcr ps [--json]          orcr tree [--watch] [--json]
orcr kill <id...> [--tree] [--json]
orcr attach <id>
orcr status [--json]      # herdr found? version? session running? daemon running? db ok?
orcr history [--since] [--name] [--harness] [--json]
orcr gc [--dry-run] [--json]
orcr loop / schedule / goal / workflow / top / events / serve   (M2/M3)
```

IDs are uuids; every command accepts unambiguous uuid prefixes or `--name` values.
Human output on TTY; `--json` gives stable envelopes `{"ok":true,"result":…}` /
`{"ok":false,"error":{"code":…,"message":…}}`.

## herdr driver notes (hard-won; follow exactly)

- Launch: `herdr agent start <name> --cwd <dir> --env K=V … --no-focus -- <argv…>` inside the
  owned named session (`herdr --session orcr …` CLI form; verify exact invocation against
  `herdr --help` at dev time). Parse and store `pane_id` — it is the ONLY stable handle.
- Never use `herdr agent send` / agent-name targets for polling or input (names can drop).
  Use `pane send-text <pane_id> <text>`, sleep ~1s, then `pane send-keys <pane_id> enter`.
- Poll `pane get <pane_id>` (500–1000 ms) for `agent_status`; completion = observed `working`
  at least once, then `idle`. A first `idle` without prior `working` is NOT completion —
  except profiles with a fast-turn grace (OpenCode: accept idle after 5 s grace).
- `blocked` → status blocked (emit event; in v1 this is an error state for --wait callers,
  exit 4). `herdr wait agent-status`/`wait output --match` may be used where convenient
  (timeouts are in MILLISECONDS).
- Startup prep before first prompt (per-profile "startup recipe", screen-scrape based):
  Codex update menu → send "2" + enter; OpenCode update modal → Escape ×2.
- Kill: graceful per-profile recipe first (with deadline ~5 s), then `pane close <pane_id>`.
- The reference prototype at `~/code/herdr-wrapper` (Rust) may be READ for herdr invocation
  patterns and transcript parsing details, but NO code is copied; this repo is clean-room.

## Harness profiles (trait `Profile`)

Per harness: `launch_argv(model, effort, bypass) -> Vec<String>`, `startup_recipe`,
`completion: StatusTransition | StatusWithGrace(ms) | OutputMarker{…}`, `shutdown_recipe`,
`transcript_adapter`.

- claude:   `claude --dangerously-skip-permissions [--model M] [--effort E]`;
            transcript `~/.claude/projects/**/<session_id>.jsonl` (last assistant text).
- codex:    `codex --dangerously-bypass-approvals-and-sandbox [--model M]
            [-c model_reasoning_effort="E"]`; transcript `~/.codex/**/*<session_id>*.jsonl`
            (task_complete.last_agent_message → agent_message → response_item output_text).
- pi:       `pi [--model M] [--thinking E]`; transcript `~/.pi/agent/sessions/**/*.jsonl`.
- opencode: `opencode [--model M]`; completion StatusWithGrace(5000);
            transcript via `opencode export <session_id>`.
- mock:     see below; completion OutputMarker.

## Mock agent + testing strategy

`orcr-mock-agent` — a second bin target in this workspace, a scriptable stand-in TUI agent:

- On start: prints `MOCK_READY`, then loops reading lines from stdin.
- On receiving a prompt line: prints `MOCK_WORKING`, parses the response-file path out of the
  orcr preamble, honors inline directives embedded anywhere in the prompt:
  `[[sleep:<ms>]]` delay before finishing; `[[ignore-out]]` skip writing the response file
  (exercises the fallback chain); `[[block]]` print `MOCK_BLOCKED` and stall until next input;
  `[[exit]]` terminate. Steering input received while "working" is appended into the same
  pending response.
- Default behavior: write `# mock response\n<echo of received prompt(s)>` to the response
  path, then print `MOCK_DONE <turn-counter>`.
- The mock profile's completion strategy watches pane output markers (`MOCK_DONE`,
  `MOCK_BLOCKED`) via `herdr wait output` / pane reads — no herdr detection manifests needed.

Tests:
- **Unit tests**: pure logic (argv builders, preamble/paths, steer-vs-turn state machine,
  db ops via tempdir store, config parsing, id-prefix resolution, transcript parsers with
  fixture files). A fake `herdr` shim (test fixture shell script emitting canned JSON,
  prepended to PATH) covers driver logic without herdr.
- **E2E tests** (`tests/e2e_*.rs`, gated behind env `ORCR_E2E=1`, serial): use the REAL
  installed herdr with the mock agent, an isolated session name `orcr-e2e-<rand>`, and
  `ORCR_STORE=<tempdir>`. MUST clean up (session stop + delete) via a drop guard even on
  panic. Never touch the user's default herdr session or real agents in CI-style runs.

## Milestones & acceptance

- **M0**: workspace skeleton, config + store + run-dir modules, herdr discovery + driver,
  profiles, mock agent, fake-herdr unit suite, e2e harness boots mock agent and round-trips
  one prompt→response through real herdr.
- **M1**: run/send/wait/out/ps/tree/kill/attach/status/gc/history(basic) + env contract +
  steer semantics + fallback chain + --json/exit codes. E2E: fan-out 2 mocks, steer one
  mid-turn, out --recursive, kill --tree.
- **M2**: serve (auto-start daemon, pidfile), events, loop (--every|auto|--tick-on|--max|
  --until, prompt-file re-read per tick), schedule (cron + --at, --catchup, --expires,
  tz-aware), reconciler/gc, concurrency caps + queued state.
- **M3**: top TUI (+ --pane auto-viewer), goal (judge defaults to worker harness+model;
  --judge-agent/--judge-model), workflow run (tracked node, --on-orphan kill|keep), full
  history, SKILL.md, token telemetry from transcripts.

Commit style: conventional commits (`feat:`, `fix:`, `test:`, `chore:`), small and focused.
