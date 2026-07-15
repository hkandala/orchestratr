# M1 · Server & protocol — implementation notes

Decision log: deviations from the spec, under-specified points resolved, behavioral
choices worth knowing, and discovered facts (especially about herdr). Reading all the
`notes.md` files should give full context on what changed vs the spec and why.
Capture *decisions and deviations*, not a play-by-play.

## Deviations from spec

- **`api schema` is generated client-side, not fetched from the server.** The schema is
  fully determined by the compiled method registry (`src/api.rs`), so `orcr api schema`
  builds it locally with no server/auto-start — exactly like `herdr api schema` works
  offline. `api.schema` is *also* a live socket method returning the identical document
  (tested for byte-equality), so "the socket API is the API" still holds; the CLI just
  avoids a needless round-trip.

## Decisions on under-specified points

- **Server runtime = threaded, blocking (not tokio).** rusqlite is synchronous and the
  store is a single writer, so `Mutex<Store>` + one thread per connection is the natural
  fit and avoids the async ecosystem. Subscriptions are one pump thread per subscription
  writing to a per-connection `Arc<Mutex<UnixStream>>` (responses + interleaved event
  frames share the guarded writer). Wakeups ride an `EventBus` condvar so nothing
  busy-polls the store. (M0 notes flagged this as an M1 decision.)
- **orcr socket protocol version = 1** (`wire::ORCR_PROTOCOL`), distinct from herdr's
  protocol 16. Every request declares `protocol`; a mismatch → `environment_error
  {cause: unsupported_version}`. Absent/unknown top-level fields are ignored (additive).
- **Single-instance model.** The server holds an exclusive `flock` on
  `$ORCR_HOME/orcr.lock` for its whole lifetime and refuses to open the store without it.
  `server start --foreground` *becomes* the server; `server start` (and every
  auto-starting verb) spawns a detached `--foreground` child (own session via `setsid`,
  `ORCR_HOME` passed explicitly) and waits for the readiness handshake. The start race is
  resolved entirely by the lock: the winner serves, losers `wait_for_ready` → report
  `already_running`. This makes "N racing auto-starts → exactly one server" fall out for
  free (proven in `tests/server_protocol.rs`).
- **Readiness handshake = `server.handshake`** (a cheap live method returning
  `{pid, protocol, store, ready}`). Kept separate from `server.status` so readiness polls
  don't probe herdr. Registered in the schema alongside the other live methods.
- **Event transport.** The `events` table is the durable cursor (source of truth);
  subscriber pumps read rows back from it, so `watch.open`'s "cursor pin" is simply
  reading everything `> snapshot_seq` from the durable table — no in-memory pin needed and
  no snapshot/subscribe gap. `EventBus` tracks `latest_seq` (wakeups) and
  `oldest_retained_seq` (for `cursor_expired`).
- **Bounded replay retention** defaults to 10 000 events, overridable via
  `ORCR_EVENT_RETENTION` (tests set a small value to force expiry). Trim runs after an
  append when `latest - oldest + 1 > retention`. A resume from `since_seq` where
  `since_seq + 1 < oldest_retained_seq` → `server_error {cause: cursor_expired}`; the
  client re-snapshots via `watch.open`.
- **Stub methods** (agent.*, loop.*) are registered with real param/result schemas but
  return `server_error {cause: unimplemented}` until their milestone. Unknown methods →
  `invalid_request {reason: unknown_method}`.
- **`__debug.emit_event`** is an internal, non-public method registered only when the
  server runs with `ORCR_DEBUG_METHODS=1`. It lets the subscription e2e drive the event
  transport before any real producers exist; it is excluded from `api schema`.
- **`server logs`** reads `logs/server.log` directly (JSON-per-line, size-capped rotation
  to `server.log.N`); `--follow` tails the file independently of the server (works after
  the server stops). `server stop` is idempotent (`not_running` if nothing answers) and
  never auto-starts.
- **`server enable/disable`** intentionally deferred to M5 (per this milestone's
  out-of-scope), matching the spec.

## Discovered facts / gotchas

- **Accepted connections inherit the listener's nonblocking flag** (macOS + Linux). The
  accept loop sets the *listener* nonblocking (to poll the shutdown flag), so each
  accepted `UnixStream` must be reset to blocking — otherwise a large `write_all` (e.g.
  the ~12 KB `api.schema` response) aborts at the ~8 KB socket send-buffer boundary with
  `WouldBlock`, and reads spuriously see `WouldBlock`. This bit the first test run
  (schema truncated at column ~8195); fixed by `stream.set_nonblocking(false)` right after
  `accept`.
- **AF_UNIX socket send buffer is ~8 KB** here, which is why the nonblocking bug surfaced
  precisely on the schema (the first >8 KB payload).

## Discovered facts / gotchas (cont.)

- **The race test could leak a revived server (test-hygiene fix, round 1).** The 8 racing
  `server start` procs each spawn a detached `--foreground` child. A child still in the tiny
  window *before* `try_acquire` (its fast-path handshake just failed) grabs the lock the
  instant the test `kill -9`s the winner and binds a **new** server — after the test's
  first-`is_err()` "gone" poll returns. The tempdir then deletes out from under it and it
  runs forever. Fix: `TestHome` now has a `reap_server()` (loop `kill -9` the
  handshake-reported pid — catches a mid-start child that `server stop` can't — until the
  socket stays dead for 8 consecutive probes, closing the revival window) invoked from a
  `Drop` guard on every test, and the race test asserts `reap_server()` reaches stable-dead.
  Verified: `cargo test --test server_protocol` run 5×, `pgrep -f 'orcr server start
  --foreground'` empty after each.

## Verifier & reviewer history

- **Implementation** (this pass, on `main`): wire protocol + framing → method registry +
  schema generator → store event methods → event bus → flock lock → threaded server
  (accept/dispatch/subscriptions/graceful-stop) + rotating log + client/auto-start → clap
  CLI (server + api) → acceptance tests. Green gates: `cargo build`, `cargo fmt --check`,
  `cargo clippy --all-targets -D warnings`, `cargo test` (unit + `tests/server_protocol`),
  and `ORCR_E2E=1 cargo test --test e2e` (5/5 against live herdr 0.7.2). Post-run
  `herdr session list` shows only the untouched `default` session.
- **Verify — round 1: FAIL → fixed → PASS.** The verifier found one concrete issue: the
  race test could leave an auto-start-revived `--foreground` orphan bound to a
  soon-deleted tempdir (a mid-start child grabbing the lock the instant the winner is
  `kill -9`ed). Resolved by `TestHome::reap_server()` + a `Drop` guard on every test that
  loops `kill -9` on the handshake-reported pid until the socket stays dead across 8
  probes, with the race test asserting stable-dead (commit `dea1d6c`). Re-verify: full
  suite green, `pgrep -f 'orcr server start --foreground'` empty after 5 repeat runs.
- **Review: PASS.** Code-review pass over the M1 surface (wire/framing, registry/schema
  generation, event bus + cursor retention, flock single-instance, threaded server +
  subscription pumps, client auto-start, CLI) found no blocking correctness/robustness/
  spec-adherence issues; the nonblocking-accept and orphan-reap fixes above were the
  substantive items and were already resolved.
- **Scribe — final green check** (2026-07-13, on `main`, clean tree): `cargo build` ok;
  `cargo fmt --check` ok; `cargo clippy --all-targets -- -D warnings` clean; `cargo test`
  green (unit + `handshake` + `home_config` + `server_protocol` 6/6 + `e2e` skip-path);
  `ORCR_E2E=1 cargo test --test e2e` 5/5 against live herdr 0.7.2;
  `ORCR_E2E=1 cargo test --test conformance_live` 1/1 (contract matches live
  `herdr api schema`). Post-run `herdr session list` shows only the untouched `default`
  session; no `--foreground` orphans. **M1 green.**
