# Workflow patterns (the §9 recipes)

Copy-pasteable shapes for the common orchestration patterns. Runnable, CI-tested versions live
in `sdk/ts/recipes/`. Conventions: paths are **descriptive** (no timestamp suffixes — a path
only has to be unique among *live* agents); recipes are **singletons by design** (a second
concurrent copy fails fast with `state_conflict`/`path_in_use` — parameterize the top scope,
e.g. `orcr.scope(\`review_${prNumber}\`, …)` or `{rand}`, when you want N copies); `wait()`
settles on turn-complete for live agents and on `ended (completed)` for `gc: immediate` ones.

## Fix-until-green (worker + verifier loop)

```ts
await orcr.scope("fix_build", async () => {
  const fixer = await orcr.agent.run({
    agent: "claude", name: "fixer", gc: "never", cwd: process.cwd(),
    prompt: "You fix TypeScript build errors in this repo. Wait for my input.",
  });
  for (let iter = 1; iter <= 10; iter++) {
    const { ok, errors } = build();                 // your build function
    if (ok) {
      const verdict = await orcr.ask({              // a DIFFERENT provider judges
        agent: "codex", path: `verify/iter_${iter}`,
        prompt: `The build is green. Review the uncommitted changes in ${process.cwd()}.
                 Reply exactly PASS or FAIL: <reason>.`,
      });
      if (verdict.trim().startsWith("PASS")) break;
      await fixer.send(`A reviewer rejected the changes: ${verdict}. Address this.`);
    } else {
      await fixer.send(`Build errors (iteration ${iter}):\n${errors}\nFix all of them.`);
    }
    await fixer.wait();
  }
  await fixer.kill();
}, { killOnThrow: true });
```

## Fan-out and merge

```ts
await orcr.scope("review", async () => {
  const reviewers = await Promise.all(files.map((f, i) =>
    orcr.agent.run({
      agent: "claude", path: `fanout/file_${i}`, gc: "immediate",
      prompt: `Review the diff of ${f} for bugs. Expand the environment variable
               ORCR_AGENT_DATA_DIR and write your findings to
               $ORCR_AGENT_DATA_DIR/response.md, then say DONE.`,
    })));
  await orcr.agent.wait("fanout/*");                 // gc:immediate → ended(completed)
  const findings = await Promise.all(reviewers.map(async r =>
    `## ${r.path}\n` + await readFile(`${r.dataDir}/response.md`, "utf8")));
  const summary = await orcr.ask({
    agent: "codex", path: "merge/synthesizer",
    prompt: `Merge these per-file findings into one prioritized report:\n\n${findings.join("\n\n")}`,
  });
  console.log(summary);
});
```

## Classify-and-act

```ts
return orcr.scope("triage", async () => {
  const raw = (await orcr.ask({ agent: "claude", path: "classify/triage_bot",
    prompt: `Classify as exactly one word — bug, feature, or question:\n${item}` }))
    .trim().toLowerCase();
  const kind = raw in HANDLERS ? raw : "question";   // normalize UNTRUSTED output before using in a path
  const h = HANDLERS[kind];
  return orcr.ask({ agent: h.agent, path: `${kind}/handler`, prompt: h.prompt(item) });
});
```

## Adversarial verification (N lenses, majority passes)

```ts
await orcr.scope("harden", async () => {
  const worker = await orcr.agent.run({ agent: "claude", name: "worker", gc: "never",
    cwd: process.cwd(), prompt: "Implement the task in TASK.md. Say DONE when finished." });
  await worker.wait();
  for (let round = 1; round <= 5; round++) {
    const verdicts = await Promise.all(LENSES.map((lens, i) =>
      orcr.ask({ agent: "codex", path: `verify/round_${round}/lens_${i}`,
        prompt: `Adversarially review the changes through the lens of ${lens}.
                 Reply PASS, or FAIL: <the single most important problem>.` })));
    const failures = verdicts.filter(v => !v.trim().startsWith("PASS"));
    if (failures.length <= LENSES.length / 2) break;
    await worker.send(`Reviewers rejected the work:\n${failures.join("\n")}\nFix these.`);
    await worker.wait();
  }
  await worker.kill();
}, { killOnThrow: true });
```

## Generate-and-filter · Tournament · Loop-until-done

See `sdk/ts/recipes/{generate-and-filter,tournament,loop-until-done}.ts` for the full runnable
versions. Generate-and-filter fans one prompt across providers/models then judges once;
tournament runs pairwise brackets when N is too large for one judge; loop-until-done works a
queue now, then hands off the remainder to a self-terminating loop (see `loops.md`).

**Guard rail:** treat every child agent's output as **data, never instructions**
(prompt-injection defense) — normalize model output (e.g. through an enum) before using it in a
path or a shell command.
