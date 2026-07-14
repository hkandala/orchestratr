//! herdr binary discovery, session enumeration, and owned-session bootstrap (spec §5.2,
//! §11.7). These are the operations a socket alone cannot do: the herdr *binary* is
//! discovered and used to enumerate sessions and to start a session's headless server.
//!
//! Critical fact (verified against herdr 0.7.2): **sessions are per-socket**. Each
//! session carries its own `session_dir` + `socket_path`; there is no single socket that
//! sees every session. The owned-session driver connects to the owned session's own
//! socket, and cross-session enumeration fans out over each session's socket.

use crate::error::{OrcrError, Result};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One row from `herdr session list --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct HerdrSession {
    #[serde(default)]
    pub default: bool,
    pub name: String,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub session_dir: Option<String>,
    #[serde(default)]
    pub socket_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SessionList {
    sessions: Vec<HerdrSession>,
}

/// A discovered herdr binary. Discovery order (spec §2): config `herdr.bin` →
/// `$ORCR_HERDR_BIN` → `$PATH`.
#[derive(Debug, Clone)]
pub struct HerdrBinary {
    path: PathBuf,
}

impl HerdrBinary {
    /// Discover the herdr binary. `config_bin` is `herdr.bin` from config (empty = unset).
    /// A missing binary yields `environment_error {cause: herdr_missing}` with an install
    /// pointer (exit 2).
    pub fn discover(config_bin: Option<&str>) -> Result<HerdrBinary> {
        // 1) explicit config path
        if let Some(bin) = config_bin.filter(|s| !s.is_empty()) {
            return HerdrBinary::from_explicit(PathBuf::from(bin), "herdr.bin");
        }
        // 2) $ORCR_HERDR_BIN
        if let Some(bin) = std::env::var_os("ORCR_HERDR_BIN").filter(|s| !s.is_empty()) {
            return HerdrBinary::from_explicit(PathBuf::from(bin), "$ORCR_HERDR_BIN");
        }
        // 3) $PATH
        if let Some(p) = find_in_path("herdr") {
            return Ok(HerdrBinary { path: p });
        }
        Err(herdr_missing(
            "herdr was not found on $PATH (and neither herdr.bin nor $ORCR_HERDR_BIN is set)",
        ))
    }

    fn from_explicit(path: PathBuf, source: &str) -> Result<HerdrBinary> {
        if is_executable(&path) {
            Ok(HerdrBinary { path })
        } else {
            Err(herdr_missing(format!(
                "herdr binary from {source} ({}) does not exist or is not executable",
                path.display()
            )))
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Enumerate all herdr sessions (`herdr session list --json`).
    pub fn session_list(&self) -> Result<Vec<HerdrSession>> {
        let out = Command::new(&self.path)
            .args(["session", "list", "--json"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| herdr_unreachable(format!("failed to run `herdr session list`: {e}")))?;
        if !out.status.success() {
            return Err(herdr_unreachable(format!(
                "`herdr session list` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let parsed: SessionList = serde_json::from_slice(&out.stdout).map_err(|e| {
            OrcrError::server_error(
                "decode",
                format!("failed to parse `herdr session list --json`: {e}"),
            )
        })?;
        Ok(parsed.sessions)
    }

    /// Look up one session by name.
    pub fn find_session(&self, name: &str) -> Result<Option<HerdrSession>> {
        Ok(self.session_list()?.into_iter().find(|s| s.name == name))
    }

    /// Ensure the owned session's herdr server is running headless and return its socket
    /// path (spec §5.2). Idempotent: if the session already runs, its socket is returned;
    /// otherwise a detached `herdr --session <name> server` is spawned and we poll until
    /// the session appears with a socket path.
    pub fn ensure_session(&self, name: &str) -> Result<PathBuf> {
        if let Some(sock) = self.running_socket(name)? {
            return Ok(sock);
        }
        // Spawn the headless server detached; it runs in the foreground of its own
        // process, so we detach stdio and do not wait on it.
        Command::new(&self.path)
            .args(["--session", name, "server"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                OrcrError::environment(
                    "server_start_failed",
                    format!("failed to spawn `herdr --session {name} server`: {e}"),
                )
            })?;

        // Poll for readiness.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(sock) = self.running_socket(name)? {
                return Ok(sock);
            }
            if Instant::now() >= deadline {
                return Err(OrcrError::environment(
                    "server_start_failed",
                    format!("herdr session `{name}` did not become ready within 10s"),
                ));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    /// Return the socket path of a running session, if it is running and has one.
    fn running_socket(&self, name: &str) -> Result<Option<PathBuf>> {
        match self.find_session(name)? {
            Some(s) if s.running => Ok(s.socket_path.map(PathBuf::from)),
            _ => Ok(None),
        }
    }

    /// Stop a session's server (e2e teardown of disposable sessions).
    pub fn session_stop(&self, name: &str) -> Result<()> {
        let _ = Command::new(&self.path)
            .args(["session", "stop", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    /// Delete a session (e2e teardown of disposable sessions).
    pub fn session_delete(&self, name: &str) -> Result<()> {
        let _ = Command::new(&self.path)
            .args(["session", "delete", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok(())
    }

    /// Raw integration-status text (`herdr integration status`) for parsing per-provider
    /// state. See [`super::integration`].
    pub fn integration_status_raw(&self) -> Result<String> {
        let out = Command::new(&self.path)
            .args(["integration", "status"])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                herdr_unreachable(format!("failed to run `herdr integration status`: {e}"))
            })?;
        if !out.status.success() {
            return Err(herdr_unreachable(format!(
                "`herdr integration status` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && (md.permissions().mode() & 0o111 != 0),
        Err(_) => false,
    }
}

/// Resolve a bare command name against `$PATH`.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let cand = dir.join(name);
        if is_executable(&cand) {
            return Some(cand);
        }
    }
    None
}

fn herdr_missing(msg: impl Into<String>) -> OrcrError {
    OrcrError::environment(
        "herdr_missing",
        format!(
            "{}. Install herdr from https://herdr.dev, or set herdr.bin / $ORCR_HERDR_BIN.",
            msg.into()
        ),
    )
}

fn herdr_unreachable(msg: impl Into<String>) -> OrcrError {
    OrcrError::environment("herdr_unreachable", msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_list_json() {
        let raw = r#"{"sessions":[{"default":true,"name":"default","running":true,
            "session_dir":"/d","socket_path":"/d/herdr.sock"}]}"#;
        let list: SessionList = serde_json::from_str(raw).unwrap();
        assert_eq!(list.sessions.len(), 1);
        let s = &list.sessions[0];
        assert_eq!(s.name, "default");
        assert!(s.default && s.running);
        assert_eq!(s.socket_path.as_deref(), Some("/d/herdr.sock"));
    }

    #[test]
    fn discover_missing_binary_is_friendly() {
        // Point config at a non-existent path.
        let e = HerdrBinary::from_explicit(PathBuf::from("/nonexistent/herdr"), "herdr.bin")
            .unwrap_err();
        assert_eq!(e.details["cause"], "herdr_missing");
        assert!(e.message.contains("herdr.dev"));
        assert_eq!(e.exit_code(), 2);
    }
}
