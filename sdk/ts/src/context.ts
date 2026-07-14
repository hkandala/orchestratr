// `orcr.context.fromEnv()` — the canonical env-derivation helper (spec §8, §5.3). Never
// hand-parse ORCR_PATH; use this. Distinguishes an agent (ORCR_AGENT_DATA_DIR set) from a
// loop-run command (ORCR_AGENT_DATA_DIR unset but ORCR_LOOP_DATA_DIR set) from a plain shell.

import { loopNameFrom, nameOf, scopeOfAgent } from "./path.js";

export interface LoopMembership {
  name: string;
  runId: string;
  path: string;
  dataDir: string;
}

export interface OrcrContext {
  kind: "agent" | "loopRun" | "root";
  id?: string;
  path?: string;
  /** The caller's scope — where relative paths resolve. Agent: path minus name. Run: full path. */
  scope?: string;
  dataDir?: string;
  parent?: { id?: string; path?: string };
  loop?: LoopMembership;
}

function nonEmpty(v: string | undefined): string | undefined {
  return v && v.length > 0 ? v : undefined;
}

/** Derive the current orcr context from the process environment (spec §5.3 env contract). */
export function fromEnv(env: NodeJS.ProcessEnv = process.env): OrcrContext {
  const id = nonEmpty(env.ORCR_ID);
  const path = nonEmpty(env.ORCR_PATH);
  const agentDataDir = nonEmpty(env.ORCR_AGENT_DATA_DIR);
  const loopDataDir = nonEmpty(env.ORCR_LOOP_DATA_DIR);
  const parentId = nonEmpty(env.ORCR_PARENT_ID);
  const parentPath = nonEmpty(env.ORCR_PARENT_PATH);

  const loop: LoopMembership | undefined =
    loopDataDir && path
      ? {
          name: loopNameFrom(path),
          // A loop-run command's path IS `<loop>/<run_id>`; an agent inside it has a longer
          // path but the run id is still the second segment.
          runId: path.split("/")[1] ?? nameOf(path),
          path: path.split("/").slice(0, 2).join("/"),
          dataDir: loopDataDir,
        }
      : undefined;

  // A loop-run command: ORCR_AGENT_DATA_DIR is unset (runs aren't agents), but it has an id,
  // a path, and a loop data dir. Its scope is its whole run path (a run is a directory).
  if (!agentDataDir && id && path && loopDataDir) {
    return { kind: "loopRun", id, path, scope: path, loop };
  }

  // An agent: has its own data dir. Scope is its path minus its name (agents are files).
  if (agentDataDir && id && path) {
    const ctx: OrcrContext = {
      kind: "agent",
      id,
      path,
      scope: scopeOfAgent(path),
      dataDir: agentDataDir,
    };
    if (parentId || parentPath) ctx.parent = { id: parentId, path: parentPath };
    if (loop) ctx.loop = loop;
    return ctx;
  }

  return { kind: "root" };
}
