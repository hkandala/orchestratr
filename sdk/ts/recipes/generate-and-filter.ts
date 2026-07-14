// §9.5 Generate-and-filter — fan the same prompt across providers/models, judge once, keep the
// winner.
import { orcr } from "@orchestratr/sdk";
import { PRIMARY, SECONDARY, mockHint, runAsScript } from "./_common.js";

const GENERATORS = [
  { agent: PRIMARY, model: "opus" },
  { agent: PRIMARY, model: "sonnet" },
  { agent: SECONDARY },
];

export async function run(): Promise<void> {
  await orcr.scope("landing_copy", async () => {
    const drafts = await Promise.all(
      GENERATORS.map((g, i) =>
        orcr.ask({
          ...g,
          path: `generate/gen_${i}`,
          prompt: "Write hero copy for orchestratr.dev: one headline, one subhead.",
        }),
      ),
    );

    const pick = await orcr.ask({
      agent: PRIMARY,
      path: "judge/picker",
      prompt:
        `Pick the best draft. Reply with only its number.\n` +
        drafts.map((d, i) => `--- ${i} ---\n${d}`).join("\n") +
        mockHint("@say=0"),
    });
    console.log("generate-and-filter picked:", (drafts[parseInt(pick.trim(), 10)] ?? drafts[0]).split("\n")[0]);
  });
}

runAsScript(run);
