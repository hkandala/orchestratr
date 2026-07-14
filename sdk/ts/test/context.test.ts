import { test } from "node:test";
import assert from "node:assert/strict";
import { fromEnv } from "../src/context.js";

test("root context when no env", () => {
  const c = fromEnv({});
  assert.equal(c.kind, "root");
  assert.equal(c.scope, undefined);
});

test("agent context: scope is path minus name", () => {
  const c = fromEnv({
    ORCR_ID: "u1",
    ORCR_PATH: "review/fanout/file_1",
    ORCR_AGENT_DATA_DIR: "/home/.orcr/data/review/fanout/file_1/u1",
    ORCR_PARENT_ID: "p1",
    ORCR_PARENT_PATH: "review/lead",
  });
  assert.equal(c.kind, "agent");
  assert.equal(c.scope, "review/fanout");
  assert.equal(c.dataDir, "/home/.orcr/data/review/fanout/file_1/u1");
  assert.deepEqual(c.parent, { id: "p1", path: "review/lead" });
});

test("loop-run context: scope is the whole run path (no agent data dir)", () => {
  const c = fromEnv({
    ORCR_ID: "run1",
    ORCR_PATH: "nightly/r82c9s",
    ORCR_LOOP_DATA_DIR: "/home/.orcr/data/nightly",
  });
  assert.equal(c.kind, "loopRun");
  assert.equal(c.scope, "nightly/r82c9s");
  assert.equal(c.loop?.name, "nightly");
  assert.equal(c.loop?.runId, "r82c9s");
  assert.equal(c.loop?.dataDir, "/home/.orcr/data/nightly");
});

test("agent inside a loop keeps agent kind but sees loop membership", () => {
  const c = fromEnv({
    ORCR_ID: "u2",
    ORCR_PATH: "nightly/r82c9s/triage",
    ORCR_AGENT_DATA_DIR: "/home/.orcr/data/nightly/r82c9s/triage/u2",
    ORCR_LOOP_DATA_DIR: "/home/.orcr/data/nightly",
  });
  assert.equal(c.kind, "agent");
  assert.equal(c.scope, "nightly/r82c9s");
  assert.equal(c.loop?.name, "nightly");
  assert.equal(c.loop?.runId, "r82c9s");
});
