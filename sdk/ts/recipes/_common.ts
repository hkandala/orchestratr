// Shared helpers for the recipe fixtures. These are the "real stubs" the milestone calls
// for — the illustrative helpers from the spec (stillCheap(), queueSize(), git-diff lists)
// replaced by deterministic implementations so the recipes run end-to-end in CI against the
// mock provider. The copy-pasteable, provider-literal versions live in
// skill/references/patterns.md.
//
// Provider selection is env-driven so CI can point every recipe at the mock provider:
//   ORCR_RECIPE_AGENT     primary provider   (default "claude")
//   ORCR_RECIPE_VERIFIER  secondary provider (default "codex")
// When the primary is "mock", `mockHint()` appends mock directives (`@say=…`, `@write=…`) so
// the mock produces the guaranteed-format answers a real model would; real providers ignore
// these (they are treated as literal prompt text) or never see them.

export const PRIMARY = process.env.ORCR_RECIPE_AGENT ?? "claude";
export const SECONDARY = process.env.ORCR_RECIPE_VERIFIER ?? "codex";
export const IS_MOCK = PRIMARY === "mock";

/** Append a mock-only directive (e.g. `@say=PASS`) — a no-op for real providers. */
export function mockHint(directive: string): string {
  return IS_MOCK ? ` ${directive}` : "";
}

/** A deterministic build oracle: fails `failFor` times, then goes green. */
export function makeBuild(failFor: number): () => { ok: boolean; errors: string } {
  let calls = 0;
  return () => {
    calls += 1;
    if (calls <= failFor) return { ok: false, errors: `error TS1234: sample error #${calls}` };
    return { ok: true, errors: "" };
  };
}

/** A tiny in-memory work queue (stands in for the durable-handoff queue). */
export class WorkQueue {
  private items: string[];
  constructor(items: string[]) {
    this.items = [...items];
  }
  size(): number {
    return this.items.length;
  }
  workOne(): string | undefined {
    return this.items.shift();
  }
}

/** Run a recipe `main` as a script: exit non-zero on throw so the e2e harness sees failure. */
export function runAsScript(main: () => Promise<void>): void {
  main().then(
    () => process.exit(0),
    (e) => {
      console.error(e);
      process.exit(1);
    },
  );
}
