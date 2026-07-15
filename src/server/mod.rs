//! The orcr server: the single-writer process behind the socket API.
//!
//! Runtime model (decided here in M1; see `m1-server-protocol/notes.md`): a **threaded,
//! blocking** design, not tokio. rusqlite is synchronous and the store is a single writer,
//! so a `Mutex<Store>` plus one thread per connection is both simpler and a natural fit;
//! subscription fan-out is a pump thread per subscription writing to a per-connection
//! shared writer. Wakeups ride the [`EventBus`] condvar so nothing busy-polls the store.

pub mod client;
mod completion;
mod discovery;
mod engine;
mod gc;
mod log;
mod loops;
pub(crate) mod params;

pub use client::{Client, StartOutcome};
pub use log::ServerLog;

use crate::api;
use crate::config::Config;
use crate::driver::{HerdrBinary, HerdrDriver};
use crate::error::{OrcrError, Result};
use crate::events::{EventBus, WaitOutcome};
use crate::home::Home;
use crate::lock::InstanceLock;
use crate::store::{now_millis, AgentFull, Store};
use crate::wire::{
    err_response, event_frame, ok_response, read_frame, write_frame, Request, ORCR_PROTOCOL,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Default bounded replay retention (events kept for subscription replay).
/// Overridable via `ORCR_EVENT_RETENTION` (tests use a small value to force expiry).
const DEFAULT_EVENT_RETENTION: i64 = 10_000;

/// How often the accept loop wakes to re-check the shutdown flag.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// How often a subscription pump wakes to re-check its stop flag when idle.
const SUB_POLL: Duration = Duration::from_millis(500);
/// Grace window on graceful stop for subscription pumps to flush `server_stopping`.
const SUB_FLUSH_GRACE: Duration = Duration::from_millis(150);

/// A shared, lockable writer over one connection's socket (responses + interleaved events).
type SharedWriter = Arc<Mutex<UnixStream>>;

/// The running server. Cheaply clonable (shared inner) so each connection thread owns a
/// handle.
#[derive(Clone)]
pub struct Server {
    inner: Arc<ServerInner>,
}

struct ServerInner {
    config: Config,
    home: Home,
    store: Mutex<Store>,
    bus: EventBus,
    log: ServerLog,
    /// The owned herdr session's driver, connected lazily and cached (reconnected on error).
    driver: Mutex<Option<HerdrDriver>>,
    started_at: i64,
    pid: u32,
    retention: i64,
    debug_methods: bool,
    shutdown: AtomicBool,
    socket_path: PathBuf,
    store_path: PathBuf,
    sub_counter: AtomicU64,
    /// Serializes owned-session workspace creation so concurrent spawns under one level-1
    /// segment never create duplicate workspaces.
    spawn_lock: Mutex<()>,
    /// The latest reconciler drift snapshot for `server status`.
    drift: Mutex<gc::DriftSnapshot>,
    /// Cumulative count of moves the reconciler completed or rolled back.
    repaired: AtomicU64,
    /// Per-agent move mutexes: serialize a GC park/un-park against a concurrent `send`
    /// un-park for the *same* agent so a two-phase move is never pre-empted mid-flight
    /// (a park committing `begin_move` must not race a send's recovery/deliver).
    move_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Server {
    fn log(&self) -> &ServerLog {
        &self.inner.log
    }

    /// Append an event, wake subscribers, and enforce bounded retention. This is the entry
    /// point future producers (and the M1 debug emitter) use.
    pub fn emit_event(&self, kind: &str, ref_uuid: Option<&str>, payload: &Value) -> Result<i64> {
        let seq = {
            let mut store = self.inner.store.lock().unwrap();
            store.append_event(kind, ref_uuid, payload)?
        };
        self.publish(seq);
        Ok(seq)
    }

    /// Wake subscribers up to `seq` and enforce bounded retention. Producers that append
    /// events *inside* a store transaction call this afterward with the highest event seq
    /// they wrote (0 = nothing to publish).
    pub fn publish(&self, seq: i64) {
        if seq <= 0 {
            return;
        }
        self.inner.bus.published(seq);
        let (latest, oldest) = self.inner.bus.cursor();
        if latest - oldest + 1 > self.inner.retention {
            let new_oldest = {
                let mut store = self.inner.store.lock().unwrap();
                match store.trim_events(self.inner.retention) {
                    Ok(o) => o,
                    Err(e) => {
                        self.log().warn(format!("event trim failed: {e}"));
                        return;
                    }
                }
            };
            self.inner.bus.set_oldest_retained(new_oldest);
        }
    }
}

/// Run the server in the foreground (the `--foreground` path; also what a service unit
/// runs). Returns `Ok(StartOutcome::AlreadyRunning)` without serving if another server
/// already holds the instance lock and becomes/was ready; otherwise binds, serves until a
/// graceful stop or signal, and returns `Ok(StartOutcome::Started)` on clean shutdown.
pub fn run_foreground(home: &Home, config: Config) -> Result<StartOutcome> {
    // Fast path: a healthy server already answers — nothing to do.
    let client = Client::new(home.socket_path());
    if client.handshake().is_ok() {
        return Ok(StartOutcome::AlreadyRunning);
    }

    // Race for the instance lock. The winner is the one true server; a loser waits for the
    // winner's readiness and reports already_running (idempotent start).
    let lock = match InstanceLock::try_acquire(home.lock_path())? {
        Some(l) => l,
        None => {
            if client.wait_for_ready(Duration::from_secs(15)).is_ok() {
                return Ok(StartOutcome::AlreadyRunning);
            }
            return Err(OrcrError::environment(
                "server_start_failed",
                "another process holds the instance lock but no server became ready",
            ));
        }
    };

    // We hold the lock: we are the server. Open the store (only under the lock).
    let store = Store::open(home.store_path())?;
    let retention = std::env::var("ORCR_EVENT_RETENTION")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_EVENT_RETENTION);

    let latest = store.latest_event_seq()?;
    let oldest = store.oldest_event_seq()?.unwrap_or(latest + 1).max(1);
    let bus = EventBus::new(latest, oldest);

    let log = ServerLog::open(
        &home.logs_dir(),
        config.logs.max_bytes,
        config.logs.max_files,
    )?;

    // Bind the socket under a tight umask, having validated + cleared any stale socket
    // (safe because we hold the lock).
    let listener = bind_socket(&home.socket_path())?;

    let server = Server {
        inner: Arc::new(ServerInner {
            config,
            home: home.clone(),
            store: Mutex::new(store),
            bus,
            log,
            driver: Mutex::new(None),
            started_at: now_millis(),
            pid: std::process::id(),
            retention,
            debug_methods: std::env::var("ORCR_DEBUG_METHODS").as_deref() == Ok("1"),
            shutdown: AtomicBool::new(false),
            socket_path: home.socket_path(),
            store_path: home.store_path(),
            sub_counter: AtomicU64::new(0),
            spawn_lock: Mutex::new(()),
            drift: Mutex::new(gc::DriftSnapshot::default()),
            repaired: AtomicU64::new(0),
            move_locks: Mutex::new(HashMap::new()),
        }),
    };

    server.log().info(format!(
        "server started pid={} protocol={} socket={} store={}",
        server.inner.pid,
        ORCR_PROTOCOL,
        server.inner.socket_path.display(),
        server.inner.store_path.display(),
    ));

    // Install signal handlers so SIGTERM/SIGINT trigger a graceful stop.
    install_signal_handlers(&server);

    // Reconcile the store against herdr reality on start, then start the queue
    // engine (promotion + spawn pipelines + stuck-start guard).
    server.reconcile_on_start();
    server.recover_loops_on_start();
    server.start_queue_worker();
    server.start_completion_monitor();
    server.start_gc_engine();
    server.start_unmanaged_discovery();
    server.start_loop_scheduler();

    server.serve(listener);

    // Cleanup: unlink the socket, then release the lock (implicit on drop).
    let _ = std::fs::remove_file(&server.inner.socket_path);
    server.log().info("server stopped");
    drop(lock);
    Ok(StartOutcome::Started)
}

impl Server {
    /// The accept loop. Nonblocking + a short poll so the shutdown flag is honored
    /// promptly (there is no clean cross-thread interrupt for `accept`).
    fn serve(&self, listener: UnixListener) {
        listener
            .set_nonblocking(true)
            .expect("set listener nonblocking");
        loop {
            if self.inner.shutdown.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // The listener is nonblocking (for the shutdown poll); accepted
                    // connections must be blocking, or a large write_all would abort at the
                    // socket buffer boundary and a read would spuriously see WouldBlock.
                    if let Err(e) = stream.set_nonblocking(false) {
                        self.log()
                            .warn(format!("set connection blocking failed: {e}"));
                        continue;
                    }
                    let server = self.clone();
                    std::thread::spawn(move || server.handle_conn(stream));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    self.log().warn(format!("accept error: {e}"));
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
        // Wake every subscription so each emits `server_stopping` and exits, then give the
        // pump threads a brief grace window to flush that final frame before we exit.
        self.inner.bus.shutdown();
        std::thread::sleep(SUB_FLUSH_GRACE);
    }

    /// Handle one client connection: read requests, dispatch, write responses; subscriptions
    /// spawn pump threads that write to the same (mutex-guarded) socket.
    fn handle_conn(&self, stream: UnixStream) {
        let read_half = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => {
                self.log().warn(format!("connection clone failed: {e}"));
                return;
            }
        };
        let writer: SharedWriter = Arc::new(Mutex::new(stream));
        let sub_stops: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::new(Mutex::new(Vec::new()));

        let mut reader = BufReader::new(read_half);
        loop {
            match read_frame(&mut reader) {
                Ok(Some(bytes)) => {
                    let stop = self.handle_request(&bytes, &writer, &sub_stops);
                    if stop {
                        break;
                    }
                }
                Ok(None) => break, // client closed
                Err(e) => {
                    // Best-effort: report the framing error, then drop the connection.
                    let _ = write_to(&writer, &err_response(&Value::Null, &e));
                    break;
                }
            }
        }

        // Client gone (or server.stop): stop this connection's subscriptions.
        for s in sub_stops.lock().unwrap().iter() {
            s.store(true, Ordering::SeqCst);
        }
    }

    /// Dispatch one request. Writes the response itself (so streaming methods can write an
    /// initial response then start a pump). Returns `true` when the connection should end
    /// (a handled `server.stop`).
    fn handle_request(
        &self,
        bytes: &[u8],
        writer: &SharedWriter,
        sub_stops: &Arc<Mutex<Vec<Arc<AtomicBool>>>>,
    ) -> bool {
        let req = match Request::from_slice(bytes) {
            Ok(r) => r,
            Err(e) => {
                let _ = write_to(writer, &err_response(&Value::Null, &e));
                return false;
            }
        };

        // Version negotiation: every request declares the protocol.
        if req.protocol != ORCR_PROTOCOL {
            let _ = write_to(
                writer,
                &err_response(&req.id, &crate::wire::unsupported_version(req.protocol)),
            );
            return false;
        }

        // Simple request→response methods: each yields a `Result<Value>` written back at the
        // single write site below. Streaming / side-effecting methods fall through to the
        // second match.
        let simple: Option<Result<Value>> = match req.method.as_str() {
            "server.handshake" => Some(Ok(self.handshake_result())),
            "server.status" => Some(Ok(self.status_result())),
            "api.schema" => Some(Ok(api::schema_document())),
            "api.snapshot" => Some(Ok(self.build_snapshot().1)),
            "agent.run" => Some(self.handle_agent_run(&req.params)),
            "agent.send" => Some(self.handle_agent_send(&req.params)),
            "agent.wait" => Some(self.handle_agent_wait(&req.params)),
            "agent.ask" => Some(self.handle_agent_ask(&req.params)),
            "agent.logs" => Some(self.handle_agent_logs(&req.params)),
            "agent.kill" => Some(self.handle_agent_kill(&req.params)),
            "agent.ls" => Some(self.handle_agent_ls(&req.params)),
            "agent.attach.prepare" => Some(self.handle_agent_attach_prepare(&req.params)),
            "agent.attach.heartbeat" => Some(self.handle_agent_attach_heartbeat(&req.params)),
            "agent.attach.release" => Some(self.handle_agent_attach_release(&req.params)),
            "loop.create" => Some(self.handle_loop_create(&req.params)),
            "loop.pause" => Some(self.handle_loop_set_paused(&req.params, true)),
            "loop.resume" => Some(self.handle_loop_set_paused(&req.params, false)),
            "loop.rm" => Some(self.handle_loop_rm(&req.params)),
            "loop.ls" => Some(self.handle_loop_ls(&req.params)),
            "loop.logs" => Some(self.handle_loop_logs(&req.params)),
            "loop.run.start" => Some(self.handle_loop_run_start(&req.params)),
            "loop.run.stop" => Some(self.handle_loop_run_stop(&req.params)),
            "loop.run.ls" => Some(self.handle_loop_run_ls(&req.params)),
            _ => None,
        };
        if let Some(out) = simple {
            let _ = write_to(writer, &respond(&req.id, out));
            return false;
        }

        // Streaming / side-effecting / debug methods.
        match req.method.as_str() {
            "server.stop" => {
                let _ = write_to(
                    writer,
                    &ok_response(&req.id, json!({ "status": "stopping" })),
                );
                self.log()
                    .info("server.stop requested — shutting down gracefully");
                self.inner.shutdown.store(true, Ordering::SeqCst);
                self.inner.bus.shutdown();
                true
            }
            "events.subscribe" => {
                self.start_subscription(&req, writer, sub_stops, false);
                false
            }
            "watch.open" => {
                self.start_subscription(&req, writer, sub_stops, true);
                false
            }
            "__debug.delete_agent" if self.inner.debug_methods => {
                let uuid = req
                    .params
                    .get("uuid")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let out = {
                    let mut store = self.inner.store.lock().unwrap();
                    store.debug_delete_agent(uuid)
                };
                let _ = write_to(
                    writer,
                    &respond(&req.id, out.map(|_| json!({ "deleted": true }))),
                );
                false
            }
            "__debug.emit_event" if self.inner.debug_methods => {
                let kind = req
                    .params
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("debug.tick");
                let ref_uuid = req.params.get("ref_uuid").and_then(|v| v.as_str());
                let payload = req
                    .params
                    .get("payload")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match self.emit_event(kind, ref_uuid, &payload) {
                    Ok(seq) => {
                        let _ = write_to(writer, &ok_response(&req.id, json!({ "seq": seq })));
                    }
                    Err(e) => {
                        let _ = write_to(writer, &err_response(&req.id, &e));
                    }
                }
                false
            }
            other => {
                let err = match api::find_method(other) {
                    Some(m) if !m.implemented => OrcrError::server_error(
                        "unimplemented",
                        format!("method `{other}` is registered but not yet implemented"),
                    ),
                    Some(_) => OrcrError::server_error(
                        "unimplemented",
                        format!("method `{other}` has no live handler"),
                    ),
                    None => OrcrError::invalid_request(
                        format!("unknown method `{other}`"),
                        "unknown_method",
                    ),
                };
                let _ = write_to(writer, &err_response(&req.id, &err));
                false
            }
        }
    }

    /// Start an `events.subscribe` (watch=false) or `watch.open` (watch=true) subscription.
    fn start_subscription(
        &self,
        req: &Request,
        writer: &SharedWriter,
        sub_stops: &Arc<Mutex<Vec<Arc<AtomicBool>>>>,
        watch: bool,
    ) {
        let sub_id = format!(
            "sub-{}",
            self.inner.sub_counter.fetch_add(1, Ordering::SeqCst)
        );

        let since_seq: i64 = if watch {
            // watch.open pins the snapshot_seq and subscribes from it in one shot — the
            // durable events table IS the pin (we replay everything > snapshot_seq from it),
            // so no event between snapshot and subscribe can be missed.
            let (seq, snapshot) = self.build_snapshot();
            let _ = write_to(
                writer,
                &ok_response(
                    &req.id,
                    json!({ "subscription": sub_id, "snapshot_seq": seq, "snapshot": snapshot }),
                ),
            );
            seq
        } else {
            let since = req
                .params
                .get("since_seq")
                .and_then(|v| v.as_i64())
                .unwrap_or(0)
                .max(0);
            if self.inner.bus.is_expired(since) {
                let (_, oldest) = self.inner.bus.cursor();
                let err = OrcrError::server_error(
                    "cursor_expired",
                    format!(
                        "cursor since_seq={since} has fallen out of the retained window \
                         (oldest replayable seq is {oldest}); re-snapshot and resubscribe"
                    ),
                )
                .with_details(json!({ "cause": "cursor_expired", "oldest_seq": oldest }));
                let _ = write_to(writer, &err_response(&req.id, &err));
                return;
            }
            let _ = write_to(
                writer,
                &ok_response(
                    &req.id,
                    json!({ "subscription": sub_id, "from_seq": since }),
                ),
            );
            since
        };

        let stop = Arc::new(AtomicBool::new(false));
        sub_stops.lock().unwrap().push(stop.clone());
        self.spawn_pump(sub_id, since_seq, writer.clone(), stop);
    }

    /// The subscription pump: drain new events from the durable store, write frames, then
    /// sleep on the bus until more arrive. Exits on stop (client gone) or shutdown
    /// (emitting a final `server_stopping` frame).
    fn spawn_pump(
        &self,
        sub_id: String,
        since_seq: i64,
        writer: SharedWriter,
        stop: Arc<AtomicBool>,
    ) {
        let server = self.clone();
        std::thread::spawn(move || {
            let mut next = since_seq; // deliver events with seq > next
            loop {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                // If our cursor has fallen out of the retained window (extreme churn + a
                // slow/backed-up client), the next `events_since` would silently start at the
                // new oldest row, skipping the trimmed range. Signal `cursor_expired` and stop
                // so the client re-snapshots and resubscribes — mirroring the
                // subscribe-time check, which only ran once at subscribe.
                if server.inner.bus.is_expired(next) {
                    let (_, oldest) = server.inner.bus.cursor();
                    let frame = event_frame(
                        &sub_id,
                        0,
                        json!({ "kind": "cursor_expired", "oldest_seq": oldest }),
                    );
                    let _ = write_to(&writer, &frame);
                    return;
                }
                // Drain everything currently available.
                loop {
                    let rows = {
                        let store = server.inner.store.lock().unwrap();
                        store.events_since(next, 256)
                    };
                    let rows = match rows {
                        Ok(r) => r,
                        Err(e) => {
                            server
                                .log()
                                .warn(format!("subscription {sub_id} read error: {e}"));
                            return;
                        }
                    };
                    if rows.is_empty() {
                        break;
                    }
                    for row in rows {
                        let mut ev = match row.payload {
                            Value::Object(map) => Value::Object(map),
                            other => json!({ "data": other }),
                        };
                        if let Value::Object(map) = &mut ev {
                            map.insert("kind".to_string(), json!(row.kind));
                            if let Some(r) = &row.ref_uuid {
                                map.insert("ref_uuid".to_string(), json!(r));
                            }
                        }
                        let frame = event_frame(&sub_id, row.seq, ev);
                        if write_to(&writer, &frame).is_err() {
                            return; // client gone
                        }
                        next = row.seq;
                    }
                }
                // Wait for more.
                match server.inner.bus.wait_for(next + 1, SUB_POLL) {
                    WaitOutcome::Ready | WaitOutcome::TimedOut => continue,
                    WaitOutcome::ShuttingDown => {
                        let frame = event_frame(&sub_id, 0, json!({ "kind": "server_stopping" }));
                        let _ = write_to(&writer, &frame);
                        return;
                    }
                }
            }
        });
    }

    /// The readiness handshake result.
    fn handshake_result(&self) -> Value {
        json!({
            "pid": self.inner.pid,
            "protocol": ORCR_PROTOCOL,
            "store": self.inner.store_path.display().to_string(),
            "ready": true,
        })
    }

    /// A consistent snapshot: `snapshot_seq` = current max event seq, computed under the
    /// store lock so a subscriber resuming from it sees every later event.
    fn build_snapshot(&self) -> (i64, Value) {
        let store = self.inner.store.lock().unwrap();
        let seq = store.latest_event_seq().unwrap_or(0);
        let all = store
            .list_agents(&crate::store::AgentFilter {
                include_ended: false,
                ..Default::default()
            })
            .unwrap_or_default();
        let agents: Vec<Value> = all.iter().map(|a| agent_row_json(&store, a)).collect();
        let queue: Vec<Value> = all
            .iter()
            .filter(|a| a.status == "queued")
            .map(|a| json!({ "uuid": a.uuid, "path": a.path, "agent": a.agent }))
            .collect();
        let loops: Vec<Value> = store
            .list_loops(&[], None, false)
            .unwrap_or_default()
            .iter()
            .map(|l| {
                let mut row = loops::loop_row_json(l);
                // Active runs (running/stopping) become the loop's subtree in `top`.
                let runs: Vec<Value> = store
                    .active_runs(&l.uuid)
                    .unwrap_or_default()
                    .iter()
                    .map(|r| {
                        json!({
                            "uuid": r.uuid,
                            "run_id": r.run_id,
                            "kind": r.kind,
                            "status": r.status,
                            "due_at": r.due_at,
                            "started_at": r.started_at,
                        })
                    })
                    .collect();
                row["runs"] = json!(runs);
                row
            })
            .collect();
        let snap = json!({
            "snapshot_seq": seq,
            "agents": agents,
            "loops": loops,
            "queue": queue,
        });
        (seq, snap)
    }

    /// `server.status`. herdr reachability is probed best-effort: the
    /// binary is discovered and, if the owned session is already running, pinged — status
    /// never *starts* a herdr server.
    fn status_result(&self) -> Value {
        let herdr = self.herdr_health();
        let integrations = self.integration_state();

        let drift = self.inner.drift.lock().unwrap().clone();
        let counts = self.counts(&drift).unwrap_or_else(|_| {
            json!({ "live": 0, "queued": 0, "blocked": 0, "unmanaged": 0,
                    "unmarked_panes": 0, "unknown_marked_panes": 0 })
        });

        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "protocol": ORCR_PROTOCOL,
            "pid": self.inner.pid,
            "uptime_ms": now_millis() - self.inner.started_at,
            "socket": self.inner.socket_path.display().to_string(),
            "store": self.inner.store_path.display().to_string(),
            "herdr": herdr,
            "integrations": integrations,
            "counts": counts,
            // Whether loop firing survives a reboot: true only when `server enable` has
            // registered a launchd/systemd unit. The scheduler always runs while
            // the server is up; this reflects the durable start-at-login registration.
            "loops_firing": crate::service::is_enabled(&self.inner.home),
            "loops": self.loops_status(),
            "drift": {
                "lost": drift.lost,
                "repaired": self.inner.repaired.load(Ordering::SeqCst),
                "unknown_marked_panes": drift.unknown_marked_panes,
                "unmarked_panes": drift.unmarked_panes,
            },
        })
    }

    fn counts(&self, drift: &gc::DriftSnapshot) -> Result<Value> {
        let c = self.inner.store.lock().unwrap().status_counts()?;
        Ok(json!({
            "live": c.live,
            "queued": c.queued,
            "blocked": c.blocked,
            "unmanaged": c.unmanaged,
            "unmarked_panes": drift.unmarked_panes,
            "unknown_marked_panes": drift.unknown_marked_panes,
        }))
    }

    fn herdr_health(&self) -> Value {
        let session = &self.inner.config.herdr.session;
        let bin = match HerdrBinary::discover(Some(self.inner.config.herdr.bin.as_str())) {
            Ok(b) => b,
            Err(_) => {
                return json!({
                    "bin": null, "reachable": false, "version": null, "protocol": null,
                    "socket": null, "session": session, "session_running": false,
                });
            }
        };
        // Find the owned session; ping only if it is already running (never auto-start).
        let (socket, running) = match bin.find_session(session) {
            Ok(Some(s)) if s.running => (s.socket_path.clone(), true),
            _ => (None, false),
        };
        let (version, protocol) = match &socket {
            Some(sock) => match HerdrDriver::connect(sock).and_then(|d| d.ping()) {
                Ok(pong) => (Some(pong.version), Some(pong.protocol)),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        json!({
            "bin": bin.path().display().to_string(),
            "reachable": true,
            "version": version,
            "protocol": protocol,
            "socket": socket,
            "session": session,
            "session_running": running,
        })
    }

    /// Active/paused loops + their next fires for `server status`.
    fn loops_status(&self) -> Value {
        let store = self.inner.store.lock().unwrap();
        let loops = store.list_loops(&[], None, false).unwrap_or_default();
        let rows: Vec<Value> = loops
            .iter()
            .map(|l| {
                json!({
                    "name": l.name,
                    "status": l.status,
                    "next_fire_at": l.next_fire_at,
                })
            })
            .collect();
        Value::Array(rows)
    }

    fn integration_state(&self) -> Value {
        let state = self.integration_state_typed();
        let mut map = serde_json::Map::new();
        for p in &state.providers {
            map.insert(
                p.provider.clone(),
                json!({ "orcr": p.orcr, "herdr": p.herdr }),
            );
        }
        Value::Object(map)
    }
}

/// The flat `agent ls` / snapshot row for an agent. `queue_position` and
/// `parent_path` are derived (never stored).
fn agent_row_json(store: &Store, a: &AgentFull) -> Value {
    let mut row = json!({
        "uuid": a.uuid,
        "path": a.path,
        "status": a.status,
        "managed": a.managed,
        "agent": a.agent,
        "model": a.model,
        "cwd": a.cwd,
        "pane_id": a.pane_id,
        "move_state": a.move_state,
        "created_at": a.created_at,
        "last_status_change_at": a.last_status_change_at,
    });
    // Fields the `top` tree needs to place/annotate a row: the herdr session for
    // unmanaged grouping, and the clocks that drive the age column per status.
    if let Some(s) = &a.herdr_session {
        row["herdr_session"] = json!(s);
    }
    if let Some(t) = a.starting_at {
        row["starting_at"] = json!(t);
    }
    if let Some(t) = a.idle_since {
        row["idle_since"] = json!(t);
    }
    if let Some(t) = a.parked_at {
        row["parked_at"] = json!(t);
    }
    if a.status == "queued" {
        if let Ok(Some(q)) = store.queue_position(&a.uuid) {
            row["queue_position"] = json!(q);
        }
    }
    if let Some(pid) = &a.parent_id {
        row["parent_id"] = json!(pid);
        if let Ok(Some(parent)) = store.agent_full(pid) {
            row["parent_path"] = json!(parent.path);
        }
    }
    if let Some(bk) = &a.blocked_kind {
        row["blocked_kind"] = json!(bk);
    }
    if let Some(er) = &a.exit_reason {
        row["exit_reason"] = json!(er);
    }
    if let Some(ea) = a.ended_at {
        row["ended_at"] = json!(ea);
    }
    row
}

/// Turn a handler `Result<Value>` into a wire response envelope correlated by `id`.
fn respond(id: &Value, out: Result<Value>) -> Value {
    match out {
        Ok(v) => ok_response(id, v),
        Err(e) => err_response(id, &e),
    }
}

/// Write one frame to a shared writer, locking it for the duration.
fn write_to(writer: &SharedWriter, value: &Value) -> Result<()> {
    let mut w = writer
        .lock()
        .map_err(|_| OrcrError::server_error("poisoned", "connection writer lock poisoned"))?;
    write_frame(&mut *w, value)
}

/// Bind the Unix socket with a tight umask (mode 0600), lstat-validating the path (no
/// symlinks) and clearing a stale socket first. Caller MUST hold the instance lock.
fn bind_socket(path: &Path) -> Result<UnixListener> {
    validate_socket_path(path)?;
    // A pre-existing socket here (same-uid, validated) is stale — we hold the lock, so no
    // live server owns it; unlink it before binding.
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| {
            OrcrError::environment(
                "server_start_failed",
                format!("cannot remove stale socket {}: {e}", path.display()),
            )
        })?;
    }
    // SAFETY: umask is process-global; we set a tight mask, bind, then restore.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(path);
    unsafe { libc::umask(old) };
    let listener = listener.map_err(|e| {
        OrcrError::environment(
            "server_start_failed",
            format!("cannot bind socket {}: {e}", path.display()),
        )
    })?;
    // Belt-and-suspenders: force 0600 regardless of umask.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
}

/// Reject a socket path that is a symlink or owned by another uid. A missing
/// path is fine (we are about to create it). A non-socket file is an error.
pub fn validate_socket_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(OrcrError::environment(
                    "unsafe_home",
                    format!("socket path {} is a symlink; refusing", path.display()),
                ));
            }
            use std::os::unix::fs::{FileTypeExt, MetadataExt};
            let uid = unsafe { libc::getuid() };
            if md.uid() != uid {
                return Err(OrcrError::environment(
                    "unsafe_home",
                    format!(
                        "socket path {} is owned by uid {}, not {uid}; refusing",
                        path.display(),
                        md.uid()
                    ),
                ));
            }
            if !md.file_type().is_socket() {
                return Err(OrcrError::environment(
                    "unsafe_home",
                    format!(
                        "path {} exists and is not a socket; refusing to overwrite",
                        path.display()
                    ),
                ));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(OrcrError::environment(
            "server_start_failed",
            format!("cannot stat socket path {}: {e}", path.display()),
        )),
    }
}

/// Install SIGTERM/SIGINT handlers that request a graceful shutdown.
fn install_signal_handlers(server: &Server) {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        let _ = signal_hook::flag::register(sig, flag.clone());
    }
    let server = server.clone();
    std::thread::spawn(move || loop {
        if flag.load(Ordering::SeqCst) {
            server
                .log()
                .info("received termination signal — shutting down gracefully");
            server.inner.shutdown.store(true, Ordering::SeqCst);
            server.inner.bus.shutdown();
            return;
        }
        if server.inner.shutdown.load(Ordering::SeqCst) {
            return; // shut down by another path; nothing to watch
        }
        std::thread::sleep(Duration::from_millis(100));
    });
}
