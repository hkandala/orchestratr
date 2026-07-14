import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import { GeneratedClient, PROTOCOL_METHODS } from "../src/generated.js";

// The generated-client CI check (spec §8 acceptance): the SDK covers 100% of schema methods,
// and drift fails the build. We compare the committed generated client against the live
// `orcr api schema` — both the method-name set AND a callable method per protocol method.

// Resolve the `orcr` binary so `npm test` is reproducible on a fresh checkout: prefer
// `$ORCR_BIN` (set by the Rust test harness), then the repo-local built binary (the normal
// dev flow after `cargo build`), then `orcr` on PATH. Returns null when none exists so the
// live-schema tests can skip cleanly instead of crashing with ENOENT.
function resolveOrcrBin(): string | null {
  if (process.env.ORCR_BIN && process.env.ORCR_BIN.length > 0) return process.env.ORCR_BIN;
  const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
  for (const profile of ["debug", "release"]) {
    const p = resolve(repoRoot, "target", profile, "orcr");
    if (existsSync(p)) return p;
  }
  return null;
}

const ORCR_BIN = resolveOrcrBin();
const noBin = ORCR_BIN
  ? false
  : "no orcr binary found (set ORCR_BIN or run `cargo build`); skipping live-schema drift check";

function liveSchemaMethods(): string[] {
  const raw = execFileSync(ORCR_BIN ?? "orcr", ["api", "schema"], {
    encoding: "utf8",
    maxBuffer: 32 * 1024 * 1024,
  });
  return Object.keys(JSON.parse(raw).methods).sort();
}

function camel(name: string): string {
  return name
    .split(/[._]/)
    .map((p, i) => (i === 0 ? p : p.charAt(0).toUpperCase() + p.slice(1)))
    .join("");
}

test("generated PROTOCOL_METHODS covers 100% of the live schema", { skip: noBin }, () => {
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
