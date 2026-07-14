// §9.1 Fix-until-green — worker + verifier loop. Fix build errors with one agent, verify with
// a *different* provider, repeat until the verifier says PASS. (Spec-literal version, with a
// real `execSync` build, is in skill/references/patterns.md.)
import { orcr } from "@orchestratr/sdk";
import { PRIMARY, SECONDARY, makeBuild, mockHint, runAsScript } from "./_common.js";

export async function run(): Promise<void> {
  const build = makeBuild(1); // fails once, then green (deterministic for CI)

  await orcr.scope("fix_build", async () => {
    const fixer = await orcr.agent.run({
      agent: PRIMARY,
      name: "fixer",
      gc: "never",
      cwd: process.cwd(),
      prompt: "You fix TypeScript build errors in this repo. Wait for my input.",
    });
    await fixer.wait(); // let the fixer come up + process its priming prompt before we steer it

    for (let iter = 1; iter <= 10; iter++) {
      const { ok, errors } = build();
      if (ok) {
        // independent eyes: a different provider judges the changes, not the author
        const verdict = await orcr.ask({
          agent: SECONDARY,
          path: `verify/iter_${iter}`,
          prompt:
            `The build is green. Review the uncommitted changes in ${process.cwd()} for ` +
            `correctness and unintended edits. Reply exactly PASS or FAIL: <reason>.` +
            mockHint("@say=PASS"),
        });
        if (verdict.trim().startsWith("PASS")) break;
        await fixer.send(`A reviewer rejected the changes: ${verdict}. Address this.`);
      } else {
        await fixer.send(`Build errors (iteration ${iter}):\n${errors}\nFix all of them.`);
      }
      await fixer.wait();
    }
    await fixer.kill();
  }, { killOnThrow: true }); // any crash cleans up the whole subtree

  console.log("fix-until-green: done");
}

runAsScript(run);
