//! The socket client (spec §4, §11.6): the thin layer every CLI verb and the SDK sit on.
//!
//! Responsibilities: connect to `$ORCR_HOME/orcr.sock` and perform the version handshake;
//! send one-shot requests and decode the `{ok,result|error}` envelope back into a
//! [`Result`]; open subscription streams; and **auto-start** the server (spawn a detached
//! `orcr server start --foreground` and wait for readiness) for any verb that needs it.
//! The start race is resolved by the server's instance lock, so many clients racing to
//! auto-start still yield exactly one server (§11.6).

use crate::config::Config;
use crate::error::{ErrorCode, OrcrError, Result};
use crate::home::Home;
use crate::wire::{read_frame, write_frame, ORCR_PROTOCOL};
use serde_json::{json, Value};
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The outcome of an idempotent start / auto-start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartOutcome {
    /// This call started the server.
    Started,
    /// A healthy server was already running.
    AlreadyRunning,
}

impl StartOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            StartOutcome::Started => "started",
            StartOutcome::AlreadyRunning => "already_running",
        }
    }
}

/// A client bound to a server socket path.
#[derive(Debug, Clone)]
pub struct Client {
    socket_path: PathBuf,
}

impl Client {
    pub fn new(socket_path: impl Into<PathBuf>) -> Client {
        Client {
            socket_path: socket_path.into(),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Connect to the socket, rejecting a symlinked path (§11.6). Absence / refusal →
    /// `server_unreachable`.
    fn connect(&self) -> Result<UnixStream> {
        // lstat-validate: never follow a symlink at the socket path.
        if let Ok(md) = std::fs::symlink_metadata(&self.socket_path) {
            if md.file_type().is_symlink() {
                return Err(OrcrError::environment(
                    "unsafe_home",
                    format!(
                        "socket path {} is a symlink; refusing",
                        self.socket_path.display()
                    ),
                ));
            }
        }
        UnixStream::connect(&self.socket_path).map_err(|e| {
            OrcrError::environment(
                "server_unreachable",
                format!(
                    "cannot connect to orcr socket {}: {e}",
                    self.socket_path.display()
                ),
            )
            .with_details(json!({ "cause": "server_unreachable" }))
        })
    }

    /// Send one request and decode the response. Opens a fresh connection per call.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let mut stream = self.connect()?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(30))))
            .ok();
        let req = json!({
            "protocol": ORCR_PROTOCOL,
            "id": uuid::Uuid::now_v7().to_string(),
            "method": method,
            "params": params,
        });
        write_frame(&mut stream, &req)?;
        let mut reader = BufReader::new(stream);
        let frame = read_frame(&mut reader)?.ok_or_else(|| {
            OrcrError::environment(
                "server_unreachable",
                "server closed the connection with no response",
            )
        })?;
        let resp: Value = serde_json::from_slice(&frame).map_err(|e| {
            OrcrError::server_error("decode", format!("bad response from server: {e}"))
        })?;
        decode_response(&resp)
    }

    /// The readiness handshake (§11.6): returns `{pid, protocol, store, ready}`. Rejects a
    /// server that speaks a different protocol.
    pub fn handshake(&self) -> Result<Value> {
        let r = self.request("server.handshake", json!({}))?;
        let proto = r.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
        if proto != ORCR_PROTOCOL {
            return Err(OrcrError::environment(
                "unsupported_version",
                format!("server speaks protocol {proto}, this client speaks {ORCR_PROTOCOL}"),
            )
            .with_details(json!({ "cause": "unsupported_version" })));
        }
        Ok(r)
    }

    /// Poll the handshake until it succeeds or the deadline elapses.
    pub fn wait_for_ready(&self, timeout: Duration) -> Result<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let last = match self.handshake() {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            if Instant::now() >= deadline {
                return Err(OrcrError::environment(
                    "server_start_failed",
                    "server did not become ready in time",
                )
                .with_details(
                    json!({ "cause": "server_start_failed", "last_error": last.message }),
                ));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Ensure a healthy server is running, auto-starting one if needed. Idempotent.
    pub fn ensure_running(&self, home: &Home, _config: &Config) -> Result<StartOutcome> {
        if self.handshake().is_ok() {
            return Ok(StartOutcome::AlreadyRunning);
        }
        spawn_detached_server(home)?;
        self.wait_for_ready(Duration::from_secs(15))?;
        Ok(StartOutcome::Started)
    }

    /// Open a subscription stream (`events.subscribe` / `watch.open`). Returns the initial
    /// response plus a reader that yields subsequent event frames.
    pub fn open_stream(&self, method: &str, params: Value) -> Result<(Value, Subscription)> {
        let mut stream = self.connect()?;
        let req = json!({
            "protocol": ORCR_PROTOCOL,
            "id": uuid::Uuid::now_v7().to_string(),
            "method": method,
            "params": params,
        });
        write_frame(&mut stream, &req)?;
        let mut reader = BufReader::new(stream);
        let frame = read_frame(&mut reader)?.ok_or_else(|| {
            OrcrError::environment(
                "server_unreachable",
                "server closed before the subscribe response",
            )
        })?;
        let resp: Value = serde_json::from_slice(&frame).map_err(|e| {
            OrcrError::server_error("decode", format!("bad subscribe response: {e}"))
        })?;
        let initial = decode_response(&resp)?;
        Ok((initial, Subscription { reader }))
    }
}

/// A live subscription stream: yields `{subscription, seq, event}` frames.
pub struct Subscription {
    reader: BufReader<UnixStream>,
}

impl Subscription {
    /// Read the next event frame, or `None` at end of stream. Set a read timeout on the
    /// underlying stream first if you want a bounded wait.
    pub fn next_event(&mut self) -> Result<Option<Value>> {
        match read_frame(&mut self.reader)? {
            Some(bytes) => {
                let v: Value = serde_json::from_slice(&bytes).map_err(|e| {
                    OrcrError::server_error("decode", format!("bad event frame: {e}"))
                })?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    /// Apply a read timeout to the underlying stream (so `next_event` won't block forever).
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<()> {
        self.reader
            .get_ref()
            .set_read_timeout(timeout)
            .map_err(|e| OrcrError::server_error("socket_io", e.to_string()))
    }
}

/// Decode a `{ok,result|error}` response envelope into a [`Result`].
fn decode_response(resp: &Value) -> Result<Value> {
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
    }
    let err = resp.get("error").cloned().unwrap_or(Value::Null);
    Err(wire_error_to_orcr(&err))
}

/// Reconstruct an [`OrcrError`] from a wire error object.
fn wire_error_to_orcr(err: &Value) -> OrcrError {
    let code = err
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or("server_error");
    let message = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("server error")
        .to_string();
    let details = err.get("details").cloned().unwrap_or(Value::Null);
    let ec = match code {
        "not_found" => ErrorCode::NotFound,
        "invalid_request" => ErrorCode::InvalidRequest,
        "state_conflict" => ErrorCode::StateConflict,
        "blocked" => ErrorCode::Blocked,
        "timeout" => ErrorCode::Timeout,
        "integration_missing" => ErrorCode::IntegrationMissing,
        "transcript_unavailable" => ErrorCode::TranscriptUnavailable,
        "environment_error" => ErrorCode::EnvironmentError,
        _ => ErrorCode::ServerError,
    };
    OrcrError::new(ec, message).with_details(details)
}

/// Spawn a detached `orcr server start --foreground`, inheriting the current env plus an
/// explicit `ORCR_HOME` so the child resolves the same home. Detached into its own session
/// so it outlives the caller.
fn spawn_detached_server(home: &Home) -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().map_err(|e| {
        OrcrError::environment(
            "server_start_failed",
            format!("cannot find own executable: {e}"),
        )
    })?;
    let mut cmd = Command::new(exe);
    cmd.args(["server", "start", "--foreground"])
        .env("ORCR_HOME", home.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach into a new session so a parent shell exiting doesn't take the server down.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        OrcrError::environment(
            "server_start_failed",
            format!("failed to spawn server: {e}"),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_ok_and_error() {
        let ok = json!({"id":"x","ok":true,"result":{"a":1}});
        assert_eq!(decode_response(&ok).unwrap()["a"], 1);

        let err = json!({"id":"x","ok":false,"error":{"code":"not_found","message":"nope"}});
        let e = decode_response(&err).unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        assert_eq!(e.message, "nope");
    }

    #[test]
    fn unreachable_when_no_socket() {
        let tmp = tempfile::tempdir().unwrap();
        let c = Client::new(tmp.path().join("orcr.sock"));
        let e = c.request("server.handshake", json!({})).unwrap_err();
        assert_eq!(e.details["cause"], "server_unreachable");
    }
}
