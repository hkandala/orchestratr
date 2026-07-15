// `orcr.scope()` — async-context path scoping. Backed by AsyncLocalStorage, NOT a
// process-global, so concurrent scopes (e.g. two fan-outs) never leak into each other. Scopes
// nest: prefixes stack; a leading `/` resets to absolute. The base scope comes from the env
// context so an SDK running inside an agent/loop-run composes on top of its own path.

import { AsyncLocalStorage } from "node:async_hooks";
import { fromEnv } from "./context.js";
import { expandRand, validatePath } from "./path.js";

interface ScopeFrame {
  scope: string | undefined;
}

const storage = new AsyncLocalStorage<ScopeFrame>();

/** The current effective scope: the innermost `orcr.scope()` frame, else the env base scope. */
export function currentScope(): string | undefined {
  const frame = storage.getStore();
  if (frame) return frame.scope;
  return fromEnv().scope;
}

/** Resolve a scope fragment against the current scope into an absolute path string. */
export function resolveScopePath(current: string | undefined, fragment: string): string {
  const frag = expandRand(fragment);
  const effective = frag.startsWith("/")
    ? frag.slice(1)
    : current && current.length > 0
      ? `${current}/${frag}`
      : frag;
  validatePath(effective);
  return effective;
}

export interface ScopeOptions {
  /** On throw, barrier-kill of `<scope>/**` before re-throwing. */
  killOnThrow?: boolean;
}

/**
 * Run `fn` inside a new path scope. Every relative path created or targeted inside `fn`
 * resolves under `scopePath`. `fn` receives the composed absolute scope string.
 */
export async function runScope<T>(
  scopePath: string,
  fn: (scope: string) => Promise<T>,
  opts: ScopeOptions | undefined,
  killScope: (pattern: string) => Promise<unknown>,
): Promise<T> {
  const composed = resolveScopePath(currentScope(), scopePath);
  return storage.run({ scope: composed }, async () => {
    try {
      return await fn(composed);
    } catch (e) {
      if (opts?.killOnThrow) {
        try {
          await killScope(`/${composed}/**`);
        } catch {
          // best-effort cleanup — never mask the original error
        }
      }
      throw e;
    }
  });
}
