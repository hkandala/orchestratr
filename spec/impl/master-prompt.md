# orchestratr — implementation master prompt

This is the authoritative brief for building **orchestratr** (CLI: `orcr`) from the
design in [`spec/spec.md`](../spec.md), milestone by milestone, via orchestrated
subagents. Every subagent (implementer, verifier, reviser, reviewer) reads this file
first, then the full spec, then the milestone file it is assigned.

## 0 · What we are building

`orcr` is a cross-provider orchestrator for AI coding agents, built on
[herdr](https://herdr.dev). Read [`spec/spec.md`](../spec.md) **in full** — it is the
complete, locked design (problem, architecture, CLI, TUI, SDK, execution model, store,
config, edge cases, milestones). This is a **from-scratch build**: there is no prior
implementation to preserve. Do not reference, mention, or reintroduce any earlier
version of the design anywhere — this spec is simply *the* design.

Primary language: **Rust** (the `orcr` binary — server, CLI, driver, TUI, integrations,
store). Plus a **TypeScript SDK** and a **skill** (SKILL.md + references), delivered in M7.

## 1 · Ground truth & required reading (in order)

1. `spec/impl/master-prompt.md` — this file (the process).
2. `spec/spec.md` — the complete design (~2045 lines). **Read all of it.**
3. `spec/milestones/m<N>-*.md` — your assigned milestone's scope + acceptance + out-of-scope.
4. `spec/impl/herdr-driver-reference.md` — verified facts about the installed herdr
   0.7.2 (socket protocol 16, per-session sockets, method/param/result shapes,
   `agent_status` enum, safe-probing rules). A large head start for driver work; still
   verify against live herdr.
5. `spec/impl/m<N>-*/todos.md` and `notes.md` — the living task list and decision log
   for your milestone (you maintain these).

The section numbers referenced throughout (e.g. §5.1, §11.1) are sections of
`spec/spec.md`.

## 2 · The milestone pipeline (how each milestone is driven)

Milestones ship strictly in order **M0 → M7**. A milestone is not "done" and the next
never begins until it earns a clean PASS from BOTH the verifier and the reviewer. The
loop per milestone:

```
implement → verify → (revise → verify)* → [verifier PASS]
          → review  → (revise → review)*  → [reviewer PASS]  → MILESTONE DONE
```

Roles:

- **Implementer** — implements the entire milestone: all code, all tests, gets it
  building and green, exercises the acceptance criteria (mock-provider e2e against
  live herdr where feasible), updates `todos.md` and `notes.md`. Reports a structured
  completion summary.
- **Verifier** — a fresh, independent, adversarial pass. Reads the full spec + the
  milestone file, inspects the implementation, and **actually runs things**: `cargo
  build`, `cargo test`, `cargo clippy`, the e2e suites, and manual spot-checks against
  the acceptance criteria. Verifies the milestone is genuinely, completely implemented —
  not just that tests pass, but that the implementation matches the spec's intent and
  the milestone's acceptance list item by item. Returns a verdict: **PASS** or **FAIL**
  with a concrete, actionable issue list (each with severity + location + why it
  violates the spec/acceptance).
- **Reviser** — given a verdict's issue list, fixes every issue, keeps tests green,
  commits, updates `notes.md`. Does not expand scope beyond fixing the issues (plus
  any obviously-correct adjacent fixes).
- **Reviewer** — a code-review pass (a Claude Code code-reviewer agent) over the
  milestone's code for correctness, robustness, security, spec-adherence, test quality,
  and maintainability. Returns **PASS** or **FAIL** with prioritized findings.

Rules:
- **Do not advance milestones without a clean PASS from the verifier AND the reviewer.**
- The verifier and reviewer must be genuinely independent and skeptical — default to
  FAIL when acceptance criteria are unproven or tests are shallow/tautological.
- If a loop cannot converge (issues persist after several revise rounds), stop the
  milestone and surface the blocker clearly rather than declaring a false PASS.

## 3 · Deliverable layout & the impl folder

Final repo state (no version markers anywhere):

```
Cargo.toml, Cargo.lock          # the orcr Rust crate
src/…                           # server, CLI, driver, store, integrations, top, …
sdk/ts/…                        # @orchestratr/sdk (M7)
skill/…                         # SKILL.md + references/ (M7)
tests/…                         # unit + e2e suites
spec/
  spec.md                       # the design
  milestones/m0..m7.md          # milestone plans
  impl/
    master-prompt.md            # this file
    herdr-driver-reference.md   # verified herdr facts
    m0-foundations/  { todos.md, notes.md }
    m1-server-protocol/ { … }
    …  m7-sdk-skill/ { todos.md, notes.md }
    manual-e2e-tests.md         # (final phase) the manual e2e test plan
    manual-e2e-results.md       # (final phase) observed results / issues
```

- **`todos.md`** (per milestone): a comprehensive, checkbox task list covering the
  milestone's scope + acceptance criteria. The implementer/revisers keep it current
  (`[ ]` → `[x]`), adding tasks discovered mid-build. At milestone end every relevant
  box is checked or explicitly deferred with a reason.
- **`notes.md`** (per milestone): the decision log. Record anything that deviates from
  or is under-specified by the spec, any behavioral choice worth knowing, and any
  discovered facts (especially about herdr) — enough that reading all `notes.md` files
  gives full context on what changed vs the spec and why. Not a play-by-play; capture
  the *decisions and deviations*, not every edit.

## 4 · Engineering standards

- **Match the spec exactly.** Where the spec is precise (grammar, status vocabulary,
  error codes, exit codes, JSON shapes, env contract, store schema), implement it
  verbatim. Where it is silent, choose the simplest correct behavior and record it in
  `notes.md`.
- **Tests are mandatory and must be real.** Unit tests for logic (path/glob grammar,
  resolution, queue promotion, completion state machine, cron/tz, config validation,
  store round-trips, glob→matcher). e2e tests behind an env flag (so unit runs stay
  fast) that exercise acceptance criteria against **live herdr + the mock provider**.
  No tautological tests, no asserting-the-mock. Prefer testing observable behavior.
- **The socket API is the API.** CLI and SDK are thin clients (§3, §11.6). Build the
  method registry so `api schema` is complete and the SDK can't drift (M1 registers
  the full namespace; later milestones replace stubs).
- **Single writer.** The server owns the store; all writes go through
  `BEGIN IMMEDIATE` transactions (§12). Events are written in the same transaction as
  the change they describe (§11.6).
- **Cross-platform intent, POSIX-first reality.** Target macOS + Linux; Windows is
  future work (loops/process-groups/enable are POSIX in this release, per spec).
- **Keep it clean.** Idiomatic Rust, `cargo fmt`, no `cargo clippy` warnings in
  shipped code, no dead code, clear module boundaries mirroring the architecture (§4).

## 5 · Commit discipline (required of every implementer & reviser)

- Work directly on the `main` branch (this is a fresh in-place rebuild of the repo).
- **Commit regularly in small, focused, logically-coherent commits** — e.g. one commit
  per module + its tests, per CLI verb, per bug fix. **Never** a single giant
  end-of-milestone commit.
- Each commit must build (`cargo build` succeeds) and, where the change touches tested
  code, keep the unit suite green. Use clear conventional messages
  (`feat(store): …`, `test(driver): …`, `fix(queue): …`, `docs(impl): …`,
  `refactor(cli): …`).
- Commit the `todos.md` / `notes.md` updates alongside the code they describe.
- Do NOT `git push` (local only) and do NOT create branches. Do NOT rewrite history.

## 6 · Safety rules (absolute — protect the user's environment)

- **Never touch the user's real herdr `default` session or its panes.** A live agent
  runs there. All e2e/probing uses a **disposable** herdr session name
  (`orcr_test_<rand>` / `orcr_ci_<rand>`), created and torn down within the test
  (`herdr session stop <name>` + `herdr session delete <name>` in a drop-guard).
- **Never use `~/.orcr` for tests.** Always set `ORCR_HOME` to a throwaway tempdir;
  the spec guarantees `ORCR_HOME` relocates everything (store, socket, lock, config,
  logs, data). Pair a distinct `herdr.session` with each test home.
- Never delete or mutate anything outside the repo working tree and the throwaway test
  homes/sessions. Never `kill` processes you didn't spawn. Never operate on real
  provider accounts destructively.
- Real-provider (claude/codex) e2e is best-effort and may be deferred to the final
  manual-e2e phase; **mock-provider e2e against live herdr is the automated gate** and
  must pass. Never spawn large fleets of real paid agents in automated tests.

## 7 · Environment facts (see herdr-driver-reference.md for detail)

- herdr `0.7.2`, socket protocol `16`; **sessions are per-socket** (major finding —
  see the reference doc; the spec's "single socket for all sessions" assumption is
  wrong for this herdr and the driver must fan out over per-session sockets).
- Rust `1.89`, Node `24`, npm `11`.
- Providers on PATH: `claude`, `codex`, `pi`, `opencode`. herdr integrations current
  for claude + codex → both fully supported for M2+ e2e.
- Known-good Rust deps from the prior scaffold (adjust freely as the spec needs):
  `anyhow`, `thiserror`, `serde`/`serde_json`, `clap` (derive), `rusqlite`
  (bundled, WAL), `chrono`/`chrono-tz`/`iana-time-zone`, `cron`, `dirs`, `uuid` (v7 —
  add it), `regex`, `signal-hook`, `tracing`/`tracing-subscriber`, `toml` (config is
  JSON per §14 — prefer `serde_json`), `ratatui` + `crossterm` (M6), and dev-deps
  `assert_cmd`, `predicates`, `tempfile`. A Unix-socket + async story is needed for
  the server (§11.6) — choose `tokio` or a threaded blocking model; record the choice
  in m1 notes.

## 8 · Final phase — manual end-to-end testing

After M7 passes, a dedicated subagent:
1. Writes `spec/impl/manual-e2e-tests.md` — a comprehensive list of manual end-to-end
   tests covering the whole product (real herdr + real providers where sensible, the
   full CLI surface, loops, top, SDK, scaffold, skill), each with steps + expected
   result.
2. Executes them one at a time (a subagent per test), recording every observed issue in
   `spec/impl/manual-e2e-results.md` (test id, steps run, expected vs actual, pass/fail,
   notes). This phase **reports** issues; it does not silently fix them.

The orchestration ends with a final status summary drawn from the manual-e2e results,
plus recommended next steps.

## 9 · Definition of done (whole project)

- All milestones M0–M7 implemented, each with verifier PASS **and** reviewer PASS.
- Every `todos.md` complete (or items explicitly deferred with reasons); every
  `notes.md` capturing the milestone's deviations/decisions.
- `cargo build` + `cargo test` (unit) green; e2e suites green against live herdr + mock
  provider; `cargo clippy` clean; `cargo fmt` applied.
- SDK covers 100% of schema methods; scaffold works on a clean checkout; skill drill
  passes (§ M7 acceptance).
- Regular, small commits throughout on `main`; no version markers anywhere in the repo.
- `manual-e2e-tests.md` written and executed; `manual-e2e-results.md` records the
  outcomes; final status + next steps reported.
