import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { GeneratedClient, PROTOCOL_METHODS } from "../src/generated.js";

// The generated-client CI check (spec §8 acceptance): the SDK covers 100% of schema methods,
// and drift fails the build. We compare the committed generated client against the live
// `orcr api schema` — both the method-name set AND a callable method per protocol method.

function liveSchemaMethods(): string[] {
  const bin = process.env.ORCR_BIN && process.env.ORCR_BIN.length > 0 ? process.env.ORCR_BIN : "orcr";
  const raw = execFileSync(bin, ["api", "schema"], { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 });
  return Object.keys(JSON.parse(raw).methods).sort();
}

function camel(name: string): string {
  return name
    .split(/[._]/)
    .map((p, i) => (i === 0 ? p : p.charAt(0).toUpperCase() + p.slice(1)))
    .join("");
}

test("generated PROTOCOL_METHODS covers 100% of the live schema", () => {
  const live = liveSchemaMethods();
  const generated = [...PROTOCOL_METHODS].sort();
  assert.deepEqual(generated, live, "generated.ts is stale — run `npm run codegen`");
});

test("GeneratedClient exposes a callable method for every protocol method", () => {
  const proto = GeneratedClient.prototype as unknown as Record<string, unknown>;
  for (const name of PROTOCOL_METHODS) {
    const m = camel(name);
    assert.equal(typeof proto[m], "function", `GeneratedClient.${m} (for ${name}) missing`);
  }
});
