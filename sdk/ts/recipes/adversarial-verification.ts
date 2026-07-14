// §9.4 Adversarial verification — a worker produces; N verifiers with different lenses try to
// reject; objections loop back until a majority passes.
import { orcr } from "@orchestratr/sdk";
import { PRIMARY, SECONDARY, mockHint, runAsScript } from "./_common.js";

const LENSES = ["correctness", "security", "edge cases and error handling"];

export async function run(): Promise<void> {
  await orcr.scope("harden", async () => {
    const worker = await orcr.agent.run({
      agent: PRIMARY,
      name: "worker",
      gc: "never",
      cwd: process.cwd(),
      prompt: "Implement the task in TASK.md. Say DONE when finished.",
    });
    await worker.wait();

    for (let round = 1; round <= 5; round++) {
      const verdicts = await Promise.all(
        LENSES.map((lens, i) =>
          orcr.ask({
            agent: SECONDARY,
            path: `verify/round_${round}/lens_${i}`,
            prompt:
              `Adversarially review the uncommitted changes in ${process.cwd()} through the ` +
              `lens of ${lens}. Try hard to find a real problem. Reply PASS, or ` +
              `FAIL: <the single most important problem>.` +
              mockHint("@say=PASS"),
          }),
        ),
      );

      const failures = verdicts.filter((v) => !v.trim().startsWith("PASS"));
      if (failures.length <= LENSES.length / 2) break; // majority passed
      await worker.send(`Reviewers rejected the work:\n${failures.join("\n")}\nFix these.`);
      await worker.wait();
    }
    await worker.kill();
  }, { killOnThrow: true });

  console.log("adversarial-verification: done");
}

runAsScript(run);
