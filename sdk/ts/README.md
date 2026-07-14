# @orchestratr/sdk

The TypeScript client for [orchestratr (`orcr`)](https://github.com/) — a cross-provider
orchestrator for AI coding agents. The SDK is a first-class client of the orcr socket API:
anything the CLI can do, the SDK can do, with typed methods, typed errors, and client-side path
scoping that composes exactly like the CLI.

## Requirements

- Node ≥ 20.
- The `orcr` binary on your PATH (with a running herdr + provider integrations). The SDK
  auto-starts the server on first use.

## Quickstart

The fastest way to a runnable project is `orcr scaffold`, which writes `package.json`
(pinning `@orchestratr/sdk` to the CLI's own version), `tsconfig.json`, and a runnable
`workflow.ts`, then runs `npm install`:

```sh
orcr scaffold my-workflow && cd my-workflow
npx tsx workflow.ts
```

```ts
import { orcr } from "@orchestratr/sdk";

// Fan out reviewers under a named scope, wait for all to settle, then read each result.
await orcr.scope("review", async () => {
  const files = ["src/auth.ts", "src/db.ts"];
  const reviewers = await Promise.all(
    files.map((f, i) =>
      orcr.agent.run({
        agent: "claude",
        path: `fanout/file_${i}`,
        gc: "immediate",
        prompt: `Review ${f}. Write findings to $ORCR_AGENT_DATA_DIR/response.md, then say DONE.`,
      }),
    ),
  );
  await orcr.agent.wait("fanout/*");
  for (const r of reviewers) {
    // read `${r.dataDir}/response.md` …
  }
});
```

The one-liner form:

```ts
import { orcr } from "@orchestratr/sdk";

const answer = await orcr.ask({
  agent: "codex",
  name: "reviewer",
  prompt: "Review src/auth.ts for auth bugs. Say DONE.",
});
```

## Surface

- `orcr.agent.run/wait/ls/kill` and the returned `AgentHandle`
  (`wait`, `send`, `logs`, `followLogs`, `lastResponse`, `kill`, plus `uuid`/`path`/`name`/`dataDir`).
- `orcr.ask()` — run (`gc: immediate`) → settle wait → last response, in one call.
- `orcr.scope(path, fn, { killOnThrow? })` — an `AsyncLocalStorage` path scope; relative paths
  inside resolve under it, nesting inherits the prefix.
- `orcr.watch({ pattern?, agent?, status?, managed?, sinceSeq? })` — snapshot-then-subscribe
  `AsyncIterable` of typed events.
- `orcr.loop.*` (create/pause/resume/rm/ls/logs + `loop.run.start/stop/ls`),
  `orcr.server.*`, `orcr.api.*`, and `orcr.gen` — the generated 1:1 protocol client.
- Typed errors, one class per orcr error code (`NotFound`, `InvalidRequest`, `StateConflict`,
  `Blocked`, `Timeout`, `IntegrationMissing`, `TranscriptUnavailable`, `EnvironmentError`,
  `ServerError`), all subclasses of `OrcrError`. Blocking calls (`wait`/`ask`) are never capped
  by a client-side timeout — their deadline is the caller's own `timeout` option (unbounded when
  omitted).

## Notes

Not yet published to npm (the package name is finalized at release). `orcr scaffold` installs the
SDK pinned to the CLI's own version; set `ORCR_SDK_SPEC` to a local `file:`/tarball path for
offline installs.

See the skill references for the full SDK surface and recipes:
`skill/references/sdk.md` and `skill/references/patterns.md`.
