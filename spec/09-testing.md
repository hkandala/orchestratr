# 09 · Testing

## The pyramid

1. **Unit tests** (no herdr): pure logic — argv builders, preamble/paths, steer-vs-turn
   state machine, id allocation/resolution (`a7`, `a7:t2`, names), config parsing,
   store CRUD on tempdir dbs, transcript parsers against fixture files under
   `tests/fixtures/`.
2. **Fake-herdr driver tests** (no herdr): a PATH-prepended shell-script shim that emits
   canned herdr JSON envelopes; exercises the driver's parsing, polling, completion and
   error paths deterministically (including blocked, timeout, envelope errors).
3. **E2E tests** (real herdr + mock agent): gated behind `ORCR_E2E=1`, run
   `--test-threads=1`. Real herdr binary, isolated session `orcr-e2e-<random>`,
   `ORCR_STORE=<tempdir>`. NEVER touch the user's default herdr session, real harnesses,
   or `~/.orcr`. Cleanup (session stop + delete) via a Drop guard that runs on panic.
   Session-hygiene assertion at suite end: no running `orcr-e2e-*` sessions remain.

## The mock agent (`orcr-mock-agent`)

A scriptable stand-in TUI agent — the e2e workhorse:

- On start: prints `MOCK_READY`; then loops reading stdin lines.
- On a prompt line: prints `MOCK_WORKING`; parses the response-file path out of the orcr
  preamble; honors directives embedded anywhere in the prompt:
  - `[[sleep:<ms>]]` — delay before finishing (timeout tests, steer windows)
  - `[[ignore-out]]` — skip writing the response file (exercises fallback chain)
  - `[[block]]` — print `MOCK_BLOCKED` and stall until next input (blocked handling)
  - `[[exit]]` — terminate (lost-agent handling)
- Steering input received while working is appended into the same pending response.
- Default: writes `# mock response\n<echo of received prompt(s)>` to the response path,
  prints `MOCK_DONE <turn-counter>`.
- The mock profile's completion strategy = OutputMarker(`MOCK_DONE` / `MOCK_BLOCKED`) —
  no herdr detection manifests needed, so e2e is hermetic and fast.

## E2E scenario checklist (grows per milestone)

- M0: single round-trip prompt→response; `[[ignore-out]]` fallback capture. ✅
- M1: fan-out two mocks + `wait --all/--any`; steer mid-turn (`[[sleep]]` window) →
  single response containing both inputs; `--keep` multi-turn; `out --recursive
  --paths`; `kill --tree` on a 3-node tree; timeout → exit 3; blocked → exit 4;
  env-contract lineage (child spawned from inside a mock pane... simulated by setting
  ORCR_* env on the caller).
- M2: loop 3 ticks then `--until`; `--tick-on` fires on probe change; schedule `--at`
  one-shot fires (short-horizon test with seconds-level cron); daemon restart
  reconciliation marks a killed pane `lost`; gc cleans a orphaned pane.
- M3: goal PASS on second iter (mock judge scripted via prompt content); workflow script
  spawning two children, `--on-orphan kill`; history filters.

## CI posture

`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` on every
commit (local pre-push discipline; GitHub Actions later — needs herdr install step for
e2e, so CI runs units + fake-herdr only).
