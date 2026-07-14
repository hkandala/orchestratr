//! The herdr socket driver (spec §4, §11.7): orcr speaks herdr's own JSON socket
//! protocol directly. Every operation is pinned to a named herdr method with fixed
//! request/result shapes (see [`contract`]).
//!
//! herdr uses **one request per connection** (it closes the socket after the response),
//! so each call opens a fresh blocking connection, writes one framed request, reads the
//! one response, and closes. Connection failures are retried with backoff and surface as
//! `environment_error {cause: herdr_unreachable}`.

pub mod contract;
mod integration;
pub mod protocol;
mod session;
pub mod transcript;

pub use integration::{
    ensure_supported, launch_plan, mock_provider_enabled, tuning_for, IntegrationState,
    LaunchPlan, ProviderIntegration, TuningParams, MOCK_PROVIDER, ORCR_BUILTIN_PROVIDERS,
};
pub use protocol::*;
pub use transcript::{locate_transcript, TranscriptEntry, TranscriptLocator};
pub use session::{HerdrBinary, HerdrSession};

use crate::error::{ErrorCode, OrcrError, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default per-request IO timeout.
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(15);
/// Connection retry attempts for transient herdr-unreachable conditions.
const RETRY_ATTEMPTS: u32 = 4;
/// Base backoff between retries (doubles each attempt).
const RETRY_BASE: Duration = Duration::from_millis(100);
/// Max frame size we will read from a single response (guard against runaways).
const MAX_FRAME: usize = 32 * 1024 * 1024;

/// A driver bound to a single herdr session socket. All operations over it are scoped to
/// that socket's session.
#[derive(Debug, Clone)]
pub struct HerdrDriver {
    socket_path: PathBuf,
    io_timeout: Duration,
    /// The protocol version negotiated at connect.
    protocol: u32,
}

impl HerdrDriver {
    /// Connect to a herdr session socket and perform the version handshake. Rejects a
    /// herdr whose reported protocol is below [`MIN_HERDR_PROTOCOL`] with
    /// `environment_error {cause: unsupported_version}`.
    pub fn connect(socket_path: impl Into<PathBuf>) -> Result<HerdrDriver> {
        let mut d = HerdrDriver {
            socket_path: socket_path.into(),
            io_timeout: DEFAULT_IO_TIMEOUT,
            protocol: 0,
        };
        let pong = d.ping()?;
        if pong.protocol < MIN_HERDR_PROTOCOL {
            return Err(OrcrError::environment(
                "unsupported_version",
                format!(
                    "herdr reports socket protocol {} but orcr requires at least {} \
                     (herdr {}); upgrade herdr",
                    pong.protocol, MIN_HERDR_PROTOCOL, pong.version
                ),
            )
            .with_details(json!({
                "cause": "unsupported_version",
                "reported_protocol": pong.protocol,
                "required_protocol": MIN_HERDR_PROTOCOL,
                "herdr_version": pong.version,
            })));
        }
        d.protocol = pong.protocol;
        Ok(d)
    }

    /// The negotiated herdr protocol version.
    pub fn protocol(&self) -> u32 {
        self.protocol
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    // --- Handshake / health ---

    /// `ping` → pong (version + protocol). This is the handshake probe.
    pub fn ping(&self) -> Result<Pong> {
        let r = self.call("ping", json!({}))?;
        expect_type(&r, "pong")?;
        Ok(Pong {
            version: r
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            protocol: r.get("protocol").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        })
    }

    // --- Session tree reads ---

    /// `session.snapshot` — the whole session tree, scoped to this socket's session.
    pub fn session_snapshot(&self) -> Result<SessionSnapshot> {
        let r = self.call("session.snapshot", json!({}))?;
        expect_type(&r, "session_snapshot")?;
        from_field(&r, "snapshot")
    }

    /// `agent.list` — agent rows for this session.
    pub fn agent_list(&self) -> Result<Vec<AgentInfo>> {
        let r = self.call("agent.list", json!({}))?;
        expect_type(&r, "agent_list")?;
        from_field(&r, "agents")
    }

    /// `pane.list` — pane rows (optionally scoped to a workspace). Includes non-agent panes.
    pub fn pane_list(&self, workspace_id: Option<&str>) -> Result<Vec<PaneInfo>> {
        let r = self.call("pane.list", json!({ "workspace_id": workspace_id }))?;
        expect_type(&r, "pane_list")?;
        from_field(&r, "panes")
    }

    /// `pane.get` — a single pane by id.
    pub fn pane_get(&self, pane_id: &str) -> Result<PaneInfo> {
        let r = self.call("pane.get", json!({ "pane_id": pane_id }))?;
        expect_type(&r, "pane_info")?;
        from_field(&r, "pane")
    }

    /// `workspace.list` — workspaces in this session.
    pub fn workspace_list(&self) -> Result<Vec<WorkspaceInfo>> {
        let r = self.call("workspace.list", json!({}))?;
        expect_type(&r, "workspace_list")?;
        from_field(&r, "workspaces")
    }

    // --- Mutations ---

    /// `workspace.create` — create a workspace; returns the created workspace + its
    /// initial tab + root pane.
    pub fn workspace_create(
        &self,
        label: Option<&str>,
        cwd: Option<&str>,
        env: &std::collections::BTreeMap<String, String>,
    ) -> Result<WorkspaceCreated> {
        let r = self.call(
            "workspace.create",
            json!({ "label": label, "cwd": cwd, "env": env, "focus": false }),
        )?;
        expect_type(&r, "workspace_created")?;
        Ok(WorkspaceCreated {
            workspace: from_field(&r, "workspace")?,
            tab: from_field(&r, "tab")?,
            root_pane: from_field(&r, "root_pane")?,
        })
    }

    /// `agent.start` — herdr creates the tab + pane and runs the provider argv; the
    /// returned ids are authoritative (spec §11.1, §11.7).
    pub fn agent_start(&self, params: &AgentStartParams) -> Result<AgentInfo> {
        let r = self.call("agent.start", serde_json::to_value(params).unwrap())?;
        expect_type(&r, "agent_started")?;
        from_field(&r, "agent")
    }

    /// `pane.send_text` — type text into a pane (first half of the two-call rule, §5.6).
    pub fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        let r = self.call(
            "pane.send_text",
            json!({ "pane_id": pane_id, "text": text }),
        )?;
        expect_ack(&r)
    }

    /// `pane.send_keys` — send key names into a pane (second half of the two-call rule).
    pub fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()> {
        let r = self.call(
            "pane.send_keys",
            json!({ "pane_id": pane_id, "keys": keys }),
        )?;
        expect_ack(&r)
    }

    /// `pane.move` — move a pane to a destination (park/un-park; across workspaces).
    pub fn pane_move(
        &self,
        pane_id: &str,
        destination: PaneMoveDestination,
    ) -> Result<PaneMoveResult> {
        let r = self.call(
            "pane.move",
            json!({
                "pane_id": pane_id,
                "focus": false,
                "destination": destination,
            }),
        )?;
        expect_type(&r, "pane_move")?;
        from_field(&r, "move_result")
    }

    /// `pane.close` — close a pane (herdr clears empty tabs/workspaces automatically).
    pub fn pane_close(&self, pane_id: &str) -> Result<()> {
        let r = self.call("pane.close", json!({ "pane_id": pane_id }))?;
        expect_ack(&r)
    }

    /// `notification.show` — surface a notification (blocked alerts, future).
    pub fn notification_show(&self, title: &str, body: Option<&str>) -> Result<()> {
        let r = self.call("notification.show", json!({ "title": title, "body": body }))?;
        // returns `notification_show {reason, shown}` — accept it.
        expect_type(&r, "notification_show")
    }

    /// `pane.report_agent` — report an agent state + transcript pointer for a pane. Used
    /// by the mock provider to report state through herdr's integration mechanism (§5.6).
    pub fn pane_report_agent(
        &self,
        pane_id: &str,
        source: &str,
        agent: &str,
        state: PaneAgentState,
        agent_session_id: Option<&str>,
    ) -> Result<()> {
        let r = self.call(
            "pane.report_agent",
            json!({
                "pane_id": pane_id,
                "source": source,
                "agent": agent,
                "state": state.as_str(),
                "agent_session_id": agent_session_id,
            }),
        )?;
        expect_ack(&r)
    }

    // --- Transport ---

    /// Perform one request against the session socket, with connection-retry/backoff.
    /// Returns the `result` object (a tagged union on `type`).
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let mut attempt = 0;
        loop {
            match self.call_once(method, &params) {
                Ok(v) => return Ok(v),
                Err(e) => {
                    // Only retry transient unreachable conditions.
                    let transient = e.code == ErrorCode::EnvironmentError
                        && e.details.get("cause") == Some(&json!("herdr_unreachable"));
                    if transient && attempt < RETRY_ATTEMPTS {
                        std::thread::sleep(RETRY_BASE * 2u32.pow(attempt));
                        attempt += 1;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    fn call_once(&self, method: &str, params: &Value) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| self.unreachable(e))?;
        stream
            .set_read_timeout(Some(self.io_timeout))
            .and_then(|_| stream.set_write_timeout(Some(self.io_timeout)))
            .map_err(|e| self.unreachable(e))?;

        let id = format!("orcr:{}", uuid::Uuid::now_v7());
        let req = json!({
            "protocol": MIN_HERDR_PROTOCOL,
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_vec(&req).map_err(|e| {
            OrcrError::server_error("encode", format!("failed to encode herdr request: {e}"))
        })?;
        line.push(b'\n');
        stream.write_all(&line).map_err(|e| self.unreachable(e))?;
        stream.flush().map_err(|e| self.unreachable(e))?;

        // herdr closes the connection after the single response — read to EOF.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let n = stream.read(&mut chunk).map_err(|e| self.unreachable(e))?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > MAX_FRAME {
                return Err(OrcrError::server_error(
                    "frame_too_large",
                    "herdr response exceeded the maximum frame size",
                ));
            }
            // A full line means the response is complete even if the peer hasn't closed.
            if buf.contains(&b'\n') {
                break;
            }
        }
        if buf.is_empty() {
            return Err(self.unreachable(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "herdr closed the connection without a response",
            )));
        }

        let text = String::from_utf8_lossy(&buf);
        let first = text.lines().next().unwrap_or("");
        let env: Value = serde_json::from_str(first).map_err(|e| {
            OrcrError::server_error(
                "decode",
                format!("failed to decode herdr response for {method}: {e}"),
            )
        })?;

        if let Some(err) = env.get("error") {
            let code = err
                .get("code")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let message = err
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("herdr returned an error");
            return Err(OrcrError::new(
                ErrorCode::EnvironmentError,
                format!("herdr error on {method}: {message}"),
            )
            .with_details(json!({ "cause": "herdr_error", "herdr_code": code })));
        }

        env.get("result").cloned().ok_or_else(|| {
            OrcrError::server_error(
                "decode",
                format!("herdr response for {method} had no result field"),
            )
        })
    }

    fn unreachable(&self, e: std::io::Error) -> OrcrError {
        OrcrError::environment(
            "herdr_unreachable",
            format!(
                "cannot reach herdr socket {}: {e}",
                self.socket_path.display()
            ),
        )
    }
}

/// The result of `workspace.create`.
#[derive(Debug, Clone)]
pub struct WorkspaceCreated {
    pub workspace: WorkspaceInfo,
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
}

/// Assert the tagged-union `type` of a herdr result object.
fn expect_type(result: &Value, want: &str) -> Result<()> {
    let got = result.get("type").and_then(|v| v.as_str());
    if got == Some(want) {
        Ok(())
    } else {
        Err(OrcrError::server_error(
            "unexpected_result",
            format!(
                "herdr returned result type {:?}, expected {want:?}",
                got.unwrap_or("<none>")
            ),
        ))
    }
}

/// Accept an `ok` acknowledgement (some mutating methods return the bare `ok` variant).
fn expect_ack(result: &Value) -> Result<()> {
    expect_type(result, "ok")
}

/// Deserialize a named field of a result object into a typed value.
fn from_field<T: serde::de::DeserializeOwned>(result: &Value, field: &str) -> Result<T> {
    let v = result.get(field).ok_or_else(|| {
        OrcrError::server_error("decode", format!("herdr result missing field `{field}`"))
    })?;
    serde_json::from_value(v.clone()).map_err(|e| {
        OrcrError::server_error("decode", format!("failed to decode herdr `{field}`: {e}"))
    })
}
