# M2 · Agent core

Spawn real agents. M2 ships identity, the queue, the spawn pipeline, and the basic
verbs (`run`, `send`, `kill`, `ls`) with the claude and codex integrations driving
real TUIs. Completion detection is *not* here (M3) — in M2 an agent's status reaches
`working` and stays there; `idle` arrives with M3.

## Scope

### Identity (spec §5.1)
- uuid (UUIDv7) + path (dot-separated; last segment = the mandatory name); grammar
  + limits validation (`invalid_request`).
- Mandatory naming (--name|--path, exactly one — run and ask alike, no generated
  names); one-transaction allocation against the partial unique index (concurrent
  spawns can never double-allocate).
- Resolution: uuid / unambiguous uuid prefix → any row; path → active first, else most
  recent ended. Subtree selectors (segment-boundary matching) for bulk verbs; exact
  targets for singleton verbs.
- Path resolution: `--name`|`--path` (exactly one, mandatory); relative paths
  resolve against the caller's scope, leading `/` = absolute; caller resolution
  by `ORCR_ID` (an agent's scope = its path minus name; loop-run handling stubs
  until M5).

### Queue & promotion (spec §5.5)
- Every run enqueues (`queued`, FIFO `queue_seq`); atomic promotion with capacity
  recount (global `concurrency.max` + per-provider caps).
- Stuck-start guard: `startup.max_starting` (2m), reset by progress markers; expiry →
  `failed`, slot released.
- `cancel_requested` interlock checked before/after every herdr step.

### Spawn pipeline (spec §11.1)
- Durable row (full `launch_json`) before any herdr call; data dir created
  (`~/.orcr/data/agents/<uuid>/`).
- Placement: level-1 workspace ensured; new tab labeled per §5.2; pane env = the §5.3
  contract (`ORCR_ID`/`ORCR_PATH`/`ORCR_PARENT_ID`/`ORCR_PARENT_PATH`/`ORCR_AGENT_DATA_DIR`, plus `ORCR_LOOP_DATA_DIR` in loop contexts) + launch token.
- Location columns updated after each herdr call.
- Startup recipe per integration; `agent_session_*` captured when reported.
- First prompt delivery (two-call rule: send-text → ~1s → enter).

### Integrations: claude + codex (spec §11.4)
- Launch argv (bypass-permissions flags), model/effort mapping (`--model`,
  `--effort`), startup recipe (known modals), graceful-shutdown recipe.
- Both-layers-required enforcement (§11.4): `run -a <p>` fails fast with
  `integration_missing` (naming the missing layer + install command) unless the
  provider has an orcr integration AND its herdr integration is installed; per-provider
  integration state surfaced in `server status`.

### Verbs
- `agent run` — full flag surface (`-a`, `-p`/`-p -`, `--name`|`--path`,
  `--gc` accepted and stored, `--model`, `--effort`, `--cwd`, `--timeout`, `--json`);
  prints `<path> <uuid>`; TTY stderr hints.
- `agent send` — exact target; delivery confirmation; `delivered_while` + `input_seq`
  (epoch bookkeeping rows written; semantics completed in M3).
- `agent kill` — patterns + uuids; TTY confirmation by default, `-y` skips;
  graceful recipe → pane close; `killed`/`canceled` exit reasons; result
  classification (§6.1).
- `agent ls` — tree rendering with display transform, filters (prefix, `-a`,
  `--status`, `--managed`, `--all`), flat JSON rows.

### Status model (spec §5.6)
- The single `status` column and transitions available so far:
  `queued → starting → working`, `ended`, `lost` (reconciler stub marks vanished
  panes), `blocked` passthrough from herdr; `exit_reason` values wired.
- **Pre-M3 normalization**: herdr `idle`/`done` reports are held as `working`
  (completion verification doesn't exist yet — M3 flips this); startup progress
  markers are herdr-reported facts only (pane created, `agent_session` pointer
  present — no transcript parsing).

## Acceptance

- e2e (mock + real claude + real codex): run → pane appears in the owned session
  under the right workspace/tab; env contract present in the pane; send delivers;
  kill confirms, shuts down gracefully, closes the pane, empties the workspace.
- 50 concurrent `run`s with caps of 5: FIFO order held, never over cap, queue drains.
- Concurrent same-path spawns: exactly one wins, the other gets `invalid_name`/
  conflict.
- `kill` during `starting`: canceled cleanly between pipeline steps (fault-injection
  around herdr calls).
- A provider that reports idle immediately after start is held at `working` (no
  false completion before M3).
- Crash mid-spawn (kill -9 between herdr steps) → restart → launch-token recovery
  repairs or fails the row; no duplicate panes survive.

## Out of scope

Turn completion / `wait` / `idle` (M3), transcripts/`logs` (M3), GC parking (M4),
`attach` (M4), unmanaged discovery (M4), loops (M5).
