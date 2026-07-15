import { test } from "node:test";
import assert from "node:assert/strict";
import { currentScope, runScope } from "../src/scope.js";
import { resolveCreate, scopeOfAgent } from "../src/path.js";

// Property test: `orcr.scope` nesting composes the same effective paths
// as the CLI/server would. The oracle below replicates the server's scope semantics exactly
// (agent caller → scope = path minus name; loop-run caller → scope = full path; nested scopes
// stack prefixes; leading `/` resets to absolute), and we assert the SDK's runScope + currentScope
// composition matches it over randomized scope stacks and inputs. The live cross-check against
// the real server is in tests/recipe_e2e.rs::e2e_sdk_scope_matches_cli.

const noopKill = async () => undefined;

function oracleScope(base: string | undefined, frags: string[]): string | undefined {
  let s = base;
  for (const f of frags) {
    s = f.startsWith("/") ? f.slice(1) : s ? `${s}/${f}` : f;
  }
  return s;
}
function oracleEffective(scope: string | undefined, input: { name?: string; path?: string }): string {
  if (input.name !== undefined) return scope ? `${scope}/${input.name}` : input.name;
  const p = input.path!;
  if (p.startsWith("/")) return p.slice(1);
  return scope ? `${scope}/${p}` : p;
}

/** Compose nested scopes via the SDK, then resolve `input` at the innermost scope. */
async function sdkEffective(frags: string[], input: { name?: string; path?: string }): Promise<string> {
  const nest = async (i: number): Promise<string> => {
    if (i === frags.length) {
      const scope = currentScope();
      return resolveCreate(scope, input as { name: string } | { path: string });
    }
    return runScope(frags[i], () => nest(i + 1), undefined, noopKill);
  };
  return nest(0);
}

// A tiny deterministic PRNG so the property test is reproducible.
function mulberry32(seed: number): () => number {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function seg(rng: () => number): string {
  const n = 1 + Math.floor(rng() * 5);
  const chars = "abcdefghijklmnopqrstuvwxyz0123456789_";
  let s = "";
  for (let i = 0; i < n; i++) s += chars[Math.floor(rng() * 26)]; // letters only (safe level-1)
  return s;
}

test("nested scopes compose the same effective paths as the oracle (randomized)", async () => {
  const rng = mulberry32(1234);
  for (let iter = 0; iter < 300; iter++) {
    const nFrags = Math.floor(rng() * 3); // 0–2 nested scopes
    const frags: string[] = [];
    for (let i = 0; i < nFrags; i++) {
      const abs = rng() < 0.2;
      frags.push((abs ? "/" : "") + seg(rng));
    }
    const useName = rng() < 0.5;
    const input = useName
      ? { name: seg(rng) }
      : { path: (rng() < 0.2 ? "/" : "") + seg(rng) + (rng() < 0.5 ? "/" + seg(rng) : "") };

    // Keep composed depth within limits (skip pathological deep cases — grammar tested elsewhere).
    const oracle = oracleEffective(oracleScope(undefined, frags), input);
    if (oracle.split("/").length > 8) continue;

    const sdk = await sdkEffective(frags, input);
    assert.equal(sdk, oracle, `frags=${JSON.stringify(frags)} input=${JSON.stringify(input)}`);
  }
});

test("SDK-in-loop-in-agent explicit composition", async () => {
  // Simulate an agent caller at review/worker → base scope "review".
  const base = scopeOfAgent("review/worker"); // "review"
  const composed = await runScope(
    "fanout",
    async () =>
      runScope(
        "file_1",
        async () => resolveCreate(currentScope(), { name: "sub" }),
        undefined,
        noopKill,
      ),
    undefined,
    noopKill,
  );
  // Without an env base the SDK starts at root; assert the pure nesting shape:
  assert.equal(composed, "fanout/file_1/sub");
  // And with a base scope, the oracle agrees:
  assert.equal(oracleScope(base, ["fanout", "file_1"]), "review/fanout/file_1");
});

test("absolute scope fragment resets composition", async () => {
  const composed = await runScope(
    "refactor",
    async () =>
      runScope("/verify", async () => resolveCreate(currentScope(), { name: "checker" }), undefined, noopKill),
    undefined,
    noopKill,
  );
  assert.equal(composed, "verify/checker");
});
