// The socket transport: connect to `$ORCR_HOME/orcr.sock`, negotiate the
// protocol version, send one-shot requests, open subscription streams, and auto-start the
// server. Mirrors the Rust `server/client.rs`: one request per connection (a fresh socket
// per call), newline-delimited JSON frames, `{ok,result|error}` envelopes.

import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";
import { EnvironmentError, errorFromWire, OrcrError } from "./errors.js";

/** orcr's own socket protocol version (distinct from herdr's). Must match `wire::ORCR_PROTOCOL`. */
export const ORCR_PROTOCOL = 1;

const MAX_FRAME = 16 * 1024 * 1024;
const REQUEST_TIMEOUT_MS = 30_000;

// Server-blocking methods: the server holds the connection open until the target agent turn
// settles (or the caller-supplied `timeout` param elapses), sending nothing in the meantime.
// A fixed client wall-clock cap would kill real (non-mock) turns that routinely run for
// minutes — and make a caller's own `wait({timeout:'5m'})` unusable — so we never impose
// REQUEST_TIMEOUT_MS on these. The server owns the deadline; if it dies the socket closes and
// we surface `server_unreachable`. Mirrors the Rust client (`server/client.rs`).
const BLOCKING_METHODS = new Set<string>(["agent.wait", "agent.ask"]);

/** Resolve the orcr home dir (`$ORCR_HOME` → `~/.orcr`) — mirrors `home.rs`. */
export function orcrHome(): string {
  const env = process.env.ORCR_HOME;
  if (env && env.length > 0) return env;
  return path.join(os.homedir(), ".orcr");
}

/** The Unix socket path (`<home>/orcr.sock`). */
export function socketPath(): string {
  return path.join(orcrHome(), "orcr.sock");
}

interface Envelope {
  id?: unknown;
  ok?: boolean;
  result?: unknown;
  error?: unknown;
}

/** A framed reader over a socket: accumulates bytes, yields whole newline-delimited frames. */
class FrameReader {
  private buf = Buffer.alloc(0);
  private queue: Array<unknown> = [];
  private waiters: Array<(v: unknown | null) => void> = [];
  private rejecters: Array<(e: Error) => void> = [];
  private ended = false;
  private error: Error | null = null;

  constructor(sock: net.Socket) {
    sock.on("data", (chunk: Buffer) => this.onData(chunk));
    sock.on("end", () => this.onEnd());
    sock.on("close", () => this.onEnd());
    sock.on("error", (e) => this.onError(e as Error));
  }

  private onData(chunk: Buffer): void {
    this.buf = Buffer.concat([this.buf, chunk]);
    if (this.buf.length > MAX_FRAME) {
      this.onError(new EnvironmentError("frame exceeds the maximum size", { cause: "frame_too_large" }));
      return;
    }
    let nl: number;
    while ((nl = this.buf.indexOf(0x0a)) >= 0) {
      const line = this.buf.subarray(0, nl);
      this.buf = this.buf.subarray(nl + 1);
      const trimmed = line.toString("utf8").trim();
      if (trimmed.length === 0) continue;
      let parsed: unknown;
      try {
        parsed = JSON.parse(trimmed);
      } catch (e) {
        this.onError(new EnvironmentError(`bad frame from server: ${(e as Error).message}`));
        return;
      }
      this.push(parsed);
    }
  }

  private push(v: unknown): void {
    const w = this.waiters.shift();
    if (w) {
      this.rejecters.shift();
      w(v);
    } else {
      this.queue.push(v);
    }
  }

  private onEnd(): void {
    if (this.ended) return;
    this.ended = true;
    while (this.waiters.length) {
      this.rejecters.shift();
      this.waiters.shift()!(null);
    }
  }

  private onError(e: Error): void {
    if (this.error) return;
    this.error = e;
    while (this.rejecters.length) {
      this.waiters.shift();
      this.rejecters.shift()!(e);
    }
  }

  /** Next frame, or null at clean EOF. */
  next(): Promise<unknown | null> {
    if (this.error) return Promise.reject(this.error);
    if (this.queue.length) return Promise.resolve(this.queue.shift()!);
    if (this.ended) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
      this.waiters.push(resolve);
      this.rejecters.push(reject);
    });
  }
}

function connect(sockPath: string): Promise<net.Socket> {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(sockPath);
    sock.once("connect", () => resolve(sock));
    sock.once("error", (e) =>
      reject(
        new EnvironmentError(`cannot connect to orcr socket ${sockPath}: ${(e as Error).message}`, {
          cause: "server_unreachable",
        }),
      ),
    );
  });
}

function decode(resp: Envelope): unknown {
  if (resp.ok === true) return resp.result ?? null;
  throw errorFromWire(resp.error);
}

function buildRequest(method: string, params: unknown): string {
  return (
    JSON.stringify({
      protocol: ORCR_PROTOCOL,
      id: randomUUID(),
      method,
      params: params ?? {},
    }) + "\n"
  );
}

/** A live subscription stream — an async iterable of `{subscription,seq,event}` frames. */
export class Subscription implements AsyncIterable<Record<string, unknown>> {
  constructor(
    private readonly sock: net.Socket,
    private readonly reader: FrameReader,
    /** The initial `{subscription, snapshot_seq | from_seq, snapshot?}` response. */
    readonly initial: Record<string, unknown>,
  ) {}

  async *[Symbol.asyncIterator](): AsyncIterator<Record<string, unknown>> {
    try {
      for (;;) {
        const frame = await this.reader.next();
        if (frame === null) return;
        yield frame as Record<string, unknown>;
      }
    } finally {
      this.close();
    }
  }

  close(): void {
    this.sock.destroy();
  }
}

/** The socket transport bound to one home/socket path. */
export class Transport {
  constructor(private readonly sockPath: string = socketPath()) {}

  path(): string {
    return this.sockPath;
  }

  /**
   * Send one request, decode the `{ok,result|error}` envelope. Fresh connection per call.
   *
   * A client-side wall-clock timeout guards against a wedged server for ordinary methods, but
   * blocking methods (`agent.wait`/`agent.ask`) are never capped — their cost is the agent's
   * turn, bounded server-side by the caller's own `timeout` param (unbounded when none given).
   * On a client timeout we reject with a typed {@link EnvironmentError} (`cause:"client_timeout"`)
   * so callers can catch it via the same error hierarchy as every other failure path.
   */
  async request(method: string, params: unknown = {}): Promise<unknown> {
    const sock = await connect(this.sockPath);
    const reader = new FrameReader(sock);
    const timeoutMs = BLOCKING_METHODS.has(method) ? null : REQUEST_TIMEOUT_MS;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      sock.write(buildRequest(method, params));
      const framePromise = reader.next();
      let frame: unknown | null;
      if (timeoutMs === null) {
        frame = await framePromise;
      } else {
        const timeout = new Promise<never>((_, reject) => {
          timer = setTimeout(
            () => reject(new EnvironmentError("request timed out", { cause: "client_timeout" })),
            timeoutMs,
          );
        });
        // Promise.race attaches handlers to both, so a late socket-error rejection on
        // `framePromise` after the timeout wins is still considered handled (no unhandled reject).
        frame = await Promise.race([framePromise, timeout]);
      }
      if (frame === null) {
        throw new EnvironmentError("server closed the connection with no response", {
          cause: "server_unreachable",
        });
      }
      return decode(frame as Envelope);
    } finally {
      if (timer) clearTimeout(timer);
      sock.destroy();
    }
  }

  /** Open a subscription stream (`events.subscribe` / `watch.open`). */
  async openStream(method: string, params: unknown = {}): Promise<Subscription> {
    const sock = await connect(this.sockPath);
    const reader = new FrameReader(sock);
    sock.write(buildRequest(method, params));
    const frame = await reader.next();
    if (frame === null) {
      sock.destroy();
      throw new EnvironmentError("server closed before the subscribe response", {
        cause: "server_unreachable",
      });
    }
    const initial = decode(frame as Envelope) as Record<string, unknown>;
    return new Subscription(sock, reader, initial);
  }

  /** The readiness handshake — verifies the server speaks our protocol. */
  async handshake(): Promise<Record<string, unknown>> {
    const r = (await this.request("server.handshake", {})) as Record<string, unknown>;
    const proto = Number(r.protocol ?? 0);
    if (proto !== ORCR_PROTOCOL) {
      throw new EnvironmentError(
        `server speaks protocol ${proto}, this SDK speaks ${ORCR_PROTOCOL}`,
        { cause: "unsupported_version" },
      );
    }
    return r;
  }

  /** Ensure a healthy server is running, auto-starting one if needed. Idempotent. */
  async ensureRunning(): Promise<"started" | "already_running"> {
    try {
      await this.handshake();
      return "already_running";
    } catch {
      // fall through to auto-start
    }
    spawnDetachedServer();
    await this.waitForReady(15_000);
    return "started";
  }

  private async waitForReady(timeoutMs: number): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    let last: unknown;
    for (;;) {
      try {
        await this.handshake();
        return;
      } catch (e) {
        last = e;
      }
      if (Date.now() >= deadline) {
        throw new EnvironmentError("server did not become ready in time", {
          cause: "server_start_failed",
          last_error: last instanceof OrcrError ? last.message : String(last),
        });
      }
      await sleep(100);
    }
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

/** Spawn a detached `orcr server start --foreground`, propagating ORCR_HOME. */
function spawnDetachedServer(): void {
  const bin = process.env.ORCR_BIN && process.env.ORCR_BIN.length > 0 ? process.env.ORCR_BIN : "orcr";
  const child = spawn(bin, ["server", "start", "--foreground"], {
    detached: true,
    stdio: "ignore",
    env: { ...process.env, ORCR_HOME: orcrHome() },
  });
  child.on("error", () => {
    /* surfaced later by waitForReady's timeout */
  });
  child.unref();
}
