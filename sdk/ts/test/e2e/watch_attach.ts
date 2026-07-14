// e2e driver for the curated SDK surfaces that lack coverage elsewhere (spec §8):
//   - orcr.watch()      — snapshot-then-subscribe: snapshot/snapshotSeq + typed async events
//   - orcr.prepareAttach() / AttachHandle — command + lease fields, heartbeat/release round-trips
// Run by tests/recipe_e2e.rs (e2e_sdk_watch_and_attach) via tsx against live herdr + the mock.
// Prints "OK watch+attach" and exits 0 on success; any failure exits non-zero with FAIL:<msg>.
import { orcr } from "@orchestratr/sdk";

function fail(msg: string): never {
  console.error("FAIL:", msg);
  process.exit(1);
}

async function main(): Promise<void> {
  const a = await orcr.agent.run({
    agent: "mock",
    path: "watchee/a",
    gc: "never",
    prompt: "say hello @say=hello",
  });
  await a.wait();

  // --- watch (§8): snapshot-then-subscribe ---
  const w = await orcr.watch({});
  if (typeof w.snapshotSeq !== "number") fail("watch.snapshotSeq is not a number");
  if (!Array.isArray(w.snapshot.agents)) fail("watch.snapshot.agents missing");
  if (!(w.snapshot.agents as Array<Record<string, unknown>>).some((x) => x.uuid === a.uuid)) {
    fail("spawned agent absent from watch snapshot");
  }

  // Drive a status change and confirm at least one typed event is yielded before close().
  const nextEvent = (async () => {
    for await (const ev of w) return ev;
    return null;
  })();
  await a.kill();
  const ev = (await Promise.race([
    nextEvent,
    new Promise((r) => setTimeout(() => r(null), 8000)),
  ])) as Record<string, unknown> | null;
  w.close();
  if (!ev) fail("watch iterator yielded no event after a status change");
  if (typeof ev.kind !== "string") fail("watch event missing string `kind`");

  // --- prepareAttach + AttachHandle (§8) ---
  const b = await orcr.agent.run({
    agent: "mock",
    path: "watchee/b",
    gc: "never",
    prompt: "say hello @say=hello",
  });
  await b.wait();
  const at = await orcr.agent.prepareAttach(b.path);
  if (!Array.isArray(at.command) || at.command.length === 0) fail("attach command empty");
  if (!at.leaseId) fail("attach leaseId empty");
  if (!(at.ttlMs > 0)) fail("attach ttlMs not positive");
  if (at.uuid !== b.uuid) fail("attach uuid does not match the target");
  await at.heartbeat();
  await at.release();
  await b.kill();

  console.log("OK watch+attach");
}

main().catch((e) => fail(String(e?.stack ?? e)));
