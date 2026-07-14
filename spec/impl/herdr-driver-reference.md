# herdr driver reference — verified facts (herdr 0.7.2)

Concrete facts about the installed herdr, gathered by probing the live binary and
socket before implementation began. This is a **head start** for the driver work
(primarily M0, §11.7) — the M0 implementer MUST still regenerate the conformance
fixture from the live `herdr api schema --json` and verify every operation against a
**disposable** session, but these facts are correct as of herdr 0.7.2 and should save
a lot of discovery time. Record any discrepancy you find in the relevant milestone
`notes.md`.

## Versions / environment (verify at runtime, don't hardcode blindly)

- herdr binary: `herdr 0.7.2`, at `/Users/hkandala/.local/bin/herdr` (discover per spec:
  config `herdr.bin` → `$ORCR_HERDR_BIN` → `$PATH`).
- **herdr socket protocol version: `16`**, `schema_version: 1`. Declare a minimum and
  handshake-check it (`protocol` field in every snapshot/response).
- Rust `1.89`, Node `24`, npm `11` present.
- Agent CLIs present: `claude`, `codex`, `pi`, `opencode` (all on PATH).
- herdr integrations installed & current: **claude (v7), codex (v6)**, pi (v4),
  opencode (v8), cursor (v1). `omp`, `copilot`, `devin`, `droid`, `kimi`, `kilo`,
  `hermes`, `qodercli`, `mastracode` NOT installed.
  → claude + codex are fully supported (both layers present). Good for M2 e2e.
- Check integration state programmatically with `herdr integration status`
  (human/`grep`-able lines like `claude: current (v7) (/path)` /
  `omp: not installed (/path)`). The M0/M2 driver needs a **socket** method or a
  parse of this to report per-provider herdr-integration state in `server status`
  (§13). No dedicated socket "integration.status" method was found in protocol 16 —
  there ARE `integration.install` / `integration.uninstall` methods; status likely
  comes from parsing `herdr integration status` or reading the installed hook file
  paths. Resolve this in M0/M2 and record the chosen mechanism in notes.

## ⚠ CRITICAL ARCHITECTURE FINDING — sessions are per-socket, NOT one global socket

The spec (§2, §4, §11.7) *assumes* "herdr's single socket manages all sessions" and
flags it for M0 verification. **The reality in herdr 0.7.2 is different and must shape
the driver:**

- **Each herdr session is its own server with its own socket.** `herdr session list --json`
  returns:
  ```json
  {"sessions":[{"default":true,"name":"default","running":true,
    "session_dir":"/Users/hkandala/.config/herdr",
    "socket_path":"/Users/hkandala/.config/herdr/herdr.sock"}]}
  ```
  Each session row carries its own `session_dir` and `socket_path`. The default
  session's socket is `~/.config/herdr/herdr.sock`. A named session (e.g. `orcr`)
  gets its **own** `session_dir` + `socket_path` once created.
- Consequences for the driver (**record as a deviation from the spec's assumption in
  m0 notes.md**):
  1. **Owned-session bootstrap**: start the `orcr` session's herdr server headless via
     the binary, then discover *its* socket via `herdr session list --json` (match
     `name == herdr.session`), and connect the driver to **that** socket for all
     owned-session operations.
  2. **Unmanaged discovery (M4)**: iterate `herdr session list --json`, and for every
     session that is NOT the owned one, connect to its `socket_path` and read its
     snapshot. There is no single socket that shows every session — you fan out over
     per-session sockets.
  3. `session.snapshot` / `agent.list` / `pane.list` / `workspace.*` over a given
     socket are scoped to **that socket's session**. `AgentInfo` has NO session field
     because the socket already identifies the session — orcr must attach the session
     identity itself (from which socket it came).
- `herdr session list/attach/stop/delete <name>` exist (use `default` as name to
  target the default session for stop). Useful for e2e teardown of disposable
  sessions: `herdr session stop <name>` / `herdr session delete <name>`.

## Response envelope (CLI shape observed; confirm socket shape from schema)

`herdr api snapshot` (a thin socket client) returns:
```json
{"id":"cli:api:snapshot","result":{"snapshot":{ ...see below... }}}
```
The schema declares `success_response` and `error_response` schemas — confirm the exact
socket wire envelope (`id`, `ok`?, `result`/`error`) from `herdr api schema --json`
during M0. Requests are `{protocol, id, method, params}`.

## Socket methods (protocol 16) — the 80 available

The full list (from `herdr api schema --json`, request `method` consts). Driver-relevant
ones in **bold**:

```
agent.explain  agent.focus  agent.get  **agent.list**  agent.read  agent.rename
  **agent.send**  **agent.start**
client.window_title.clear  client.window_title.set
**events.subscribe**  **events.wait**
**integration.install**  integration.uninstall
layout.apply  layout.export  layout.set_split_ratio
**notification.show**
pane.clear_agent_authority  **pane.close**  pane.current  pane.edges  pane.focus
  pane.focus_direction  **pane.get**  pane.layout  **pane.list**  **pane.move**
  pane.neighbor  pane.process_info  pane.read  pane.release_agent  pane.rename
  pane.report_agent  pane.report_agent_session  pane.report_metadata  pane.resize
  **pane.send_input**  **pane.send_keys**  **pane.send_text**  pane.split  pane.swap
  pane.wait_for_output  pane.zoom
**ping**
plugin.action.invoke  plugin.action.list  plugin.disable  plugin.enable  plugin.link
  plugin.list  plugin.log.list  plugin.pane.close  plugin.pane.focus  plugin.pane.open
  plugin.unlink
server.agent_manifests  server.live_handoff  server.reload_agent_manifests
  server.reload_config  **server.stop**
**session.snapshot**
tab.close  **tab.create**  tab.focus  tab.get  tab.list  tab.move  tab.rename
workspace.close  **workspace.create**  workspace.focus  workspace.get
  **workspace.list**  workspace.move  workspace.rename
worktree.create  worktree.list  worktree.open  worktree.remove
```

Note method names use **underscores**: `pane.send_text`, `pane.send_keys`, `pane.move`,
etc. (the spec prose sometimes hyphenates — the wire uses underscores).

## Key request param shapes (from schema $defs)

```jsonc
// agent.start  (AgentStartParams) — herdr creates the tab+pane; returned ids authoritative
{ "name": string,            // required
  "argv": string[],          // required — the provider CLI + flags
  "cwd": string|null,
  "env": { [k:string]: string },   // ← the §5.3 ORCR_* env contract goes here
  "focus": bool = false,     // orcr always false
  "split": "right"|"down"|null,
  "tab_id": string|null,
  "workspace_id": string|null }   // ← target the owned session's workspace

// workspace.create  (WorkspaceCreateParams)
{ "label": string|null, "cwd": string|null, "env": {..}, "focus": bool=false }

// tab.create  (TabCreateParams)   — NOTE: spec says orcr does NOT pre-create tabs
{ "workspace_id": string|null, "label": string|null, "cwd": string|null,
  "env": {..}, "focus": bool=false }

// pane.move  (PaneMoveParams)
{ "pane_id": string, "focus": bool=false,
  "destination": PaneMoveDestination }
// PaneMoveDestination is a tagged union (oneOf on "type"):
//   { "type":"tab", "tab_id":string, "split":"right"|"down", "target_pane_id":string|null, "ratio":number|null }
//   { "type":"new_tab", "workspace_id":string|null, "label":string|null }
//   { "type":"new_workspace", "tab_label":string|null, "label":string|null }
// → for GC park: move pane to the `idle` workspace. "new_tab" with workspace_id=idle
//   creates a tab in idle; or ensure idle workspace + move. Verify which form parks
//   cleanly and lets un-park recreate the home tab. (M4)

// pane.close  / pane.get   (PaneTarget)    { "pane_id": string }
// pane.send_text (PaneSendTextParams)       { "pane_id": string, "text": string }
// pane.send_keys (PaneSendKeysParams)       { "pane_id": string, "keys": string[] }
//   → the two-call rule (§5.6): send_text, wait ~1s, send_keys ["Enter"] (verify key name)
// pane.list (PaneListParams)  { "workspace_id": string|null }
// agent.list / workspace.list / session.snapshot  → EmptyParams  {}
// agent.get (AgentTarget)   { "target": string }
// events.subscribe (EventsSubscribeParams) { "subscriptions": Subscription[] }
//   Subscription is oneOf {type:"workspace.created"|"pane.created"|"pane.closed"|
//     "pane.exited"|"pane.moved"|"pane.agent_..."| ...}
// notification.show { "title": string, "body": string|null, "sound": ..., "position": ... }
// ping  {}
```

## Key result shapes (from success_response $defs)

```jsonc
// AgentInfo (agent.list rows; also in session.snapshot.agents / .panes)
{ "terminal_id": string,     // ← GLOBALLY UNIQUE, never reused — unmanaged identity key (§5.7)
  "agent_status": "idle"|"working"|"blocked"|"done"|"unknown",   // ← the only 5 states
  "workspace_id": string, "tab_id": string, "pane_id": string,   // e.g. "w3","w3:t1","w3:p1"
  "focused": bool, "revision": uint64,
  "agent": string|null,        // provider name e.g. "claude"
  "display_agent": string|null,
  "agent_session": null | { "source":string, "agent":string,
                            "kind":"id"|"path", "value":string },  // ← transcript pointer (§5.6)
  "cwd": string|null, "foreground_cwd": string|null,  // pane cwd is authoritative (§5.2)
  "name": string|null, "title": string|null,
  "state_labels": { [k]:string },
  "custom_status": string|null, "screen_detection_skipped": bool }
```

- **`agent_status` enum is exactly `idle | working | blocked | done | unknown`.** This
  is the raw herdr signal. Per spec §5.6: normalize `done` → treat as `idle` for the
  completion check, and as `ended` only when pane closure is also observed; `unknown`
  is herdr's catch-all (and the permanent status when the provider's herdr integration
  is absent).
- `agent_session.value` + `kind` (`id`|`path`) locates the provider's native
  transcript — the basis for `logs` (M3). Example claude row:
  `{"source":"herdr:claude","agent":"claude","kind":"id","value":"<uuid>"}`.
- Pane/terminal id format: `workspace_id`="w3", `tab_id`="w3:t1", `pane_id`="w3:p1",
  `terminal_id`="term_65682c97505bd1".

## session.snapshot result (the whole session tree in one doc)

`herdr api snapshot` → `result.snapshot` contains:
`{ agents:[AgentInfo…], panes:[PaneInfo…], tabs:[TabInfo…], workspaces:[…],
   layouts:[…], focused_pane_id, focused_tab_id, focused_workspace_id, protocol, … }`.
`panes[]` includes non-agent panes too (plain shells show `agent_status:"unknown"`).
Use this for the reconciler's "what does herdr actually show" side (§11.5) and for
`ls`/`top` snapshots — scoped to the socket's session.

## Events (M1/M3/M4/M6 depend on this)

`events.subscribe { subscriptions:[{type:"pane.created"}, {type:"pane.closed"},
{type:"pane.exited"}, {type:"pane.moved"}, ...] }`. Event payloads use an `EventData`
tagged union with `type` consts like `pane_created`, `pane_closed`, `pane_exited`,
`workspace_created`, `workspace_closed`, `tab_created`, plus agent/pane state-change
kinds (inspect `schemas.event.$defs.EventData` fully in M0). `events.wait` is a
one-shot variant. These are HERDR's events — distinct from orcr's own `events` table
(§12); orcr's driver consumes herdr events to update the store and emits its own.

## empty-workspace auto-removal (verify in M0)

Spec §5.2: herdr removes a workspace once it has no panes; orcr must always **close
panes** it's done with (closing the last pane closes the tab; emptying the workspace
removes it). M0 acceptance: create pane → close pane → workspace gone. Verify the exact
behavior on the disposable session.

## Safe probing rules for all agents

- **NEVER** operate on the user's `default` session or create panes in it. All e2e /
  probing uses a **disposable** session name (e.g. `orcr_test_<rand>`) and cleans up
  with `herdr session stop <name>` + `herdr session delete <name>` in a drop-guard.
- **NEVER** use `~/.orcr` for tests — always set `ORCR_HOME` to a throwaway tempdir.
- The user has a live claude agent running in the `default` session (that's this
  orchestration). Do not touch it.
