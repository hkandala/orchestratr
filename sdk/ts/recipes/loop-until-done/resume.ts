// §9.7 resume.ts — one increment per loop run (runs with the §5.3 env contract). Works one
// item; removes its own loop when the queue is empty.
import { orcr } from "@orchestratr/sdk";
import { queueSize, workOneItem } from "./queue.js";

export async function run(): Promise<void> {
  const ctx = orcr.context.fromEnv();
  if (ctx.kind !== "loopRun") throw new Error("resume.ts must run under an orcr loop");

  await workOneItem(); // agents spawned here land under burn_down/<run_id>/…

  if (queueSize() === 0) {
    await orcr.loop.rm(ctx.loop!.name); // self-terminate
  }
}

run().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
