# M0 · Foundations — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts (especially about herdr). Reading all the
`notes.md` files should give full context on what changed vs the spec and why.
Capture *decisions and deviations*, not a play-by-play.

## Deviations from spec

- **Sessions are per-socket, not one global socket** (already flagged in the driver
  reference). Confirmed against live herdr 0.7.2: `herdr session list --json` returns
  one row per session, each with its own `session_dir` + `socket_path`. The spec's
  "herdr's single socket manages all sessions" (§2, §4, §11.7) is wrong for this herdr.
  The driver therefore: (1) bootstraps the owned session's herdr server headless via
  the binary, discovers its socket via `herdr session list --json` (match name), and
  connects to *that* socket; (2) for cross-session enumeration, fans out over each
  session's `socket_path`. `session.snapshot`/`agent.list`/`pane.list`/`workspace.*`
  over a given socket are scoped to that socket's session; orcr attaches session
  identity itself (from which socket the row came).

## Decisions on under-specified points

- **M0 driver is synchronous/blocking** (`std::os::unix::net::UnixStream` with read/write
  timeouts). herdr uses **one request per connection** (see discovered facts), so there
  is no long-lived multiplexed connection to manage; each driver call opens a fresh
  connection, writes one request, reads one response, closes. The server's async/event
  story (§11.6, `events.subscribe`) is deferred to M1+.
- **Request envelope** includes `protocol` (`{protocol, id, method, params}`). herdr
  ignores unknown fields and does not require `protocol` on the wire, but we always send
  it (matches the spec and is future-proof).
- **Version handshake is orcr-side.** herdr does NOT reject a fabricated protocol number
  on requests (see discovered facts), so the driver enforces the check itself: it pings
  on connect, reads the `protocol` from the `pong` result, and rejects a mismatch with
  `environment_error {cause: unsupported_version}` (mapped to `herdr_unreachable`
  semantics). The acceptance test "handshake rejects a fabricated protocol number" is
  proven by pointing the handshake at a stub unix socket that returns a `pong` with a
  bad protocol number and asserting rejection.
- **Config unknown-key suggestion** uses Levenshtein distance against the known key set
  (threshold ≤ 2, nearest match) — no external crate.
- **Home safety check** uses `std::os::unix::fs::MetadataExt` (uid, mode) + `libc::getuid`.
  `unsafe_home` when the dir is not owned by the current uid or is group/world-writable
  (mode & 0o022 != 0).
- **Schema version** stamped in a `meta` table (`key='schema_version'`); opening a store
  with a different stamped version → `environment_error {cause: store_version_mismatch}`
  with a refusal message. `ORCR_SCHEMA_VERSION` starts at 1.

## Discovered facts / gotchas

- **herdr socket protocol = 16**, version `0.7.2`. `pong` result:
  `{type:"pong", version, protocol, capabilities:{live_handoff, detached_server_daemon}}`.
- **ONE REQUEST PER CONNECTION.** herdr closes the socket after sending one response.
  A second request on the same connection → `BrokenPipe`. The driver opens a fresh
  connection per request. (Confirmed by probing the default socket.)
- **herdr does not validate the request `protocol` field** — a `ping` with
  `protocol:999` still returns a normal `pong` (protocol 16). Version enforcement is
  entirely orcr's responsibility (read the reported protocol from `pong`).
- **Wire envelopes**: request `{protocol,id,method,params}`; success
  `{id, result:{type:"<tag>", ...}}` — `result` is a tagged union on `type`; error
  `{id, error:{code, message}}`. Newline-delimited JSON.
- **Result tags** used by the driver: `ping`→`pong`; `session.snapshot`→`session_snapshot`
  `{snapshot}`; `workspace.create`→`workspace_created` `{workspace,tab,root_pane}`;
  `workspace.list`→`workspace_list` `{workspaces}`; `agent.start`→`agent_started`
  `{agent,argv}`; `agent.list`→`agent_list` `{agents}`; `pane.list`→`pane_list` `{panes}`;
  `pane.get`→`pane_info` `{pane}`; `pane.move`→`pane_move` `{move_result}`;
  `pane.close`/`pane.send_text`/`pane.send_keys`→`ok` `{}`; `notification.show`→
  `notification_show` `{reason,shown}`.
- **AgentInfo** required: `terminal_id, agent_status, workspace_id, tab_id, pane_id,
  focused, revision`; optional `agent, agent_session{source,agent,kind,value},
  cwd, foreground_cwd, name, title, display_agent, custom_status, state_labels,
  screen_detection_skipped`. `agent_status` enum: `idle|working|blocked|done|unknown`.
- **PaneInfo** adds `label`, `scroll`; same identity/status fields.
- **WorkspaceInfo**: `workspace_id, number, label, focused, pane_count, tab_count,
  active_tab_id, agent_status, worktree?`.
- **Mock self-reporting to herdr**: `pane.report_agent {pane_id, source, agent, state,
  agent_session_id?, agent_session_path?, ...}` lets a pane report its own state
  (`PaneAgentState` = idle|working|blocked|unknown) and transcript pointer. This is how
  the mock provider "reports state through herdr's integration mechanism" without a
  herdr integration hook installed. The pane discovers its own pane via `pane.current`
  (or a herdr-injected env var, to be confirmed in the e2e harness).
- **PaneMoveDestination** `new_tab` variant only requires `type`; `{type:"new_tab",
  workspace_id}` moves a pane to a tab in the given workspace (used for GC park later).
- **herdr injects pane env** `HERDR_SESSION`, `HERDR_SOCKET_PATH`, `HERDR_PANE_ID`,
  `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, `HERDR_ENV=1` into every pane it starts. The mock
  self-discovers its socket + pane from `HERDR_SOCKET_PATH` / `HERDR_PANE_ID` — orcr does
  not need to inject them. (Confirmed by dumping a probe pane's env.)
- **herdr surfaces a reported `idle` (after `working`) as `done`.** When an integration
  reports `working` then `idle`, `pane.get`/`agent.list` show `agent_status:"done"` — this
  is herdr's turn-complete signal, and is exactly why spec §5.6 has orcr normalize
  `done`→`idle` for the completion check. Confirmed via probe and exercised in the e2e
  state-reporting test (it asserts on `normalize_done(status)`). `working` and `blocked`
  reports surface verbatim.
- **`herdr --session <name> server`** starts a session's headless server (foreground);
  orcr spawns it detached (stdio to null) and polls `session list --json` for readiness.
  There is no `herdr session start`. `session stop`/`session delete <name>` tear it down.
- **workspace.create adds a root shell pane.** A freshly created workspace has one pane;
  closing it removes the (now-empty) workspace. Empty-workspace auto-removal confirmed
  (create workspace → close its last pane → workspace gone), per spec §5.2.
- **e2e result types confirmed live**: `agent.start`→`agent_started`, `pane.get`→
  `pane_info`, `pane.send_text`/`send_keys`/`close`/`report_agent`→`ok`,
  `workspace.create`→`workspace_created`, `pane.move`→`pane_move`.

## Verifier & reviewer history

_(record each verify/review round's verdict + how issues were resolved)_
</invoke>
