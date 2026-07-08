# 02 · Architecture

## Components

```
orcr CLI ──┬── engine (spawn / turn tracking / steer / wait / kill)
           ├── herdr driver (shells `herdr`, parses JSON envelopes)
           ├── profiles (one module per harness — see 05)
           ├── store (sqlite WAL + run dirs)
           ├── daemon `orcr serve` (jobs: loop/schedule/goal/workflow; reconciler)
           └── tui (`orcr top`, tree/history rendering)
```

## Daemon-on-demand (chosen model)

Same binary, two roles — exactly herdr's own client/server shape:

- **Stateless verbs** (`run`, `send`, `wait`, `out`, `ps`, `tree`, `kill`, `attach`,
  `status`, `history`, `gc`) work with **no daemon**: they shell herdr directly and
  read/write sqlite.
- The first command needing standing supervision (`loop`, `schedule`, `goal`, `workflow`
  with background children, `top --follow`) **auto-starts `orcr serve`** (background
  process, pidfile in store, unix-socket ping for liveness).
- Degradation is graceful: daemon dies → stateless verbs still work; jobs pause and are
  reconciled on restart.
- Optional hardening: `orcr serve --install` writes a launchd/systemd unit (M4).
- Single-writer discipline: the daemon is the only writer for **job** state; the CLI is
  the writer for direct spawns. sqlite WAL + busy_timeout handles concurrent readers.

Rejected alternatives, kept for the record: a mandatory dedicated daemon (heavier
lifecycle for the 80% case) and a fully daemonless design with supervisor panes inside
herdr (scheduling reliability chained to the herdr server; no push channel). The hybrid
borrows the daemonless design's nicest trick: supervisors may optionally run visibly
inside herdr panes.

## Store layout

Everything under `~/.orcr/`, overridable via `ORCR_STORE` (tests depend on this):

```
~/.orcr/
  config.toml        # user config (TOML, herdr-style)
  orcr.db            # sqlite, WAL
  runs/<uuid>/       # FLAT run dirs — one per agent; lineage lives in the db (04)
  logs/
  serve.pid  serve.sock
```

### config.toml (all keys optional; defaults shown)

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
bin = ""                  # empty = $ORCR_HERDR_BIN → $PATH lookup
session = "orcr"          # the single owned herdr session name

[viewer]
auto = true               # auto-open `orcr top --pane` when spawning from inside herdr
```

## sqlite schema (user_version = 1; refuse mismatched versions with a clear message)

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

`events` is append-only; it doubles as the audit log and the TUI's change feed
(`orcr events --follow --json` tails it as NDJSON).

## Status model

```
queued → starting → working → (idle ⇄ working)* → done | failed | timeout | killed | lost
                        └→ blocked (recoverable via send/attach)
```

| status | meaning |
| --- | --- |
| queued | admitted, waiting on a concurrency slot |
| starting | pane launched; harness booting; startup recipe running |
| working | herdr reports `working` during an active turn |
| idle | turn complete on a `--keep` agent, awaiting next prompt |
| blocked | herdr reports `blocked` (question/login/limit screen); notify + steerable |
| done | turn finished, response file exists, pane auto-closed (default policy) |
| failed / timeout / killed | terminal errors; `exit_reason` says which |
| lost | pane vanished / herdr unreachable; set by the reconciler |

## Exit codes (CLI-wide)

`0` ok · `2` environment/config error (herdr missing, bad config, version skew) ·
`3` timeout · `4` blocked · `5` killed · `6` not found · `1` anything else.

## Telemetry

herdr exposes no token/cost/last-message fields. Tokens and cost come best-effort from
per-harness transcript adapters (05); last-message snippets come free from response files.
Rolled up per subtree in `tree`/`top`. Absent data degrades to elapsed time + turn count.
