# M2 · Agent core — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts (especially about herdr). Reading all the
`notes.md` files should give full context on what changed vs the spec and why.
Capture *decisions and deviations*, not a play-by-play.

## Deviations from spec

- **Data dir + `launch.json` are written *before* the durable row**, reversing §11.1's
  "row first, then data dir" order. Reason: the queue worker can promote and start the
  pipeline (which reads `launch.json`) within one tick of the insert, so the payload must
  exist first. A failed `enqueue` (e.g. `path_in_use`) removes the just-created data dir.
  Net effect matches the spec's intent (identity durable + audit written before any herdr
  call); only the two local writes are reordered.
- **Tab labels are herdr's defaults, not the §5.2 "path after first segment".** herdr
  0.7.2's `agent.start {name}` sets the **pane label** (and `agent.name`) — verified live —
  while tabs are auto-labeled `"1"`. orcr passes `tab_label(path)` as the agent `name`, so
  the pane carries the agent identity (and reconcile matches orphans on it). Renaming the
  *tab* per §5.2 would need `tab.rename` (not in the driver contract); deferred as polish.
- **`ORCR_HOME` is injected into pane env** in addition to the §5.3 contract, so a nested
  `orcr` call reaches the same server (required for relocated-home e2e and lineage). The
  launch token rides as `ORCR_LAUNCH_TOKEN` (internal, not part of the public contract).
- **Reserved level-1 set is `idle` + `unmanaged` only.** Active-loop-name reservation and
  loop-run caller scope are deferred to M5 (loops don't exist yet).

## Decisions on under-specified points

- **Caller scope is derived from `caller_path` (`ORCR_PATH` minus the name), not a store
  lookup of `ORCR_ID`.** The CLI forwards its own `ORCR_ID`/`ORCR_PATH`; the server strips
  the last segment for the scope and uses `caller_id` as `parent_id`. Equivalent to the
  §5.3 algorithm for agents; loop-run callers land in M5.
- **A concurrency slot = status NOT IN (`queued`,`ended`,`lost`)** (i.e. starting/working/
  idle/blocked/parked all occupy a slot). Promotion is FIFO by `queue_seq`; a provider at
  its cap is skipped so its FIFO siblings wait while other providers still promote.
- **Workspace creation closes the root shell pane.** `workspace.create` adds a root pane
  that would pin the workspace forever; the pipeline closes it right after the agent pane
  exists, so empty-workspace auto-removal works on kill. Workspace ensure is serialized by
  a `spawn_lock` so concurrent spawns under one level-1 segment never create duplicates.
- **Mock provider gate.** A test-only `mock` provider (argv = `$ORCR_MOCK_AGENT_BIN`) is
  enabled by `ORCR_ALLOW_MOCK_PROVIDER=1`; it bypasses the both-layers check (it self-reports
  via `pane.report_agent`) and is the automated e2e gate. Never available in a normal build.
- **Pre-M3 status.** The pipeline flips `starting → working` after delivery and never reads
  herdr `idle`/`done` to change orcr status — so a provider that reports idle immediately is
  held at `working` (the acceptance falls out of the design; M3 adds completion).
- **Crash recovery matching** (§11.1): rows with a recorded `pane_id` are confirmed against
  the live snapshot (present → repair `starting`→`working`; gone → `failed` if starting,
  else `lost`). Rows with **no** `pane_id` (spawn crashed before recording it) match an
  orphan pane by **pane label == `tab_label(path)`**, close it, and fail the row — because
  herdr 0.7.2 exposes no pane env over the socket, the spec's "match by `ORCR_ID` + launch
  token" isn't possible; the pane label (= the agent name, unique among active paths) is the
  stand-in. No duplicate pane survives either way.

## Discovered facts / gotchas

- **`agent.start {name}` → `pane.label` and `agent.name`** (herdr 0.7.2, probed live); the
  tab keeps herdr's auto label (`"1"`). This shapes both placement and reconcile matching.
- **No pane env over the socket.** `AgentInfo`/`PaneInfo` carry no env/argv, so launch-token
  crash matching reads a stored `pane_id` or the pane label — never the injected env.
- A `mock_env.json` dumped by the mock into `$ORCR_AGENT_DATA_DIR` is how e2e asserts the
  §5.3 env contract actually reached the pane (no other read-back path exists).
- `status = lost` is set by the start-up reconciler when a *running* agent's recorded pane
  vanished; the full "resolve lost → ended(lost) after one confirming poll" loop is M4.

## Verifier & reviewer history

- **Implementation** (this pass, on `main`): `path` module (grammar/scope/patterns) →
  store agent DAL (enqueue, FIFO promotion, resolution, ls, turns, cancel, location) →
  orcr integration layer (launch plans + both-layers enforcement) → server engine (queue
  worker, spawn pipeline, `agent.run/send/kill/ls`, start-up reconciler) → CLI verbs →
  mock env dump → e2e suite. Green gates: `cargo build`, `cargo fmt --check`,
  `cargo clippy --all-targets -D warnings` (clean), `cargo test` (100 unit),
  `ORCR_E2E=1 cargo test --test agent_e2e` (9/9 against live herdr 0.7.2). Post-run
  `herdr session list` shows only the untouched `default` session; no `--foreground`
  orphans.
- **Verify — round 1: FAIL → fixed → PASS.** The verifier found one concrete
  spec-adherence gap: `--timeout` (§5.4/§6) was accepted but not validated at `run` and
  the resulting kill deadline was never persisted, so a durable `deadline_at` was missing
  from the agent row (§12). Resolved in commit `504e644`: `--timeout` is parsed up front
  (units required → `invalid_request` on a bad value) and `deadline_at = created_at +
  timeout` is written into the durable row (the deadline *enforcement* sweep stays out of
  scope here — the reaper lands with the completion/GC work). Re-verify: full suite green.
- **Review: PASS.** Code-review pass over the M2 surface (path grammar/scope/patterns,
  agent DAL incl. FIFO promotion + partial-unique path allocation, spawn pipeline +
  cancel interlock, start-up reconciler, integrations/both-layers enforcement, the four
  verbs + CLI) found no blocking correctness/robustness/spec-adherence issues; the
  `deadline_at` gap above was the substantive item and was already resolved.
- **Scribe — final green check** (2026-07-13, on `main`, clean tree): `cargo build` ok;
  `cargo fmt --check` ok; `cargo clippy --all-targets -- -D warnings` clean; `cargo test`
  green (100 lib unit + `handshake` 2 + `home_config` 2 + `server_protocol` 6 + `e2e`
  skip-path 5); `ORCR_E2E=1 cargo test --test agent_e2e -- --test-threads=1` 9/9,
  `--test e2e` 5/5, `--test conformance_live` 1/1 — all against live herdr 0.7.2. Post-run
  `herdr session list` shows only the untouched `default` session; no `--foreground`
  orphans. **M2 green.**
