// §9.3 Classify-and-act — one cheap classification routes each item to a per-class handler.
import { orcr } from "@orchestratr/sdk";
import { PRIMARY, SECONDARY, mockHint, runAsScript } from "./_common.js";

const HANDLERS: Record<string, { agent: string; prompt: (t: string) => string }> = {
  bug: { agent: PRIMARY, prompt: (t) => `Reproduce and fix this bug report:\n${t}` },
  feature: { agent: SECONDARY, prompt: (t) => `Draft an implementation plan for:\n${t}` },
  question: { agent: PRIMARY, prompt: (t) => `Answer this user question precisely:\n${t}` },
};

export async function triage(item: string): Promise<string> {
  return orcr.scope("triage", async () => {
    const raw = (
      await orcr.ask({
        agent: PRIMARY,
        path: "classify/triage_bot",
        prompt: `Classify this as exactly one word — bug, feature, or question:\n${item}` + mockHint("@say=bug"),
      })
    )
      .trim()
      .toLowerCase();
    // normalize UNTRUSTED model output through the enum before using it in a path
    const kind = raw in HANDLERS ? raw : "question";

    const h = HANDLERS[kind];
    return orcr.ask({ agent: h.agent, path: `${kind}/handler`, prompt: h.prompt(item) });
  });
}

export async function run(): Promise<void> {
  const out = await triage("The login button does nothing on Safari.");
  console.log("classify-and-act:", out.split("\n")[0]);
}

runAsScript(run);
