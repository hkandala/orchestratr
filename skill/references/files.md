# The data-dir & file conventions

orcr guarantees a unique, existing data directory per agent/loop/run and hands it to the
context as env (§5.3). Contents are entirely yours — orcr never infers success from files.

## The data tree mirrors the path tree

```
agent  review/fanout/file_1  → ~/.orcr/data/review/fanout/file_1/<uuid>/
                                 launch.json (orcr-written audit)
                                 + whatever the agent writes (response.md, out/, …)
loop   nightly               → ~/.orcr/data/nightly/            (ORCR_LOOP_DATA_DIR)
                                 loop.json · cross-run state the loop keeps
run    nightly/r82c9s        → ~/.orcr/data/nightly/r82c9s/
                                 run.log · run scratch
agent  nightly/r82c9s/triage → ~/.orcr/data/nightly/r82c9s/triage/<uuid>/
```

The uuid leaf means reused paths never collide and every generation stays browsable.
`ORCR_LOOP_DATA_DIR` points at the **loop's** folder — one scratch space shared across all runs
(state that survives run to run lives here); each run's folder and its agents' folders nest
inside it automatically.

## Env contract (injected into every managed pane / loop-run command)

```
ORCR_ID              this agent's uuid (or, in a loop-run command, the run's uuid)
ORCR_PATH            your absolute path, ending in your own leaf (name, or run id)
ORCR_PARENT_ID       the spawning context's uuid (unset at root)
ORCR_PARENT_PATH     the spawning context's path (unset at root)
ORCR_AGENT_DATA_DIR  this agent's data dir (unset in loop-run commands — runs aren't agents)
ORCR_LOOP_DATA_DIR   the loop's shared data dir (set inside loops; unset outside)
```

All values are absolute. Derive context with `orcr.context.fromEnv()` (SDK) — never hand-parse
`ORCR_PATH`. In shell, the loop name is `"${ORCR_PATH%%/*}"`.

## The file convention (guaranteed-format answers)

When a step needs a structured answer, tell the agent **where to write it**, then read and
**validate** the file yourself:

1. **Absolute paths only** — with one allowed exception: `$ORCR_AGENT_DATA_DIR` /
   `$ORCR_LOOP_DATA_DIR`, which the prompt must tell the agent to *expand*: *"expand the
   environment variable ORCR_AGENT_DATA_DIR and write your findings to
   $ORCR_AGENT_DATA_DIR/response.md"*. The caller reads the same path from the handle's
   `dataDir` (`a.dataDir`) / the run's `dataDir`.
2. **A completion sentinel** in the prompt (*"…then say DONE"*).
3. Prefer temp-file + rename when atomicity matters.

`ask()` / `logs --last-response` cover the casual cases via the native transcript — orcr keeps
no response copies, it always reads the provider's own file.

Data-dir cleanup is future work (§17); nothing here is an identity authority — rows and uuids
are.
