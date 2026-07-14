# M1 · Server & protocol — todos

Ships: server process, single-instance lock + auto-start, socket API skeleton (full method
registry + self-describing schema), events + snapshot-then-subscribe.

## Foundations / reading
- [x] Read master-prompt.md + full spec.md (relevant §§) + this milestone + herdr-driver-reference.md + m0 notes
- [x] Decide server runtime (threaded blocking, not tokio) — recorded in notes.md

## Wire protocol (§11.6)
- [x] orcr wire envelopes: request `{protocol,id,method,params}`, response `{id,ok,result|error}`, event `{subscription,seq,event}`
- [x] `ORCR_PROTOCOL` constant (orcr's own protocol, distinct from herdr 16)
- [x] Newline-delimited JSON framing with max-frame-size enforcement
- [x] Version negotiation on request → `unsupported_version`; unknown fields ignored

## Method registry + api schema (§6.5, §11.6)
- [x] Method registry: full namespace registered (server.*, api.*, events.*, watch.*, agent.*, loop.*, loop.run.*)
- [x] Live handlers: server.handshake, server.status, server.stop, api.schema, api.snapshot, events.subscribe, watch.open
- [x] Stub handlers for all not-yet-implemented methods → `server_error {cause: unimplemented}`
- [x] `api schema` — versioned JSON Schema generated from the registry (validates as JSON Schema; 100% method coverage)
- [x] `api snapshot` — consistent state doc stamped with `snapshot_seq`

## Events (§11.6, §12)
- [x] `events` table as the durable subscription cursor; `Store::append_event` (same-txn) + `events_since`
- [x] Monotonic `events.seq`
- [x] EventBus (mutex+condvar) for live wakeups + retention bookkeeping
- [x] `events.subscribe {since_seq}` — replay then live stream; no gaps, no duplicates
- [x] Bounded replay retention → `cursor_expired`
- [x] `watch.open` — snapshot + subscription under one cursor pin (snapshot_seq), no re-snapshot livelock
- [x] server_stopping frame on graceful stop closes subscriptions

## Server process (§4, §6.4, §11.6)
- [x] `orcr server start` — idempotent (healthy handshake → already_running); blocks until readiness
- [x] `orcr server start --foreground` — becomes the server (service-unit mode)
- [x] `orcr server stop` — graceful: stop accepting, close subs (server_stopping), persist, release socket; never touches panes
- [x] `orcr server status` — version, protocol, socket/store paths, herdr reachability, counts (zeroes), integrations, config summary
- [x] `orcr server logs [--tail <n>] [--follow]` — reads server.log (structured lines, rotated)
- [x] Structured server logging to `logs/server.log` with size-cap rotation

## Single instance & auto-start (§11.6)
- [x] Exclusive `flock` lock file; server refuses to open the store without it
- [x] Unix socket at `$ORCR_HOME/orcr.sock`, umask 077 (mode 0600), lstat-validated (no symlinks)
- [x] Client auto-start path (used by every server-needing verb): validate socket via handshake → if absent/stale, spawn `--foreground` and wait for readiness
- [x] Stale sockets unlinked only under the lock and only if same-uid
- [x] Distinct failure errors: `server_unreachable`, `server_start_failed`, `herdr_unreachable`

## CLI plumbing (§6, §13)
- [x] clap-based CLI; server + api nouns wired; hidden `__m0-selfcheck` preserved
- [x] `--json` envelope machinery (`{"ok":true,result}` / `{"ok":false,error}`) to stdout; logs to stderr
- [x] §13 error enum + exit-code mapping across the CLI
- [x] Duration parsing (units required) available to CLI (reused from duration.rs)
- [x] TTY detection helper

## Acceptance criteria (prove each)
- [x] Race test: N processes auto-start simultaneously → exactly one server, all clients get a healthy handshake
- [x] `kill -9` the server → next client call restarts it cleanly; lock + stale socket handled; store uncorrupted (WAL)
- [x] `api schema` output validates as JSON Schema and covers 100% of registered methods
- [x] Subscription test: write events while a subscriber replays from `since_seq` → no gaps, no duplicates; expired cursor → `cursor_expired` → re-snapshot works
- [x] `server logs --follow` streams live writes

## Tests
- [x] Unit: wire framing, registry/schema coverage, event bus, cursor_expired logic, lock guard
- [x] Integration (no herdr): server start/stop/status/api over the socket; race; kill-9 restart; subscription; logs --follow
- [x] e2e (ORCR_E2E=1, live herdr): server.status reports herdr reachability + integrations against a disposable session

## Deferred / out of scope
- server enable/disable → M5 (per milestone out-of-scope)
- Agent/loop/queue/GC/reconciliation handler bodies → later milestones (registered as stubs here)
- Real event producers (agent/loop lifecycle) → later milestones; M1 proves the transport via a gated `__debug.emit_event` (not in the public schema)
</content>
</invoke>
