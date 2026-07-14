//! M1 acceptance tests for the server + socket protocol. These run **without herdr** —
//! they exercise the single-instance/auto-start machinery, the schema, the event
//! subscription contract, and log streaming, all over a throwaway `ORCR_HOME` tempdir.
//!
//! Each test spawns the real `orcr` binary (so auto-start's `current_exe` is `orcr`, not
//! the test harness) and cleans up its server at the end. No test touches `~/.orcr` or any
//! herdr session.

use orchestratr::home::Home;
use orchestratr::server::Client;
use serde_json::{json, Value};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn orcr_bin() -> &'static str {
    env!("CARGO_BIN_EXE_orcr")
}

/// A throwaway home: `tempfile::tempdir()` is 0700 (mkdtemp), which satisfies the safety
/// check, so we can point `ORCR_HOME` straight at it.
struct TestHome {
    dir: tempfile::TempDir,
}

impl TestHome {
    fn new() -> TestHome {
        TestHome {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }
    fn home(&self) -> Home {
        Home::at(self.dir.path())
    }
    fn client(&self) -> Client {
        Client::new(self.home().socket_path())
    }
    /// Run `orcr <args>` against this home with extra env, returning the output.
    fn run(&self, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut cmd = Command::new(orcr_bin());
        cmd.args(args)
            .env("ORCR_HOME", self.dir.path())
            .stdin(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("run orcr")
    }
    /// Start the server (idempotent) and wait for it to be ready.
    fn start_server(&self, env: &[(&str, &str)]) {
        let out = self.run(&["server", "start"], env);
        assert!(
            out.status.success(),
            "server start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        self.client()
            .wait_for_ready(Duration::from_secs(10))
            .expect("server ready");
    }
    fn stop_server(&self) {
        let _ = self.run(&["server", "stop"], &[]);
    }
}

fn handshake_pid(client: &Client) -> Option<u32> {
    client
        .handshake()
        .ok()
        .and_then(|v| v.get("pid").and_then(|p| p.as_u64()).map(|n| n as u32))
}

// --- Acceptance: race → exactly one server, all healthy ---

#[test]
fn race_auto_start_yields_one_server() {
    let th = TestHome::new();
    let home = th.home();
    let n = 8;

    // N processes race to auto-start the server simultaneously (each `server start` is the
    // auto-start path: handshake → lose the lock → wait for readiness).
    let handles: Vec<_> = (0..n)
        .map(|_| {
            let dir = home.root().to_path_buf();
            std::thread::spawn(move || {
                Command::new(orcr_bin())
                    .args(["server", "start"])
                    .env("ORCR_HOME", &dir)
                    .stdin(Stdio::null())
                    .output()
                    .expect("run")
            })
        })
        .collect();

    for h in handles {
        let out = h.join().unwrap();
        assert!(
            out.status.success(),
            "a racer failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // All clients see one healthy server.
    let client = th.client();
    let pid = handshake_pid(&client).expect("server should be healthy after the race");
    assert!(pid > 0);
    // Stable across probes (same single server).
    assert_eq!(handshake_pid(&client), Some(pid));

    // Uniqueness proof: kill that one pid → the socket must go dead. If a second server had
    // bound, one would still answer.
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    let gone = wait_until(Duration::from_secs(5), || client.handshake().is_err());
    assert!(gone, "no server should answer after killing the single pid");
}

// --- Acceptance: kill -9 → next client restarts cleanly; store uncorrupted ---

#[test]
fn kill_dash_9_then_restart() {
    let th = TestHome::new();
    th.start_server(&[]);
    let client = th.client();
    let pid1 = handshake_pid(&client).expect("first server up");

    // Hard-kill the server. The lock (advisory) releases on process death; the socket file
    // is left stale.
    let _ = Command::new("kill")
        .args(["-9", &pid1.to_string()])
        .status();
    assert!(
        wait_until(Duration::from_secs(5), || client.handshake().is_err()),
        "server should be unreachable after kill -9"
    );

    // Next call restarts cleanly: new pid, stale socket cleared, store still usable (WAL).
    th.start_server(&[]);
    let pid2 = handshake_pid(&client).expect("restarted server up");
    assert_ne!(pid1, pid2, "restart should be a fresh server");

    // Store uncorrupted: a snapshot round-trips with a valid snapshot_seq.
    let snap = client.request("api.snapshot", json!({})).expect("snapshot");
    assert!(snap.get("snapshot_seq").and_then(|v| v.as_i64()).is_some());

    th.stop_server();
}

// --- Acceptance: api schema validates as JSON Schema and covers 100% of methods ---

#[test]
fn api_schema_validates_and_is_complete() {
    let th = TestHome::new();
    // `api schema` is generated locally (no server needed) — mirrors herdr's offline schema.
    let out = th.run(&["api", "schema"], &[]);
    assert!(out.status.success());
    let doc: Value = serde_json::from_slice(&out.stdout).expect("schema is JSON");

    // The whole document must itself be a valid JSON Schema (compiles under the meta-schema).
    jsonschema::JSONSchema::compile(&doc).expect("schema document is a valid JSON Schema");

    // Every registered method appears, and each method's params/result is a valid subschema.
    let methods = orchestratr::api::methods();
    let schema_methods = doc["methods"].as_object().expect("methods object");
    assert_eq!(
        schema_methods.len(),
        methods.len(),
        "schema must cover 100% of registered methods"
    );
    for m in &methods {
        let entry = schema_methods
            .get(m.name)
            .unwrap_or_else(|| panic!("schema missing method {}", m.name));
        jsonschema::JSONSchema::compile(&entry["params"])
            .unwrap_or_else(|_| panic!("params schema for {} is invalid", m.name));
        jsonschema::JSONSchema::compile(&entry["result"])
            .unwrap_or_else(|_| panic!("result schema for {} is invalid", m.name));
    }

    // The socket method returns the identical document.
    th.start_server(&[]);
    let via_socket = th
        .client()
        .request("api.schema", json!({}))
        .expect("api.schema");
    assert_eq!(
        via_socket, doc,
        "socket api.schema must match generated schema"
    );
    th.stop_server();
}

// --- Acceptance: subscription replay/live with no gaps or dups; cursor_expired ---

#[test]
fn subscription_replay_live_and_cursor_expired() {
    let th = TestHome::new();
    // Debug emitter on; tiny retention so we can force cursor_expired.
    th.start_server(&[("ORCR_DEBUG_METHODS", "1"), ("ORCR_EVENT_RETENTION", "5")]);
    let client = th.client();

    let emit = |i: i64| {
        client
            .request(
                "__debug.emit_event",
                json!({ "kind": "debug.tick", "payload": { "i": i } }),
            )
            .unwrap_or_else(|e| panic!("emit {i}: {e}"))
    };

    // Emit 3 events, then subscribe from 0 → replay all 3 in order, no gaps/dups.
    for i in 1..=3 {
        emit(i);
    }
    let (init, mut sub) = client
        .open_stream("events.subscribe", json!({ "since_seq": 0 }))
        .expect("subscribe");
    assert_eq!(init["from_seq"], 0);
    sub.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    let mut seen: Vec<i64> = Vec::new();
    // Live-emit 2 more while subscribed; expect to receive all 5 (seq 1..=5) exactly once.
    for i in 4..=5 {
        emit(i);
    }
    while seen.len() < 5 {
        let ev = sub.next_event().expect("event").expect("stream open");
        let seq = ev["seq"].as_i64().unwrap();
        assert_eq!(ev["event"]["kind"], "debug.tick");
        seen.push(seq);
    }
    assert_eq!(
        seen,
        vec![1, 2, 3, 4, 5],
        "no gaps, no duplicates, in order"
    );
    drop(sub);

    // Force the retained window forward: emit well past the retention cap so old seqs drop.
    for i in 6..=20 {
        emit(i);
    }
    // Resuming from an old cursor now fails with cursor_expired.
    let expired = client.open_stream("events.subscribe", json!({ "since_seq": 1 }));
    match expired {
        Err(e) => {
            assert_eq!(
                e.details["cause"], "cursor_expired",
                "expected cursor_expired"
            );
        }
        Ok(_) => panic!("expected cursor_expired for an old cursor"),
    }

    // Re-snapshot recovers: watch.open gives a fresh snapshot_seq and a working subscription.
    let (winit, _wsub) = client
        .open_stream("watch.open", json!({}))
        .expect("watch.open");
    assert!(winit.get("snapshot_seq").and_then(|v| v.as_i64()).is_some());
    assert!(winit.get("subscription").is_some());

    th.stop_server();
}

// --- Graceful stop closes open subscriptions with a server_stopping frame ---

#[test]
fn graceful_stop_sends_server_stopping_to_subscribers() {
    let th = TestHome::new();
    th.start_server(&[]);
    let client = th.client();

    let (_init, mut sub) = client
        .open_stream("watch.open", json!({}))
        .expect("watch.open");
    sub.set_read_timeout(Some(Duration::from_secs(5))).unwrap();

    // Stop the server on another connection; the subscriber should get server_stopping.
    let _ = client.request("server.stop", json!({}));

    let mut saw_stopping = false;
    while let Ok(Some(ev)) = sub.next_event() {
        if ev["event"]["kind"] == "server_stopping" {
            saw_stopping = true;
            break;
        }
    }
    assert!(
        saw_stopping,
        "subscription should receive server_stopping on graceful stop"
    );
}

// --- Acceptance: server logs --follow streams live writes ---

#[test]
fn server_logs_follow_streams_live_writes() {
    use std::io::{BufRead, BufReader};

    let th = TestHome::new();
    th.start_server(&[]);

    // Start a follower.
    let mut follow = Command::new(orcr_bin())
        .args(["server", "logs", "--follow"])
        .env("ORCR_HOME", th.dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn follow");
    let stdout = follow.stdout.take().unwrap();

    // Read follower output on a thread; look for the graceful-stop line we will trigger.
    let handle = std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(10);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            if line.contains("shutting down gracefully") || line.contains("server.stop") {
                return true;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        false
    });

    // Give the follower a moment to attach, then trigger a fresh log write.
    std::thread::sleep(Duration::from_millis(500));
    th.stop_server();

    let saw = handle.join().unwrap();
    let _ = follow.kill();
    let _ = follow.wait();
    assert!(saw, "follow should stream the live server.stop log line");
}

/// Poll `f` until true or the deadline.
fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
