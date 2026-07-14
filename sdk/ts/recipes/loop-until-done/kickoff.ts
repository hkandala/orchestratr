// §9.7 Loop-until-done + durable handoff — kickoff.ts: work now, then hand off to a loop.
// Work a queue interactively; when the remaining work becomes "check back later", hand off to
// a loop and exit. (Spec-literal version is in skill/references/patterns.md.)
import { orcr } from "@orchestratr/sdk";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { queueSize, workOneItem } from "./queue.js";

// Immediate-work budget: process a few items now, leave the rest for the loop (stands in for
// the spec's `stillCheap()` cost gate).
const BUDGET = Number(process.env.ORCR_RECIPE_BUDGET ?? "1") | 0;

export async function run(): Promise<void> {
  let done = 0;
  while (queueSize() > 0 && done < BUDGET) {
    await workOneItem(); // §9.1-style inner loop
    done += 1;
  }

  if (queueSize() > 0) {
    // The loop's script lives in a scaffolded project (§6.6) at the reusable home; the loop
    // keeps *this* cwd (the workspace its agents inherit); the script is invoked by absolute
    // path. In the fixture, resume.ts sits next to this file.
    const here = path.dirname(fileURLToPath(import.meta.url));
    const tsx = process.env.ORCR_RECIPE_TSX ?? path.join(here, "..", "..", "node_modules", ".bin", "tsx");
    await orcr.loop.create({
      name: process.env.ORCR_RECIPE_LOOP ?? "burn_down",
      cron: "*/30 * * * *",
      timeout: "25m",
      command: [tsx, path.join(here, "resume.ts")],
    });
    console.log("handed off to loop burn_down"); // safe to exit now
  } else {
    console.log("queue drained during kickoff");
  }
}

run().then(
  () => process.exit(0),
  (e) => {
    console.error(e);
    process.exit(1);
  },
);
