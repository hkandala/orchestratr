// orchestratr — thin typed wrapper around the `orcr` CLI.
//
// The CLI is the contract: every call here shells `orcr … --json` via child_process
// and parses the single `{"ok":…}` envelope on stdout. The SDK never gains private
// capabilities. Zero runtime dependencies; Node >= 18.

import { execFile, spawn, type ChildProcess } from "node:child_process";

// ----------------------------------------------------------------------------------
// Errors — mapped from the CLI exit-code table (spec/03).
// ----------------------------------------------------------------------------------

export class OrcrError extends Error {
  constructor(
    message: string,
    public readonly exitCode: number,
    public readonly code: string,
    public readonly details?: unknown,
  ) {
    super(message);
    this.name = new.target.name;
  }
}

/** exit 2 — environment/config problem (herdr missing, bad config.toml). */
export class EnvConfigErr extends OrcrError {}
/** exit 3 — a wait or `run --wait` timed out; `details` carries the partial result. */
export class TimeoutErr extends OrcrError {}
/** exit 4 — an agent is blocked and needs a human; `details` carries the result. */
export class BlockedErr extends OrcrError {}
/** exit 5 — the agent was killed. */
export class KilledErr extends OrcrError {}
/** exit 6 — id or name not found. */
export class NotFoundErr extends OrcrError {}
/** exit 7 — lifecycle-invalid operation; `details` has {current_status, wanted, id}. */
export class StateConflictErr extends OrcrError {}

function errorFor(exitCode: number, message: string, code: string, details?: unknown): OrcrError {
  switch (exitCode) {
    case 2:
      return new EnvConfigErr(message, exitCode, code, details);
    case 3:
      return new TimeoutErr(message, exitCode, code, details);
    case 4:
      return new BlockedErr(message, exitCode, code, details);
    case 5:
      return new KilledErr(message, exitCode, code, details);
    case 6:
      return new NotFoundErr(message, exitCode, code, details);
    case 7:
      return new StateConflictErr(message, exitCode, code, details);
    default:
      return new OrcrError(message, exitCode, code, details);
  }
}

// ----------------------------------------------------------------------------------
// Option and result types (mirror the CLI flags and JSON result shapes).
// ----------------------------------------------------------------------------------

export interface RunOptions {
  harness: string;
  prompt?: string;
  promptFile?: string;
  name?: string;
  model?: string;
  effort?: string;
  cwd?: string;
  /** Seconds; forwarded as `--timeout <n>s`. */
  timeoutS?: number;
  keep?: boolean;
  mode?: "tui" | "exec";
  worktree?: boolean;
  parent?: string;
  session?: string;
  /** Block through the first turn and populate `Handle.text`. */
  wait?: boolean;
}

export interface SendOptions {
  promptFile?: string;
  steer?: boolean;
  turn?: boolean;
  wait?: boolean;
}

export interface WaitOptions {
  any?: boolean;
  tree?: string;
  timeoutS?: number;
}

export interface OutOptions {
  turn?: number;
  recursive?: boolean;
  /** Return path metadata only (no body text): `--format path`. */
  paths?: boolean;
}

export interface HistoryOptions {
  since?: string;
  status?: string;
  parent?: string;
  name?: string;
  harness?: string;
  limit?: number;
}

export interface WaitResult {
  completed: string[];
  pending: string[];
  blocked: string[];
  timed_out: boolean;
}

export interface OutItem {
  id: string;
  name: string | null;
  turn: number;
  path: string;
  source: string | null;
  text: string | null;
}

export interface OrcrEvent {
  type: string;
  id: string | null;
  time: string;
  payload: unknown;
}

export interface EventStream {
  close(): void;
}

type Id = string | Handle;

function idOf(ref: Id): string {
  return typeof ref === "string" ? ref : ref.id;
}

// ----------------------------------------------------------------------------------
// Client
// ----------------------------------------------------------------------------------

export interface ClientOptions {
  /** Path to the orcr binary; defaults to $ORCR_BIN then "orcr" on PATH. */
  bin?: string;
  /** Extra environment for spawned orcr processes (e.g. ORCR_STORE). */
  env?: Record<string, string>;
}

export class Client {
  private readonly bin: string;
  private readonly env: Record<string, string>;

  constructor(options: ClientOptions = {}) {
    this.bin = options.bin ?? process.env.ORCR_BIN ?? "orcr";
    this.env = options.env ?? {};
  }

  async run(options: RunOptions): Promise<Handle> {
    const args = ["run", "--harness", options.harness];
    if (options.prompt !== undefined) args.push("-p", options.prompt);
    if (options.promptFile !== undefined) args.push("--prompt-file", options.promptFile);
    if (options.name !== undefined) args.push("--name", options.name);
    if (options.model !== undefined) args.push("--model", options.model);
    if (options.effort !== undefined) args.push("--effort", options.effort);
    if (options.cwd !== undefined) args.push("--cwd", options.cwd);
    if (options.timeoutS !== undefined) args.push("--timeout", `${options.timeoutS}s`);
    if (options.keep) args.push("--keep");
    if (options.mode !== undefined) args.push("--mode", options.mode);
    if (options.worktree) args.push("--worktree");
    if (options.parent !== undefined) args.push("--parent", options.parent);
    if (options.session !== undefined) args.push("--session", options.session);
    if (options.wait) args.push("--wait");
    const result = (await this.exec(args)) as {
      agent: { id: string };
      response?: { text: string };
    };
    return new Handle(result.agent.id, this, result, result.response?.text ?? null);
  }

  async send(id: Id, prompt: string, options: SendOptions = {}): Promise<unknown> {
    const args = ["send", idOf(id)];
    if (options.promptFile !== undefined) {
      args.push("--prompt-file", options.promptFile);
    } else {
      args.push(prompt);
    }
    if (options.steer) args.push("--steer");
    if (options.turn) args.push("--turn");
    if (options.wait) args.push("--wait");
    return this.exec(args);
  }

  async wait(ids: Id | Id[], options: WaitOptions = {}): Promise<WaitResult> {
    const list = Array.isArray(ids) ? ids : [ids];
    const args = ["wait", ...list.map(idOf)];
    if (options.any) args.push("--any");
    if (options.tree !== undefined) args.push("--tree", options.tree);
    if (options.timeoutS !== undefined) args.push("--timeout", `${options.timeoutS}s`);
    return (await this.exec(args)) as WaitResult;
  }

  async out(id: Id, options: OutOptions = {}): Promise<OutItem[]> {
    const args = ["out", idOf(id)];
    if (options.turn !== undefined) args.push("--turn", String(options.turn));
    if (options.recursive) args.push("--recursive");
    if (options.paths) args.push("--format", "path");
    const result = (await this.exec(args)) as { items: OutItem[] };
    return result.items;
  }

  async show(id: Id): Promise<unknown> {
    return this.exec(["show", idOf(id)]);
  }

  async kill(id: Id | Id[], options: { tree?: boolean } = {}): Promise<unknown> {
    const list = Array.isArray(id) ? id : [id];
    const args = ["kill", ...list.map(idOf)];
    if (options.tree) args.push("--tree");
    return this.exec(args);
  }

  async ps(): Promise<unknown[]> {
    const result = (await this.exec(["ps"])) as { agents: unknown[] };
    return result.agents;
  }

  async tree(id?: Id): Promise<unknown[]> {
    const args = ["tree"];
    if (id !== undefined) args.push(idOf(id));
    const result = (await this.exec(args)) as { roots: unknown[] };
    return result.roots;
  }

  async history(options: HistoryOptions = {}): Promise<unknown[]> {
    const args = ["history"];
    if (options.since !== undefined) args.push("--since", options.since);
    if (options.status !== undefined) args.push("--status", options.status);
    if (options.parent !== undefined) args.push("--parent", options.parent);
    if (options.name !== undefined) args.push("--name", options.name);
    if (options.harness !== undefined) args.push("--harness", options.harness);
    if (options.limit !== undefined) args.push("--limit", String(options.limit));
    const result = (await this.exec(args)) as { items: unknown[] };
    return result.items;
  }

  /** Spawns `orcr events --follow --json` and streams one event per NDJSON line. */
  events(onEvent: (event: OrcrEvent) => void): EventStream {
    const child: ChildProcess = spawn(this.bin, ["events", "--follow", "--json"], {
      env: { ...process.env, ...this.env },
      stdio: ["ignore", "pipe", "ignore"],
    });
    let buffer = "";
    child.stdout?.on("data", (chunk: Buffer) => {
      buffer += chunk.toString("utf8");
      let index = buffer.indexOf("\n");
      while (index >= 0) {
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (line.length > 0) {
          try {
            onEvent(JSON.parse(line) as OrcrEvent);
          } catch {
            // Non-JSON noise on the stream is ignored.
          }
        }
        index = buffer.indexOf("\n");
      }
    });
    return {
      close() {
        child.kill();
      },
    };
  }

  private exec(args: string[]): Promise<unknown> {
    return new Promise((resolve, reject) => {
      execFile(
        this.bin,
        [...args, "--json"],
        {
          env: { ...process.env, ...this.env },
          maxBuffer: 64 * 1024 * 1024,
        },
        (error, stdout) => {
          const exitCode =
            error && typeof (error as NodeJS.ErrnoException & { code?: unknown }).code === "number"
              ? ((error as unknown as { code: number }).code as number)
              : error
                ? 1
                : 0;
          if (error && (error as NodeJS.ErrnoException).code === "ENOENT") {
            reject(
              new EnvConfigErr(`orcr binary not found: ${this.bin}`, 2, "env_config"),
            );
            return;
          }
          let envelope: { ok?: boolean; result?: unknown; error?: { code?: string; message?: string; details?: unknown } };
          try {
            envelope = JSON.parse(stdout) as typeof envelope;
          } catch {
            reject(
              new OrcrError(
                `orcr did not print a JSON envelope (exit ${exitCode}): ${stdout.slice(0, 200)}`,
                exitCode,
                "bad_output",
              ),
            );
            return;
          }
          if (envelope.ok === true) {
            if (exitCode === 0) {
              resolve(envelope.result);
            } else {
              // e.g. `wait` exits 3/4 while still printing a full ok envelope.
              reject(
                errorFor(exitCode, `orcr exited ${exitCode}`, "status", envelope.result),
              );
            }
            return;
          }
          const code = envelope.error?.code ?? "error";
          const message = envelope.error?.message ?? `orcr exited ${exitCode}`;
          reject(errorFor(exitCode, message, code, envelope.error?.details));
        },
      );
    });
  }
}

// ----------------------------------------------------------------------------------
// Handle — convenience object returned by run().
// ----------------------------------------------------------------------------------

export class Handle {
  constructor(
    public readonly id: string,
    private readonly client: Client,
    /** The full `run` result envelope body. */
    public readonly result: unknown = undefined,
    /** First-turn response body when `run({wait: true})` was used. */
    public readonly text: string | null = null,
  ) {}

  async wait(options: WaitOptions = {}): Promise<WaitResult> {
    return this.client.wait(this.id, options);
  }

  /** Latest response body (empty string when no response file exists yet). */
  async out(): Promise<string> {
    const items = await this.client.out(this.id);
    return items[items.length - 1]?.text ?? "";
  }

  async send(prompt: string, options: SendOptions = {}): Promise<unknown> {
    return this.client.send(this.id, prompt, options);
  }

  async kill(options: { tree?: boolean } = {}): Promise<unknown> {
    return this.client.kill(this.id, options);
  }
}

// ----------------------------------------------------------------------------------
// Default client + bound module-level functions.
// ----------------------------------------------------------------------------------

export const orcr = new Client();

export const run = orcr.run.bind(orcr);
export const send = orcr.send.bind(orcr);
export const wait = orcr.wait.bind(orcr);
export const out = orcr.out.bind(orcr);
export const show = orcr.show.bind(orcr);
export const kill = orcr.kill.bind(orcr);
export const ps = orcr.ps.bind(orcr);
export const tree = orcr.tree.bind(orcr);
export const history = orcr.history.bind(orcr);
export const events = orcr.events.bind(orcr);
