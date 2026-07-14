# Loops (durable cron)

A loop runs a command on a schedule, durably (it survives server restarts). Use a loop for
recurring or unattended work; use plain agents for one-shot tasks. A loop **is a reusable
workflow** — its script lives in `~/.orcr/workflows/<name>/` (scaffold it), and its `data/<loop>/`
dir holds only runtime state.

## Create

```sh
orcr loop create <name> "<cron>" [--once-at <dur|ts>] [--max-concurrency <n>] \
                 [--overlap queue|skip] [--timeout <dur>] -- <argv>...
```

- `<name>` is one root-level segment. The command after `--` is an argv array (executed
  directly — no shell).
- Cron is five fields `min hour dom month dow` (quote it): `*/30 * * * *` (every 30 min),
  `0 9 * * 1-5` (09:00 weekdays), `0 * * * *` (hourly). Cadence is evaluated in the creating
  timezone (DST-correct). `create` echoes the cadence in words + the next local/UTC fire.
- `--once-at 30m` / `--once-at 2026-07-14T09:00` fires exactly once (no cron).
- `--timeout <dur>` kills a run after that duration. Always set one for unattended loops.

## Overlap policy (at capacity)

`--max-concurrency` bounds concurrent runs (default 1). When a fire arrives at capacity:

- `--overlap queue` (default): coalesce — one pending run is held and fires when a slot frees
  (later fires while pending fold into it; missed cron fires are skipped-and-logged, never
  replayed).
- `--overlap skip`: drop the fire.

## Manage

```sh
orcr loop ls [--all]                 orcr loop pause <name>    orcr loop resume <name>
orcr loop run start <name>           # fire once now (works even while paused)
orcr loop run stop <name> [<run_id>] # stop one run (+ glob-kills its <loop>/<run_id>/** agents) or all
orcr loop run ls <name> [--all]
orcr loop logs <name> [--run <id>] [--source orcr|command] [--follow]
orcr loop rm <name> [--kill-active]  # remove the definition (its script survives)
```

## Self-terminating loops (§9.7)

A loop's run command runs with the §5.3 env contract (`ORCR_ID` = run uuid, `ORCR_PATH` =
`<loop>/<run_id>`, `ORCR_LOOP_DATA_DIR`). It parents agents *inside* the run. A loop can remove
itself when its work is done:

```ts
const ctx = orcr.context.fromEnv();
if (ctx.kind !== "loopRun") throw new Error("must run under an orcr loop");
await workOneItem();                          // agents land under <loop>/<run_id>/…
if (queueSize() === 0) await orcr.loop.rm(ctx.loop!.name);   // self-terminate
```

**Stop condition rule:** every loop needs a stop condition **with a number in it** ("0 items
left", "max 20 runs") — never "until it's done". Pair it with `--timeout` so a stuck run can't
run forever.
