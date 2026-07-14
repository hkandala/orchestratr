// @orchestratr/sdk — a typed client of the orcr socket API (spec §8).
//
//   import { orcr } from "@orchestratr/sdk";
//   const a = await orcr.agent.run({ agent: "codex", name: "worker", prompt: "…" });
//   await a.wait();
//   console.log(await a.lastResponse());
//
// Two layers: the generated protocol client (`orcr.gen`, every socket method 1:1) and the
// convenience helpers on top (`orcr.agent.run`, `orcr.ask`, `orcr.scope`, `orcr.watch`,
// `orcr.loop.*`). See skill/references/sdk.md for the full surface.

export { orcr, OrcrClient, AgentHandle, AttachHandle, Watch, loopNameFrom } from "./client.js";
export type {
  RunOptions,
  SpawnOptions,
  WaitOptions,
  LogsOptions,
  LsOptions,
  KillOptions,
  WatchOptions,
  LoopCreateOptions,
  OrcrContext,
} from "./client.js";

export { fromEnv } from "./context.js";
export type { LoopMembership } from "./context.js";

export {
  GeneratedClient,
  PROTOCOL_METHODS,
  STREAMING_METHODS,
  EVENT_KINDS,
  ERROR_CODES,
} from "./generated.js";

export { Transport, Subscription, ORCR_PROTOCOL, orcrHome, socketPath } from "./wire.js";

export {
  OrcrError,
  NotFound,
  InvalidRequest,
  StateConflict,
  Blocked,
  Timeout,
  IntegrationMissing,
  TranscriptUnavailable,
  EnvironmentError,
  ServerError,
  errorFromWire,
} from "./errors.js";

export {
  Pattern,
  resolveCreate,
  resolveSelector,
  validatePath,
  validSegment,
  expandRand,
  isPattern,
  nameOf,
  scopeOfAgent,
} from "./path.js";
