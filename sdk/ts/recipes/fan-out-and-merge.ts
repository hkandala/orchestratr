// Fan-out and merge — review changed files in parallel (one cheap agent each,
// gc: immediate), then a synthesizer merges the findings. (Spec-literal version, driven by
// `git diff --name-only`, is in skill/references/patterns.md.)
import { orcr } from "@orchestratr/sdk";
import { readFile } from "node:fs/promises";
import { PRIMARY, SECONDARY, mockHint, runAsScript } from "./_common.js";

export async function run(): Promise<void> {
  // In the spec this is `git diff --name-only main`; the fixture uses a fixed list.
  const files = ["src/parser.ts", "src/eval.ts"];
  if (files.length === 0) {
    console.log("No changed files.");
    return;
  }

  await orcr.scope(process.env.ORCR_RECIPE_SCOPE ?? "review", async () => {
    const reviewers = await Promise.all(
      files.map((f, i) =>
        orcr.agent.run({
          agent: PRIMARY,
          path: `fanout/file_${i}`,
          gc: "immediate",
          prompt:
            `Review the diff of ${f} against main for bugs and risky changes. Expand the ` +
            `environment variable ORCR_AGENT_DATA_DIR and write your findings to ` +
            `$ORCR_AGENT_DATA_DIR/response.md, then say DONE.` +
            mockHint("@write=response.md"),
        }),
      ),
    );

    // settles when every reviewer finishes: gc:immediate → ended (completed)
    await orcr.agent.wait("fanout/*");

    const findings = await Promise.all(
      reviewers.map(async (r) => `## ${r.path}\n` + (await readFile(`${r.dataDir}/response.md`, "utf8"))),
    );

    const summary = await orcr.ask({
      agent: SECONDARY,
      path: "merge/synthesizer",
      prompt:
        `Merge these per-file review findings into one prioritized report, deduplicating ` +
        `overlaps:\n\n${findings.join("\n\n")}`,
    });
    console.log(summary);
  });
}

runAsScript(run);
