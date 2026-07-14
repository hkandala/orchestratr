// §9.6 Tournament — when N is too large for one judge, run pairwise brackets; winners advance.
import { orcr } from "@orchestratr/sdk";
import { PRIMARY, mockHint, runAsScript } from "./_common.js";

export async function tournament(candidates: string[]): Promise<string> {
  return orcr.scope(process.env.ORCR_RECIPE_SCOPE ?? "tournament", async () => {
    let pool = candidates;
    for (let round = 1; pool.length > 1; round++) {
      const next: string[] = [];
      for (let i = 0; i < pool.length; i += 2) {
        if (i + 1 >= pool.length) {
          next.push(pool[i]);
          continue;
        } // bye
        const verdict = await orcr.ask({
          agent: PRIMARY,
          path: `round_${round}/match_${i / 2}`,
          prompt:
            `Which is better, A or B? Reply exactly A or B.\n` +
            `--- A ---\n${pool[i]}\n--- B ---\n${pool[i + 1]}` +
            mockHint("@say=A"),
        });
        next.push(verdict.trim().startsWith("B") ? pool[i + 1] : pool[i]);
      }
      pool = next;
    }
    return pool[0];
  });
}

export async function run(): Promise<void> {
  const winner = await tournament(["alpha", "bravo", "charlie", "delta"]);
  console.log("tournament winner:", winner);
}

runAsScript(run);
