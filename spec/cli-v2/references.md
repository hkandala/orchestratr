# Reference CLI summaries (grounding for reviewers)

## herdr (the substrate orcr shells out to)

Hierarchy: Session → Workspace → Tab → Pane → Agent.
- Session: persistent server namespace (own panes/sockets/state). Default session
  exists; named via `herdr --session <name>`. `session list/attach/stop/delete`.
- Workspace: project container, one per repo/task typically; `workspace create
  [--cwd <path>] [--label] [--env] [--focus|--no-focus]`; sidebar aggregates agent
  attention per workspace.
- Worktree: `worktree list/create/open/remove` bound to workspace (`--branch --base
  --path --label`).
- Tab: addressable layout in a workspace; `tab create [--workspace] [--cwd] [--label]
  [--env] [--focus|--no-focus]`, `tab list/get/focus/rename/close`.
- Pane: real terminal; `pane split --direction right|down [--ratio] [--cwd] [--env]`,
  `pane read [--source visible|recent|recent-unwrapped|detection] [--lines]`,
  `pane send-text`, `pane send-keys`, `pane run`, `pane close/move/swap/zoom/rename`,
  `pane report-agent` (external processes can register agent state).
- Agent: process herdr recognizes in a pane. States: working/idle/blocked/done/unknown.
  `agent list`, `agent get/read/send/rename/focus/attach [--takeover]`,
  `agent wait <target> --status idle|working|... [--timeout <ms>]`,
  `agent start <name> [--cwd] [--workspace] [--tab] [--split right|down] [--env]
  [--focus|--no-focus] -- <argv...>`.
- `wait output <pane> --match <text> [--regex] [--timeout <ms>]`,
  `wait agent-status <pane> --status <s> [--timeout <ms>]`.
- Env inside herdr panes: HERDR_ENV=1, HERDR_PANE_ID, HERDR_TAB_ID,
  HERDR_WORKSPACE_ID, HERDR_SESSION.
- Verb style: noun-first (`herdr <noun> <verb>`), `--json` everywhere, `server` for
  headless (`herdr server`, `server stop`), timeouts in ms.

## paseo (similar goals, but not on a terminal workspace manager — agents are
daemon-managed processes, not attachable TUIs)

- `paseo run <task> [--provider <p>] [--detach] [--worktree <branch>]
  [--output-schema <f>]` — WAITS by default, prints result; --detach for background.
- `paseo ls [-a all] [-g global] [--json]` — lists agents in CURRENT DIRECTORY by
  default; -g for all dirs.
- `paseo attach <id>` — stream output (not a real TUI takeover).
- `paseo send <id> <msg> [--no-wait]` — follow-up task; waits by default.
- `paseo logs <id> [-f] [--tail n] [--filter type]`.
- `paseo wait <id> [--timeout s]`.
- `paseo schedule create --every <dur> --cwd <path>` / `schedule ls/pause`.
- `paseo permit ls|allow|deny` (permission requests surface).
- `paseo daemon start|status|stop`, `daemon pair` (remote), `--host` global.
- ID abbreviation (shorten if unambiguous). --json/--format yaml/-q ids-only.
- Agents spawn sub-agents by invoking the same CLI. cwd-scoped listing is a core
  ergonomic: `ls` shows "agents working here".

## orca (heavier orchestration layer: coordinator/worker messaging)

- `orca orchestration send --to <handle|@group> --type status|dispatch|worker_done|
  escalation|decision_gate|heartbeat --task-id --dispatch-id` — an inbox/messaging
  model between agents; group addresses @all/@idle/@codex.
- `orca orchestration task-create/task-list/task-update` — shared task board with
  statuses pending/ready/dispatched/completed/failed/blocked.
- `orca orchestration dispatch --task <id> --to <worker> --inject` — injects a
  preamble telling the worker how to report back (task-id + dispatch-id = completion
  authority).
- `orca orchestration ask --to <coordinator> --question --options --timeout-ms` —
  blocking question, returns .answer.
- `orca orchestration run --spec --max-concurrent --worktree active` — managed
  coordinator loop.
- Decision gates: gate-create/gate-resolve/gate-list.
- Takeaway: orca builds task/inbox/gate PRIMITIVES so coordination is CLI-buildable;
  heavier than orcr wants, but shows what agent-to-agent needs beyond spawn/wait.

## orcr v1 (what we're simplifying FROM)

Verbs: run(async default)/send/wait/out/show/ps/tree/kill/attach/status/history/gc +
loop/schedule/goal/workflow + job ls/show/pause/resume/rm + top/events/serve.
Dedicated hidden herdr session "orcr" per host. Auto-viewer top pane on spawn from
inside herdr. Ids a/l/s/g/w<N>. Exit codes 0/2/3/4/5/6/7. Run-dir contract
(NNN-prompt.md/NNN-response.md + preamble + file→transcript→scrape guarantee). Env
contract ORCR_ID/PARENT/DEPTH/STORE/OUT. Completion = herdr working→idle transition.
Implemented in Rust, ~8k lines, 76 unit + 22 e2e tests. Steer-vs-turn send semantics
with intent flags and exit 7 conflicts.
