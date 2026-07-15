# M1 · Server & protocol

The single-writer server process and the socket API that everything else is a client
of. M1 is done when two racing clients always end up talking to exactly one healthy
server, and the protocol is self-describing.

## Scope

### Server process (spec §4, §6.4)
- `orcr server start` — idempotent (healthy handshake → `already_running`); blocks
  until readiness; `--foreground` mode for service units.
- `orcr server stop` — graceful control-plane stop: stop accepting requests, close
  subscriptions with `server_stopping`, persist state, release socket. Never touches
  panes/agents.
- `orcr server status` — version, protocol, socket/store paths, herdr reachability
  (binary, socket, session), counts (zeroes for now), config summary.
- `orcr server logs [--tail <n>] [--follow]` — reads `~/.orcr/logs/server.log`
  (structured lines, rotated).

### Single instance & auto-start (spec §11.6)
- Exclusive `flock` lock file in `ORCR_HOME`; server refuses to open the store
  without it.
- Client auto-start path (used by every verb): validate existing socket via
  handshake → if absent/stale, race for the lock; losers wait for readiness instead
  of spawning; stale sockets unlinked only under the lock and only if same-uid.
- Distinct failure errors: `server_unreachable`, `server_start_failed`,
  `herdr_unreachable`.

### Socket protocol (spec §11.6)
- Unix socket at `$ORCR_HOME/orcr.sock`, umask 077, lstat-validated (no symlinks).
- Newline-delimited JSON envelopes, one multiplexed connection: requests
  `{protocol, id, method, params}`; responses correlated by id; subscription events
  `{subscription, seq, event}` interleaved.
- Version negotiation on first request → `unsupported_version`; unknown fields
  ignored; max frame size enforced.
- Method registry: **the full method namespace is registered in M1** with typed
  param/result schemas — later milestones replace `unimplemented` stub handlers, so
  `api schema` is complete from day one and M7's SDK generation can't drift. Live in
  M1: `server.status`, `api.schema`, `api.snapshot`, `events.subscribe`,
  `watch.open` (snapshot + subscription under one cursor pin).

### api noun (spec §6.5)
- `orcr api schema` — versioned JSON Schema of all methods/params/results/events/
  error codes (generated from the method registry, not hand-written).
- `orcr api snapshot` — one consistent state document stamped with `snapshot_seq`.

### Events (spec §11.6, §12)
- `events` table as the subscription cursor: rows written in the same transaction as
  the change they describe; `events.seq` monotonic.
- `events.subscribe` with `since_seq`; snapshot-then-subscribe contract
  (`snapshot_seq` in every snapshot); bounded replay retention → `cursor_expired`.

### CLI plumbing
- `--json` envelope machinery (`{"ok":true,result}` / `{"ok":false,error}`), the §13
  error enum and exit-code mapping, duration parsing (units required), TTY detection.

## Acceptance

- Race test: N processes auto-start simultaneously → exactly one server, all clients
  get a healthy handshake.
- `kill -9` the server → next client call restarts it cleanly; lock and stale socket
  handled; store uncorrupted (WAL).
- `api schema` output validates as JSON Schema and covers 100% of registered methods.
- Subscription test: write events while a subscriber replays from `since_seq` →
  no gaps, no duplicates; expired cursor → `cursor_expired` → re-snapshot works.
- `server logs --follow` streams live writes.

## Out of scope

Agents, queue, GC, loops, top, unmanaged discovery. `server enable/disable` lands in
M5 (it exists to make loops fire after reboot).
