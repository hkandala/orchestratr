// The convenience layer (spec §8): `orcr.*` — a curated surface on top of the generated
// protocol client. Every helper documents the protocol calls it makes; anything here a shell
// script could do with `orcr … --json`. Paths are resolved client-side (§5.1 grammar) so the
// composed effective paths match the CLI exactly, then sent as absolute selectors.

import path from "node:path";
import { GeneratedClient } from "./generated.js";
import { Subscription, Transport, orcrHome } from "./wire.js";
import { fromEnv, OrcrContext } from "./context.js";
import { currentScope, runScope, ScopeOptions } from "./scope.js";
import {
  isPattern,
  loopNameFrom,
  nameOf,
  resolveCreate,
  resolveSelector,
  NameOrPath,
} from "./path.js";
import { InvalidRequest, TranscriptUnavailable } from "./errors.js";

/** A `name` XOR `path`, plus provider/model knobs shared by run + ask (§8). */
export interface SpawnOptions {
  agent?: string;
  prompt: string;
  name?: string;
  path?: string;
  model?: string;
  effort?: string;
  cwd?: string;
  timeout?: string;
}
export interface RunOptions extends SpawnOptions {
  gc?: "immediate" | "idle" | "never" | string;
}

export interface WaitOptions {
  timeout?: string;
}
export interface LogsOptions {
  tail?: number;
}
export interface LsOptions {
  pattern?: string;
  agent?: string;
  status?: string;
  managed?: boolean;
  unmanaged?: boolean;
  all?: boolean;
}
export interface KillOptions {
  force?: boolean;
}

export interface WatchOptions {
  pattern?: string;
  agent?: string;
  status?: string;
  managed?: boolean;
  sinceSeq?: number;
}

export interface LoopCreateOptions {
  name: string;
  cron?: string;
  onceAt?: string;
  maxConcurrency?: number;
  overlap?: "queue" | "skip" | "allow" | string;
  timeout?: string;
  command: string[];
  cwd?: string;
}

/** The caller lineage params from the process env (parent id/path), for every scoped call. */
function callerParams(): Record<string, string> {
  const p: Record<string, string> = {};
  const id = process.env.ORCR_ID;
  const cp = process.env.ORCR_PATH;
  if (id && id.length > 0) p.caller_id = id;
  if (cp && cp.length > 0) p.caller_path = cp;
  return p;
}

/** Resolve a creation target to an absolute selector string (leading `/`). Enforces name XOR path. */
function resolveCreateAbs(opts: { name?: string; path?: string }): string {
  const hasName = typeof opts.name === "string" && opts.name.length > 0;
  const hasPath = typeof opts.path === "string" && opts.path.length > 0;
  if (hasName === hasPath) {
    throw new InvalidRequest("exactly one of `name` or `path` is required (naming is mandatory)", {
      reason: "naming_required",
    });
  }
  const input: NameOrPath = hasName ? { name: opts.name! } : { path: opts.path! };
  const effective = resolveCreate(currentScope(), input);
  return `/${effective}`;
}

/** Resolve a selector/pattern to an absolute form; undefined → `<scope>/**` (or throw at root). */
function resolvePatternAbs(pattern: string | undefined): string {
  if (pattern === undefined) {
    const scope = currentScope();
    if (!scope) {
      throw new InvalidRequest(
        "no scope active — pass an explicit pattern (e.g. `**`) to target every active agent",
        { reason: "no_scope" },
      );
    }
    return `/${scope}/**`;
  }
  return `/${resolveSelector(currentScope(), pattern)}`;
}

/** A live agent handle (spec §8) — the return of `orcr.agent.run()`. */
export class AgentHandle {
  readonly uuid: string;
  readonly path: string;
  readonly name: string;
  readonly dataDir: string;

  constructor(
    private readonly orcr: OrcrClient,
    row: Record<string, unknown>,
  ) {
    this.uuid = String(row.uuid);
    this.path = String(row.path);
    this.name = nameOf(this.path);
    this.dataDir = String(row.data_dir ?? "");
  }

  /** `agent.wait` on this agent (targets by uuid — unambiguous across generations). */
  async wait(opts: WaitOptions = {}): Promise<Record<string, unknown>> {
    const params: Record<string, unknown> = { targets: [this.uuid], ...callerParams() };
    if (opts.timeout) params.timeout = opts.timeout;
    return (await this.orcr.req("agent.wait", params)) as Record<string, unknown>;
  }

  /** `agent.send` — deliver a prompt to this agent. */
  async send(prompt: string): Promise<Record<string, unknown>> {
    return (await this.orcr.req("agent.send", {
      target: this.uuid,
      prompt,
      ...callerParams(),
    })) as Record<string, unknown>;
  }

  /** `agent.logs` — the transcript entries (optionally only the last `tail`). */
  async logs(opts: LogsOptions = {}): Promise<unknown[]> {
    const params: Record<string, unknown> = {
      target: this.uuid,
      last_response: false,
      ...callerParams(),
    };
    if (opts.tail !== undefined) params.tail = opts.tail;
    const r = (await this.orcr.req("agent.logs", params)) as Record<string, unknown>;
    return (r.entries as unknown[]) ?? [];
  }

  /** Poll the transcript, yielding entries as they appear (streaming is a separate concern). */
  async *followLogs(opts: { intervalMs?: number } = {}): AsyncIterable<unknown> {
    const interval = opts.intervalMs ?? 500;
    let seen = 0;
    for (;;) {
      const entries = await this.logs();
      for (let i = seen; i < entries.length; i++) yield entries[i];
      seen = entries.length;
      // Stop once the agent has ended and we've drained its transcript.
      const ls = await this.orcr.agent.ls({ all: true, pattern: this.uuid });
      const me = ls.find((a) => String((a as Record<string, unknown>).uuid) === this.uuid) as
        | Record<string, unknown>
        | undefined;
      if (me && String(me.status) === "ended") {
        const after = await this.logs();
        for (let i = seen; i < after.length; i++) yield after[i];
        return;
      }
      await new Promise((r) => setTimeout(r, interval));
    }
  }

  /** `agent.logs --last-response` — the final assistant message text (throws if unavailable). */
  async lastResponse(): Promise<string> {
    const r = (await this.orcr.req("agent.logs", {
      target: this.uuid,
      last_response: true,
      ...callerParams(),
    })) as Record<string, unknown>;
    const resp = r.response as Record<string, unknown> | undefined;
    if (!resp || typeof resp.text !== "string") {
      throw new TranscriptUnavailable("no response text available", { uuid: this.uuid });
    }
    return resp.text;
  }

  /** `agent.kill` on this agent. */
  async kill(opts: KillOptions = {}): Promise<Record<string, unknown>> {
    return (await this.orcr.req("agent.kill", {
      targets: [this.uuid],
      force: opts.force ?? false,
      ...callerParams(),
    })) as Record<string, unknown>;
  }
}

/** The `orcr.agent.*` collection surface. */
class AgentApi {
  constructor(private readonly orcr: OrcrClient) {}

  /** `agent.run` — spawn a managed agent; returns a handle immediately. */
  async run(opts: RunOptions): Promise<AgentHandle> {
    const params: Record<string, unknown> = {
      path: resolveCreateAbs(opts),
      prompt: opts.prompt,
      ...callerParams(),
    };
    if (opts.agent) params.agent = opts.agent;
    if (opts.gc) params.gc = opts.gc;
    if (opts.model) params.model = opts.model;
    if (opts.effort) params.effort = opts.effort;
    if (opts.cwd) params.cwd = opts.cwd;
    if (opts.timeout) params.timeout = opts.timeout;
    const r = (await this.orcr.req("agent.run", params)) as Record<string, unknown>;
    return new AgentHandle(this.orcr, r.agent as Record<string, unknown>);
  }

  /** `agent.wait` on a pattern (relative to scope; `/` absolute). No arg → `<scope>/**`. */
  async wait(pattern?: string, opts: WaitOptions = {}): Promise<Record<string, unknown>> {
    const params: Record<string, unknown> = {
      targets: [resolvePatternAbs(pattern)],
      ...callerParams(),
    };
    if (opts.timeout) params.timeout = opts.timeout;
    return (await this.orcr.req("agent.wait", params)) as Record<string, unknown>;
  }

  /** `agent.ls` — active (and, with `all`, ended) agents as flat rows. */
  async ls(opts: LsOptions = {}): Promise<unknown[]> {
    const params: Record<string, unknown> = { ...callerParams() };
    if (opts.pattern !== undefined) params.pattern = `/${resolveSelector(currentScope(), opts.pattern)}`;
    if (opts.agent) params.agent = opts.agent;
    if (opts.status) params.status = opts.status;
    if (opts.managed) params.managed = true;
    if (opts.unmanaged) params.unmanaged = true;
    if (opts.all) params.all = true;
    const r = (await this.orcr.req("agent.ls", params)) as Record<string, unknown>;
    return (r.agents as unknown[]) ?? [];
  }

  /** `agent.kill` on a pattern. No interactive confirm in the SDK (`-y` semantics). */
  async kill(pattern?: string, opts: KillOptions = {}): Promise<Record<string, unknown>> {
    return (await this.orcr.req("agent.kill", {
      targets: [resolvePatternAbs(pattern)],
      force: opts.force ?? false,
      ...callerParams(),
    })) as Record<string, unknown>;
  }

  /** `agent.attach.prepare` — returns the exec command + lease; exec it yourself, then heartbeat/release. */
  async prepareAttach(
    target: string,
    opts: { takeover?: boolean } = {},
  ): Promise<AttachHandle> {
    const r = (await this.orcr.req("agent.attach.prepare", {
      target: `/${resolveSelector(currentScope(), target)}`,
      takeover: opts.takeover ?? false,
      ...callerParams(),
    })) as Record<string, unknown>;
    return new AttachHandle(this.orcr, r);
  }
}

/** An attach lease + exec command (spec §8). The caller execs `command`; heartbeat/release manage the lease. */
export class AttachHandle {
  readonly command: string[];
  readonly leaseId: string;
  readonly uuid: string;
  readonly path: string;
  readonly ttlMs: number;

  constructor(
    private readonly orcr: OrcrClient,
    row: Record<string, unknown>,
  ) {
    this.command = (row.command as string[]) ?? [];
    this.leaseId = String(row.lease_id ?? "");
    this.uuid = String(row.uuid ?? "");
    this.path = String(row.path ?? "");
    this.ttlMs = Number(row.ttl_ms ?? 0);
  }

  async heartbeat(): Promise<void> {
    await this.orcr.req("agent.attach.heartbeat", { lease_id: this.leaseId });
  }
  async release(): Promise<void> {
    await this.orcr.req("agent.attach.release", { lease_id: this.leaseId });
  }
}

/** The `orcr.loop.run.*` surface. */
class LoopRunApi {
  constructor(private readonly orcr: OrcrClient) {}

  /** `loop.run.start` — manually trigger a run; returns its handle incl. computed `dataDir`. */
  async start(name: string): Promise<Record<string, unknown>> {
    const r = (await this.orcr.req("loop.run.start", { name })) as Record<string, unknown>;
    const run = (r.run as Record<string, unknown>) ?? {};
    const runId = String(run.run_id ?? "");
    return {
      ...run,
      runId,
      dataDir: path.join(orcrHome(), "data", name, runId),
    };
  }
  /** `loop.run.stop` — stop run(s) of a loop. */
  async stop(name: string, opts: { runId?: string } = {}): Promise<Record<string, unknown>> {
    const params: Record<string, unknown> = { name };
    if (opts.runId) params.run = opts.runId;
    return (await this.orcr.req("loop.run.stop", params)) as Record<string, unknown>;
  }
  /** `loop.run.ls` — list a loop's runs. */
  async ls(name: string, opts: { all?: boolean } = {}): Promise<unknown[]> {
    const r = (await this.orcr.req("loop.run.ls", {
      name,
      all: opts.all ?? false,
    })) as Record<string, unknown>;
    return (r.runs as unknown[]) ?? [];
  }
}

/** The `orcr.loop.*` surface. */
class LoopApi {
  readonly run: LoopRunApi;
  constructor(private readonly orcr: OrcrClient) {
    this.run = new LoopRunApi(orcr);
  }

  /** `loop.create` — create a durable cron loop over an argv command. cwd defaults to cwd. */
  async create(opts: LoopCreateOptions): Promise<Record<string, unknown>> {
    const params: Record<string, unknown> = {
      name: opts.name,
      command: opts.command,
      cwd: opts.cwd ?? process.cwd(),
    };
    if (opts.cron) params.cron = opts.cron;
    if (opts.onceAt) params.once_at = opts.onceAt;
    if (opts.maxConcurrency !== undefined) params.max_concurrency = opts.maxConcurrency;
    if (opts.overlap) params.overlap = opts.overlap;
    if (opts.timeout) params.timeout = opts.timeout;
    const r = (await this.orcr.req("loop.create", params)) as Record<string, unknown>;
    return (r.loop as Record<string, unknown>) ?? {};
  }
  async pause(name: string): Promise<unknown> {
    return this.orcr.req("loop.pause", { names: [name] });
  }
  async resume(name: string): Promise<unknown> {
    return this.orcr.req("loop.resume", { names: [name] });
  }
  async rm(name: string, opts: { killActive?: boolean } = {}): Promise<unknown> {
    return this.orcr.req("loop.rm", {
      names: [name],
      kill_active: opts.killActive ?? false,
      ...callerParams(),
    });
  }
  async ls(opts: { all?: boolean; status?: string } = {}): Promise<unknown[]> {
    const params: Record<string, unknown> = {};
    if (opts.all) params.all = true;
    if (opts.status) params.status = opts.status;
    const r = (await this.orcr.req("loop.ls", params)) as Record<string, unknown>;
    return (r.loops as unknown[]) ?? [];
  }
  async logs(
    name: string,
    opts: { run?: string; source?: string; tail?: number } = {},
  ): Promise<unknown[]> {
    const params: Record<string, unknown> = { name };
    if (opts.run) params.run = opts.run;
    if (opts.source) params.source = opts.source;
    if (opts.tail !== undefined) params.tail = opts.tail;
    const r = (await this.orcr.req("loop.logs", params)) as Record<string, unknown>;
    return (r.lines as unknown[]) ?? [];
  }
}

class ServerApi {
  constructor(private readonly orcr: OrcrClient) {}
  async status(): Promise<Record<string, unknown>> {
    return (await this.orcr.req("server.status", {})) as Record<string, unknown>;
  }
  async stop(): Promise<Record<string, unknown>> {
    return (await this.orcr.req("server.stop", {})) as Record<string, unknown>;
  }
  async handshake(): Promise<Record<string, unknown>> {
    return (await this.orcr.req("server.handshake", {})) as Record<string, unknown>;
  }
}

class ApiApi {
  constructor(private readonly orcr: OrcrClient) {}
  async schema(): Promise<Record<string, unknown>> {
    return (await this.orcr.req("api.schema", {})) as Record<string, unknown>;
  }
  async snapshot(): Promise<Record<string, unknown>> {
    return (await this.orcr.req("api.snapshot", {})) as Record<string, unknown>;
  }
}

/** A snapshot-then-subscribe stream (spec §8) — what `orcr top` renders. */
export class Watch implements AsyncIterable<Record<string, unknown>> {
  constructor(private readonly sub: Subscription) {}
  /** The pinned initial snapshot document. */
  get snapshot(): Record<string, unknown> {
    return (this.sub.initial.snapshot as Record<string, unknown>) ?? {};
  }
  get snapshotSeq(): number {
    return Number(this.sub.initial.snapshot_seq ?? this.sub.initial.from_seq ?? 0);
  }
  /** Iterate typed events (the `event` payload of each frame). */
  async *[Symbol.asyncIterator](): AsyncIterator<Record<string, unknown>> {
    for await (const frame of this.sub) {
      yield (frame.event as Record<string, unknown>) ?? frame;
    }
  }
  close(): void {
    this.sub.close();
  }
}

/** The orcr SDK client. Use the default `orcr` singleton, or construct your own transport. */
export class OrcrClient {
  readonly gen: GeneratedClient;
  readonly agent: AgentApi;
  readonly loop: LoopApi;
  readonly server: ServerApi;
  readonly api: ApiApi;
  readonly context = { fromEnv };
  readonly loopNameFrom = loopNameFrom;
  private started = false;

  constructor(private readonly transport: Transport = new Transport()) {
    this.gen = new GeneratedClient(this.transport);
    this.agent = new AgentApi(this);
    this.loop = new LoopApi(this);
    this.server = new ServerApi(this);
    this.api = new ApiApi(this);
  }

  /** Ensure the server is running (auto-start), memoized. */
  private async ensure(): Promise<void> {
    if (this.started) return;
    await this.transport.ensureRunning();
    this.started = true;
  }

  /** Internal: a request that first ensures the server is running. */
  async req(method: string, params: unknown = {}): Promise<unknown> {
    await this.ensure();
    return this.transport.request(method, params);
  }

  /** The one-liner: `agent.run({gc:immediate}) → wait → lastResponse`. Returns the text. */
  async ask(opts: SpawnOptions): Promise<string> {
    const params: Record<string, unknown> = {
      path: resolveCreateAbs(opts),
      prompt: opts.prompt,
      ...callerParams(),
    };
    if (opts.agent) params.agent = opts.agent;
    if (opts.model) params.model = opts.model;
    if (opts.effort) params.effort = opts.effort;
    if (opts.cwd) params.cwd = opts.cwd;
    if (opts.timeout) params.timeout = opts.timeout;
    const r = (await this.req("agent.ask", params)) as Record<string, unknown>;
    const resp = r.response as Record<string, unknown> | undefined;
    // Share the transcript-unavailable contract with AgentHandle.lastResponse():
    // a response carrying no text is a typed error, never a silent "" (spec §8).
    if (!resp || typeof resp.text !== "string") {
      throw new TranscriptUnavailable("no response text available", {
        path: params.path as string,
      });
    }
    return resp.text;
  }

  /** Run `fn` in a new path scope. Relative paths inside resolve under `scopePath`. */
  scope<T>(
    scopePath: string,
    fn: (scope: string) => Promise<T>,
    opts?: ScopeOptions,
  ): Promise<T> {
    return runScope(scopePath, fn, opts, (pattern) => this.agent.kill(pattern, { force: true }));
  }

  /** Snapshot-then-subscribe live events (spec §8). */
  async watch(opts: WatchOptions = {}): Promise<Watch> {
    await this.ensure();
    const params: Record<string, unknown> = {};
    if (opts.pattern !== undefined)
      params.pattern = `/${resolveSelector(currentScope(), opts.pattern)}`;
    if (opts.agent) params.agent = opts.agent;
    if (opts.status) params.status = opts.status;
    if (opts.managed !== undefined) params.managed = opts.managed;
    if (opts.sinceSeq !== undefined) params.since_seq = opts.sinceSeq;
    const sub = await this.transport.openStream("watch.open", params);
    return new Watch(sub);
  }
}

export type { OrcrContext };
export { loopNameFrom };

/** The default client — the primary entry point (`import { orcr } from "@orchestratr/sdk"`). */
export const orcr = new OrcrClient();
