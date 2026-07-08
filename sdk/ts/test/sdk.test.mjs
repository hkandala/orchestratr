// Self-test for the orchestratr TS SDK. Skips cleanly when the orcr binary is not on
// PATH (or $ORCR_BIN) or when dist/ has not been built. Uses a temp ORCR_STORE so it
// never touches ~/.orcr or the user's default herdr session — the calls exercised here
// (ps, show of a missing id) read sqlite only and never start a herdr session.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const distEntry = join(here, "..", "dist", "index.js");
const orcrBin = process.env.ORCR_BIN ?? "orcr";

function orcrAvailable() {
  const probe = spawnSync(orcrBin, ["--version"], { stdio: "ignore" });
  return probe.status === 0;
}

const skip = !existsSync(distEntry)
  ? "dist/ not built (run npm run build)"
  : !orcrAvailable()
    ? "orcr binary not found"
    : false;

test("ps returns an empty agent list from a fresh store", { skip }, async () => {
  const { Client } = await import(distEntry);
  const store = mkdtempSync(join(tmpdir(), "orcr-sdk-ts-"));
  const client = new Client({ env: { ORCR_STORE: store } });
  const agents = await client.ps();
  assert.deepEqual(agents, []);
});

test("show of a missing id raises NotFoundErr", { skip }, async () => {
  const { Client, NotFoundErr } = await import(distEntry);
  const store = mkdtempSync(join(tmpdir(), "orcr-sdk-ts-"));
  const client = new Client({ env: { ORCR_STORE: store } });
  await assert.rejects(client.show("a999"), NotFoundErr);
});

const skipDistOnly = existsSync(distEntry) ? false : "dist/ not built (run npm run build)";

test("a missing binary maps to EnvConfigErr", { skip: skipDistOnly }, async () => {
  const { Client, EnvConfigErr } = await import(distEntry);
  const client = new Client({ bin: "/nonexistent/orcr-binary" });
  await assert.rejects(client.ps(), EnvConfigErr);
});
