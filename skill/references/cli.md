# orcr CLI reference

Every server-touching verb maps 1:1 to a socket method and accepts `--json` (one envelope on
stdout). Naming is mandatory on `run`/`ask` — pass `--name` (one segment, lands in your scope)
or `--path` (relative to scope; leading `/` = absolute; last segment = the name).

## agent run

```sh
orcr agent run --name <name> [-a <provider>] [-p <prompt>] [--gc auto|immediate|never] \
               [--model <m>] [--effort <e>] [--cwd <dir>] [--timeout <dur>]
orcr agent run --path <path> ...          # e.g. --path "review/fanout/file_1"
```

- Prints `<path> <uuid>` (human) / `{agent:{uuid,path,status,agent,managed,cwd,data_dir,…}}`
  (`--json`). Returns immediately — the agent runs in the background.
- `-p -` reads the prompt from stdin.
- `--gc immediate` = one-shot (ends on turn complete); `--gc never` = keep it alive to `send`
  to; `--gc auto` (default) parks then reaps on idle.
- `--timeout <dur>` (e.g. `20m`) kills the agent after that duration. Durations need units.

## agent ask

```sh
orcr agent ask --name <name> [-a <provider>] -p <prompt> [--model] [--effort] [--cwd] [--timeout]
```

Sugar for `run --gc immediate` → `wait` → last response. Prints the response text on stdout
(`--json`: `{uuid, path, response:{text,final}}`). Naming rules identical to `run`.

## agent wait

```sh
orcr agent wait <pattern|uuid>... [--timeout <dur>]
```

Blocks until every matched agent settles (turn complete | blocked | ended). Exit code encodes
the outcome (see below). A wait `--timeout` gives a partial result and exit 3 — it is never a
`timeout` *error* (that is an agent's own deadline).

## agent logs

```sh
orcr agent logs <path|uuid> [--last-response] [--tail <n>] [--follow]
```

Reads the agent's native transcript. `--last-response` prints only the final assistant message
(fails loudly with `transcript_unavailable` if none). `--follow` keeps streaming.

## agent send

```sh
orcr agent send <path|uuid> "<prompt>"      # or: -p <prompt>, or `-` for stdin
```

Delivers a prompt to an existing agent (exact target — no wildcards). Re-arms it to `working`.

## agent kill

```sh
orcr agent kill <pattern|uuid>... [--force] [-y]
```

Kills matched agents (graceful per-provider shutdown, pane closed). `--force` is required to
kill an **unmanaged** agent. `-y` skips the TTY confirmation. Quote patterns.

## agent ls

```sh
orcr agent ls [<pattern>] [-a <provider>] [--status <s>] [--managed|--unmanaged] [--all]
```

Flat rows of active agents (`--all` includes ended history). `--status blocked` surfaces the
"needs a human" queue.

## loop (durable cron)

```sh
orcr loop create <name> "<cron>" [--once-at <dur|ts>] [--max-concurrency <n>] \
                 [--overlap queue|skip] [--timeout <dur>] -- <argv>...
orcr loop pause <name>...      orcr loop resume <name>...
orcr loop rm <name>... [--kill-active] [-y]
orcr loop ls [--all]          orcr loop logs <name> [--run <id>] [--source orcr|command] [--tail] [--follow]
orcr loop run start <name>            # fire once now (works on paused loops)
orcr loop run stop <name> [<run_id>] [-y]
orcr loop run ls <name> [--all]
```

The command after `--` is an argv array (executed directly, no shell). A loop's cwd is its
creation cwd; loop scripts live in `~/.orcr/workflows/<name>/` and are invoked by absolute path.
See `loops.md`.

## scaffold

```sh
orcr scaffold [<dir>]      # generates package.json + tsconfig.json + workflow.ts, runs npm install
```

Requires Node ≥ 20. Purely local (no server). Never overwrites. See `sdk.md`.

## server / api / top

```sh
orcr server status | start [--foreground] | stop | logs [--tail] [--follow] | enable | disable
orcr api schema [--output <file>] | snapshot
orcr top [<pattern>] [-a <provider>] [--status <s>] [--managed|--unmanaged] [--loops]
```

## Exit codes (§6.1, §13)

| code | meaning                                                              |
| ---- | ------------------------------------------------------------------- |
| 0    | success / `wait`: turn complete                                     |
| 1    | invalid_request · transcript_unavailable · server_error            |
| 2    | integration_missing · environment_error                            |
| 3    | timeout (an agent's/run's own deadline; also `wait --timeout`)      |
| 4    | blocked (an agent needs a human)                                   |
| 6    | not_found                                                          |
| 7    | state_conflict (wrong state; `force_required` for unmanaged kills)  |
