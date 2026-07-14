# orcr TypeScript SDK + scaffolding

The SDK (`@orchestratr/sdk`) is a typed client of the socket API — anything the CLI can do, the
SDK can do (`orcr.gen` is the generated 1:1 protocol client; the helpers below are the curated
layer). Use it when a workflow needs real control flow: branching, retries, fan-out over a
computed list, or a loop's script.

## When (and where) to scaffold

The CLI is for one or two agents. The moment you need control flow, scaffold a project:

```sh
orcr scaffold <dir>          # writes package.json + tsconfig.json + workflow.ts, runs npm install
cd <dir> && npx tsx workflow.ts
```

Requires **Node ≥ 20**. Where the project lives is convention:

- one-time, task-specific script → `$ORCR_AGENT_DATA_DIR/workflows/` (disposable, auditable)
- reusable — and **every loop's script** → `~/.orcr/workflows/<name>/`

It's a plain npm project: `npm install` any dependency the task needs (a GitHub client, a CSV
parser). A loop's cwd stays the workspace, so a loop script is invoked by absolute path
(`<project>/node_modules/.bin/tsx <project>/x.ts`); Node resolves its imports from the project's
own `node_modules` by walking up from the script file.

## Core surface

```ts
import { orcr } from "@orchestratr/sdk";

// spawn — returns a handle immediately (naming is mandatory: name OR path)
const a = await orcr.agent.run({ agent: "codex", name: "worker", prompt: "…",
                                 gc: "never", model, effort, cwd, timeout });
a.uuid; a.path; a.name; a.dataDir;      // dataDir === $ORCR_AGENT_DATA_DIR
await a.wait({ timeout });               // settles: turn complete | blocked | ended
await a.send("…");                       // steer it
await a.logs({ tail });                  // → entries
for await (const e of a.followLogs()) { … }
await a.lastResponse();                  // → string (throws TranscriptUnavailable)
await a.kill();

// collections take §5.1 patterns (CLI-identical), relative to the current scope
await orcr.agent.wait("fanout/*", { timeout });
await orcr.agent.ls({ pattern, agent, status, managed, all });
await orcr.agent.kill("review/**", { force });   // no interactive confirm in the SDK

// the one-liner: run(gc:immediate) → wait → lastResponse
const answer = await orcr.ask({ agent: "claude", name: "quick_check", prompt: "…" });

// scopes — AsyncLocalStorage (NOT process-global); nest to stack prefixes; `/` = absolute
await orcr.scope("review", async (sc) => {
  await orcr.agent.run({ path: "fanout/file_1", … });   // → review/fanout/file_1
  await orcr.agent.wait("fanout/*");                     // → review/fanout/*
  await orcr.scope("phase_1", async () => {
    await orcr.agent.run({ name: "worker", … });         // → review/phase_1/worker
  });
}, { killOnThrow: true });   // barrier-kill <scope>/** on throw

// context — never hand-parse ORCR_PATH
const ctx = orcr.context.fromEnv();   // {kind:"agent"|"loopRun"|"root", id?, path?, scope?, dataDir?, parent?, loop?}

// live events — snapshot-then-subscribe (what `orcr top` renders)
const w = await orcr.watch({ pattern, agent, status, managed, sinceSeq });
console.log(w.snapshot, w.snapshotSeq);
for await (const ev of w) { /* typed events */ }

// loops
await orcr.loop.create({ name: "burn_down", cron: "*/30 * * * *", timeout: "25m",
                         maxConcurrency, overlap, command: [`${wf}/node_modules/.bin/tsx`, `${wf}/x.ts`] });
const run = await orcr.loop.run.start("burn_down");   // → { uuid, path, runId, loop, status, dataDir }
await orcr.loop.run.stop("burn_down", { runId });
await orcr.loop.run.ls("burn_down", { all });
await orcr.loop.ls(); await orcr.loop.logs("burn_down", { run, source });
await orcr.loop.pause("burn_down"); await orcr.loop.resume("burn_down");
await orcr.loop.rm(orcr.loopNameFrom(process.env.ORCR_PATH!));   // self-terminate

// server / api / attach (attach is terminal-mediated — exec the command yourself)
await orcr.server.status(); await orcr.api.snapshot();
const at = await orcr.agent.prepareAttach("review/worker", { takeover: false });
// → { command, leaseId, uuid, path, ttlMs }; exec command, then at.heartbeat()/at.release()
```

## Typed errors

One class per §13 code: `NotFound`, `InvalidRequest`, `StateConflict`, `Blocked`, `Timeout`,
`IntegrationMissing`, `TranscriptUnavailable`, `EnvironmentError`, `ServerError` — each carries
`{ code, message, details }`. Force-required is a `StateConflict` (`err.forceRequired`).

The SDK never prompts — destructive helpers behave like `-y` CLI calls. The socket version
check catches CLI/SDK drift loudly (`EnvironmentError`, cause `unsupported_version`).
