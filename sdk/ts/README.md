# orchestratr (TypeScript SDK)

Thin typed wrapper around the [`orcr`](https://github.com/hkandala/orchestratr) CLI —
agent orchestration over herdr. Every call shells `orcr … --json` via `child_process`
and parses the JSON envelope; the CLI is the contract and the SDK never gains private
capabilities. Zero runtime dependencies. Node >= 18.

```sh
npm install orchestratr
```

The `orcr` binary must be on `PATH` (or set `ORCR_BIN`).

## Fan-out review & merge

```ts
import { orcr } from "orchestratr";

const task = "refactor auth middleware to support API keys";
const runs = await Promise.all([
  orcr.run({ harness: "claude", prompt: task, worktree: true, name: "claude-try" }),
  orcr.run({ harness: "codex", prompt: task, worktree: true, name: "codex-try" }),
  orcr.run({ harness: "opencode", prompt: task, worktree: true, name: "oc-try" }),
]);
await orcr.wait(runs, { timeoutS: 1800 });
const summaries = await Promise.all(runs.map((r) => r.out()));
const judge = await orcr.run({
  harness: "codex",
  wait: true,
  prompt: `Pick the best of these three attempts and merge improvements:\n${summaries.join("\n---\n")}`,
});
console.log(judge.text);
```

## Surface

```ts
run(opts): Promise<Handle>        // opts mirror CLI flags; Handle = {id, text, wait, out, send, kill}
send(id, prompt, {steer?, turn?, promptFile?, wait?})
wait(ids, {any?, tree?, timeoutS?})  // fan-in; ids may be Handles
out(id, {turn?, recursive?, paths?}) // -> OutItem[] with response-file paths
show(id) / kill(id, {tree?}) / ps() / tree() / history({since?, harness?, …})
events(onEvent)                   // streams `orcr events --follow --json` NDJSON; returns {close()}
```

Errors are typed from the CLI exit-code table: `EnvConfigErr` (2), `TimeoutErr` (3),
`BlockedErr` (4), `KilledErr` (5), `NotFoundErr` (6), `StateConflictErr` (7), with the
raw envelope attached as `details`.

## Development

```sh
npm install
npm run build   # tsc -> dist/
npm test        # node --test; skips cleanly when orcr is absent
```
