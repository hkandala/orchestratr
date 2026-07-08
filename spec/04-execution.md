# 04 · Execution model

## Run modes

| mode | what runs | when |
| --- | --- | --- |
| `tui` (default) | full interactive harness in a herdr pane: launch → startup recipe → deliver prompt → await completion → capture response | always, unless opted out |
| `exec` | headless `-p`-style invocation, still inside a herdr pane, registered to herdr via `pane report-agent` with orcr as source | opt-in: API billing, ultra-short tasks, flaky TUIs |

## Decided defaults (v1)

- **Permissions: bypass-all.** Every harness gets its dangerous/bypass flag (05).
  `--read-only` and finer policies are future work (10).
- **Auto-close.** After the first completed turn (no `--keep`): capture response, close
  pane, status `done`. Kept agents get an idle reaper (config `idle_reap_min`).
- **Model/effort per invocation** on every agent-launching verb, mapped per harness.

## Env contract

Injected at launch via `herdr agent start --env`:

```
ORCR_ID=a7            ORCR_PARENT=a3 (unset at root)   ORCR_DEPTH=2
ORCR_STORE=~/.orcr    ORCR_OUT=~/.orcr/runs/a7/001-response.md
```

Any `orcr run` inside such an env auto-records lineage. `--parent <id>` overrides.
Fallback attribution chain: `--parent` → `ORCR_*` env → root. Admission control enforces
`max_depth` and `max_agents_per_tree` with clear errors (not skill-level politeness).
Cycles (`--reuse` targeting an ancestor) are detected and refused.

## Run directory contract

```
~/.orcr/runs/               # FLAT — one dir per agent, keyed by id (a7); no nesting.
  a7/                       # lineage lives in sqlite; `out --recursive`/`tree` give
    meta.json               # the hierarchical view on demand.
    001-prompt.md           # turn 1: canonical prompt (inline -p text written here,
    001-prompt.2.md         #   --prompt-file copied here)
    001-response.md         # turn 1: exactly ONE response
    002-prompt.md  002-response.md
```

- **Preamble.** orcr appends to every delivered prompt (exact wording):
  *"When you are completely finished, write your full final answer as markdown to the
  file: `<absolute response path>`. Do not consider the task done until that file is
  written."*
- **Send semantics.**
  - agent `working` → **steer**: persist as `NNN-prompt.K.md` (K=2,3…); still one
    `NNN-response.md`; completion = next stable working→idle after the LAST input.
  - agent `idle` (kept) → **new turn** N+1.
- **Response guarantee.** On completion, if the agent didn't write the file: fill from the
  harness transcript adapter, else pane scrape (`pane read --source recent-unwrapped
  --lines 1000`); record `response_source` (file|transcript|scrape) in db + meta.json.
  After `done` the file ALWAYS exists.
- meta.json mirrors the agent row (id, name, parent, harness, model, status, timestamps,
  pane ref, turns) for greppability without sqlite.

## herdr driver rules (hard-won; do not violate)

1. Discovery: config `herdr.bin` → `$ORCR_HERDR_BIN` → `$PATH`; missing → friendly error
   + https://herdr.dev + exit 2. Never embed or install herdr.
2. All calls shell the CLI and parse the JSON envelope `{"result":…}` / `{"error":…}`.
3. **pane_id is the only stable handle.** Never poll or address by agent name after
   launch (`agent send`/name targets can drop while the pane lives).
4. Input = `pane send-text <pane_id> <text>` → sleep ~1s → `pane send-keys <pane_id>
   enter`. Two calls, never one.
5. Completion = `working` observed at least once, THEN `idle`. A first `idle` without
   prior `working` is NOT completion (profiles may add a fast-turn grace, e.g. OpenCode
   accepts idle after 5s).
6. `blocked` → status blocked; for `--wait` callers exit 4. Emit event + herdr
   notification.
7. herdr `--timeout` values are **milliseconds**.
8. One owned herdr session (config `herdr.session`, default `orcr`) per host. Never touch
   the user's default session unless `--session` says so. gc/reconciler diffs sqlite ↔
   herdr reality (kills unknown orcr-tagged panes, marks vanished agents `lost`, deletes
   stopped session registrations).
9. Kill = per-profile graceful recipe with ~5s deadline, then `pane close`.
10. Feature-detect the installed herdr at startup (`status --json` protocol/version);
    docs describe newer surfaces (socket events, `api schema`) that the installed binary
    may lack — target the binary, not the docs.

## Concurrency & quota

- Caps: global `max_concurrent` (config), future per-harness/per-host. Over-cap spawns
  enter `queued` (FIFO admission by the spawning process; daemon takes over when running).
- Quirk profiles should recognize harness rate-limit screens (treated as `blocked` in v1;
  backoff/reroute policies are future work).
- Per-job budget knobs: `--max-runs`, `--max-duration` (M2).
