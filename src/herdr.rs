use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_SEND_INPUT_DELAY: Duration = Duration::from_secs(1);
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(750);
pub const INSTALL_URL: &str = "https://herdr.dev";

#[derive(Debug, Error)]
pub enum HerdrError {
    #[error("herdr was not found; install it from {INSTALL_URL}")]
    NotFound,
    #[error("failed to run herdr: {0}")]
    Io(#[from] std::io::Error),
    #[error("herdr command failed: {code}: {message}")]
    Command { code: String, message: String },
    #[error("herdr command failed: {0}")]
    CommandFailed(String),
    #[error("failed to parse herdr json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, HerdrError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrClient {
    bin: PathBuf,
    session: String,
    send_input_delay: Duration,
    poll_interval: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentStartInfo {
    pub agent_status: String,
    pub cwd: String,
    pub focused: bool,
    pub foreground_cwd: Option<String>,
    pub name: String,
    pub pane_id: String,
    pub revision: i64,
    pub tab_id: Option<String>,
    pub terminal_id: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_session: Option<AgentSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneInfo {
    pub agent_status: String,
    pub cwd: Option<String>,
    pub focused: Option<bool>,
    pub foreground_cwd: Option<String>,
    #[serde(alias = "name")]
    pub label: Option<String>,
    pub pane_id: String,
    pub revision: Option<i64>,
    pub tab_id: Option<String>,
    pub terminal_id: Option<String>,
    pub workspace_id: Option<String>,
    pub agent_session: Option<AgentSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentSession {
    pub source: Option<String>,
    pub agent: Option<String>,
    pub kind: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WaitOutputInfo {
    pub matched_line: String,
    pub pane_id: String,
    pub read: WaitReadInfo,
    pub revision: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WaitReadInfo {
    pub format: Option<String>,
    pub pane_id: String,
    pub revision: Option<i64>,
    pub source: Option<String>,
    pub text: String,
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionList {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionInfo {
    #[serde(default)]
    pub default: bool,
    pub name: String,
    #[serde(default)]
    pub running: bool,
    pub session_dir: Option<String>,
    pub socket_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionStopInfo {
    #[serde(default)]
    pub stopped: bool,
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionDeleteInfo {
    #[serde(default)]
    pub deleted: bool,
    pub session: Option<SessionInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct StatusInfo {
    pub client: Option<ClientStatus>,
    pub server: Option<ServerStatus>,
    pub update: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseCapture {
    pub text: String,
    pub source: ResponseSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseSource {
    File,
    Scrape,
}

impl ResponseSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Scrape => "scrape",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClientStatus {
    pub version: Option<String>,
    pub channel: Option<String>,
    pub protocol: Option<i64>,
    pub binary: Option<String>,
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerStatus {
    pub status: Option<String>,
    pub running: Option<bool>,
    pub version: Option<String>,
    pub protocol: Option<i64>,
    pub compatible: Option<bool>,
    pub socket: Option<String>,
    pub session: Option<String>,
    pub restart_needed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionServerStatus {
    pub status: String,
    pub running: bool,
    pub version: Option<String>,
    pub protocol: Option<i64>,
    pub compatible: Option<bool>,
    pub socket: Option<String>,
    pub session: Option<String>,
    pub restart_needed: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    Done,
    Blocked,
    Timeout,
    PaneGone,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTracker {
    seen_working: bool,
    idle_since_ms: Option<u64>,
    grace_ms: Option<u64>,
}

impl CompletionTracker {
    pub fn status_transition() -> Self {
        Self {
            seen_working: false,
            idle_since_ms: None,
            grace_ms: None,
        }
    }

    pub fn with_grace(grace_ms: u64) -> Self {
        Self {
            seen_working: false,
            idle_since_ms: None,
            grace_ms: Some(grace_ms),
        }
    }

    pub fn observe(&mut self, status: AgentStatus, elapsed_ms: u64) -> Option<CompletionOutcome> {
        match status {
            AgentStatus::Working => {
                self.seen_working = true;
                self.idle_since_ms = None;
                None
            }
            AgentStatus::Idle => {
                if self.seen_working {
                    Some(CompletionOutcome::Done)
                } else if let Some(grace_ms) = self.grace_ms {
                    let idle_since = *self.idle_since_ms.get_or_insert(elapsed_ms);
                    (elapsed_ms.saturating_sub(idle_since) >= grace_ms)
                        .then_some(CompletionOutcome::Done)
                } else {
                    None
                }
            }
            AgentStatus::Blocked => Some(CompletionOutcome::Blocked),
            AgentStatus::Done => Some(CompletionOutcome::Done),
            AgentStatus::Unknown | AgentStatus::Other => None,
        }
    }
}

impl From<&str> for AgentStatus {
    fn from(value: &str) -> Self {
        match value {
            "idle" => Self::Idle,
            "working" => Self::Working,
            "blocked" => Self::Blocked,
            "done" => Self::Done,
            "unknown" => Self::Unknown,
            _ => Self::Other,
        }
    }
}

impl HerdrClient {
    pub fn new(bin: PathBuf, session: impl Into<String>) -> Self {
        Self {
            bin,
            session: session.into(),
            send_input_delay: DEFAULT_SEND_INPUT_DELAY,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    pub fn with_timings(mut self, send_input_delay: Duration, poll_interval: Duration) -> Self {
        self.send_input_delay = send_input_delay;
        self.poll_interval = poll_interval;
        self
    }

    pub fn bin(&self) -> &Path {
        &self.bin
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    pub fn ensure_session(&self, deadline: Duration) -> Result<()> {
        Command::new(&self.bin)
            .args(["--session", &self.session, "server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let start = Instant::now();
        while start.elapsed() < deadline {
            if self
                .status_server()
                .map(|status| status.running)
                .unwrap_or(false)
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(250));
        }
        Err(HerdrError::CommandFailed(format!(
            "timed out waiting for herdr session '{}' server to start",
            self.session
        )))
    }

    pub fn status_server(&self) -> Result<SessionServerStatus> {
        self.run_session_json(["status", "server", "--json"])
    }

    pub fn version(&self) -> Result<String> {
        let output = Command::new(&self.bin).arg("--version").output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
        command_failed(&output.stderr)
    }

    pub fn agent_start(
        &self,
        label: &str,
        cwd: &Path,
        envs: &[(String, String)],
        argv: &[String],
    ) -> Result<AgentStartInfo> {
        let mut args = vec![
            "agent".to_string(),
            "start".to_string(),
            label.to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
        ];
        for (key, value) in envs {
            args.push("--env".to_string());
            args.push(format!("{key}={value}"));
        }
        args.push("--no-focus".to_string());
        args.push("--".to_string());
        args.extend(argv.iter().cloned());

        let result: AgentStartResult = self.run_session_json(args)?;
        Ok(result.agent)
    }

    pub fn pane_get(&self, pane_id: &str) -> Result<PaneInfo> {
        let result: PaneGetResult = self.run_session_json(["pane", "get", pane_id])?;
        Ok(result.pane)
    }

    pub fn pane_send_text(&self, pane_id: &str, text: &str) -> Result<()> {
        self.run_session_plain(["pane", "send-text", pane_id, text])
    }

    pub fn pane_send_keys(&self, pane_id: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec!["pane", "send-keys", pane_id];
        args.extend(keys.iter().copied());
        self.run_session_plain(args)
    }

    pub fn send_input(&self, pane_id: &str, text: &str) -> Result<()> {
        self.pane_send_text(pane_id, text)?;
        thread::sleep(self.send_input_delay);
        self.pane_send_keys(pane_id, &["enter"])
    }

    pub fn pane_read(
        &self,
        pane_id: &str,
        source: Option<&str>,
        lines: Option<u32>,
        format: Option<&str>,
    ) -> Result<String> {
        let mut args = vec!["pane".to_string(), "read".to_string(), pane_id.to_string()];
        if let Some(source) = source {
            args.push("--source".to_string());
            args.push(source.to_string());
        }
        if let Some(lines) = lines {
            args.push("--lines".to_string());
            args.push(lines.to_string());
        }
        if let Some(format) = format {
            args.push("--format".to_string());
            args.push(format.to_string());
        }
        self.run_session_text(args)
    }

    pub fn pane_close(&self, pane_id: &str) -> Result<()> {
        let _: Value = self.run_session_json(["pane", "close", pane_id])?;
        Ok(())
    }

    pub fn capture_response_with_scrape(
        &self,
        pane_id: &str,
        response_path: &Path,
    ) -> Result<ResponseCapture> {
        if response_path.exists() {
            return Ok(ResponseCapture {
                text: fs::read_to_string(response_path)?,
                source: ResponseSource::File,
            });
        }

        let text = self.pane_read(pane_id, Some("recent-unwrapped"), Some(1000), Some("text"))?;
        if let Some(parent) = response_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(response_path, &text)?;
        Ok(ResponseCapture {
            text,
            source: ResponseSource::Scrape,
        })
    }

    pub fn wait_output(
        &self,
        pane_id: &str,
        match_text: &str,
        regex: bool,
        timeout_ms: u64,
    ) -> Result<WaitOutputInfo> {
        let mut args = vec![
            "wait".to_string(),
            "output".to_string(),
            pane_id.to_string(),
            "--match".to_string(),
            match_text.to_string(),
            "--source".to_string(),
            "recent-unwrapped".to_string(),
            "--lines".to_string(),
            "1000".to_string(),
            "--timeout".to_string(),
            timeout_ms.to_string(),
        ];
        if regex {
            args.push("--regex".to_string());
        }
        let result: WaitOutputResult = self.run_session_json(args)?;
        Ok(WaitOutputInfo {
            matched_line: result.matched_line,
            pane_id: result.pane_id,
            read: result.read,
            revision: result.revision,
        })
    }

    pub fn wait_agent_status(
        &self,
        pane_id: &str,
        status: &str,
        timeout_ms: u64,
    ) -> Result<PaneInfo> {
        let result: PaneGetResult = self.run_session_json([
            "wait",
            "agent-status",
            pane_id,
            "--status",
            status,
            "--timeout",
            &timeout_ms.to_string(),
        ])?;
        Ok(result.pane)
    }

    pub fn watch_status_completion(
        &self,
        pane_id: &str,
        timeout: Duration,
        grace_ms: Option<u64>,
    ) -> Result<CompletionOutcome> {
        let start = Instant::now();
        let mut tracker = match grace_ms {
            Some(ms) => CompletionTracker::with_grace(ms),
            None => CompletionTracker::status_transition(),
        };
        loop {
            if start.elapsed() >= timeout {
                return Ok(CompletionOutcome::Timeout);
            }
            match self.pane_get(pane_id) {
                Ok(pane) => {
                    let elapsed_ms = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
                    if let Some(outcome) =
                        tracker.observe(AgentStatus::from(pane.agent_status.as_str()), elapsed_ms)
                    {
                        return Ok(outcome);
                    }
                }
                Err(HerdrError::Command { code, .. }) if code == "pane_not_found" => {
                    return Ok(CompletionOutcome::PaneGone);
                }
                Err(error) => return Err(error),
            }
            thread::sleep(self.poll_interval);
        }
    }

    pub fn watch_output_markers(
        &self,
        pane_id: &str,
        done_marker: &str,
        blocked_marker: &str,
        timeout: Duration,
    ) -> Result<CompletionOutcome> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match self.pane_read(pane_id, Some("recent-unwrapped"), Some(1000), Some("text")) {
                Ok(text) if text.contains(blocked_marker) => return Ok(CompletionOutcome::Blocked),
                Ok(text) if text.contains(done_marker) => return Ok(CompletionOutcome::Done),
                Ok(_) => thread::sleep(self.poll_interval),
                Err(HerdrError::Command { code, .. }) if code == "pane_not_found" => {
                    return Ok(CompletionOutcome::PaneGone);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(CompletionOutcome::Timeout)
    }

    pub fn session_list(&self) -> Result<SessionList> {
        self.run_top_json(["session", "list", "--json"])
    }

    pub fn session_stop(&self, name: &str) -> Result<SessionStopInfo> {
        self.run_top_json(["session", "stop", name, "--json"])
    }

    pub fn session_delete(&self, name: &str) -> Result<SessionDeleteInfo> {
        self.run_top_json(["session", "delete", name, "--json"])
    }

    pub fn status(&self) -> Result<StatusInfo> {
        self.run_top_json(["status", "--json"])
    }

    fn run_session_json<T, I, S>(&self, args: I) -> Result<T>
    where
        T: DeserializeOwned,
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new(&self.bin)
            .arg("--session")
            .arg(&self.session)
            .args(args)
            .output()?;
        parse_json_command_output(&output.stdout, &output.stderr, output.status.success())
    }

    fn run_top_json<T, I, S>(&self, args: I) -> Result<T>
    where
        T: DeserializeOwned,
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new(&self.bin).args(args).output()?;
        parse_json_command_output(&output.stdout, &output.stderr, output.status.success())
    }

    fn run_session_plain<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new(&self.bin)
            .arg("--session")
            .arg(&self.session)
            .args(args)
            .output()?;
        if output.status.success() && output.stdout.is_empty() {
            return Ok(());
        }
        if !output.stdout.is_empty() {
            parse_json_command_output::<Value>(
                &output.stdout,
                &output.stderr,
                output.status.success(),
            )?;
            return Ok(());
        }
        command_failed(&output.stderr)
    }

    fn run_session_text<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = Command::new(&self.bin)
            .arg("--session")
            .arg(&self.session)
            .args(args)
            .output()?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        if !output.stdout.is_empty() {
            parse_json_command_output::<Value>(&output.stdout, &output.stderr, false)?;
        }
        command_failed(&output.stderr)
    }
}

#[derive(Debug, Deserialize)]
struct AgentStartResult {
    agent: AgentStartInfo,
}

#[derive(Debug, Deserialize)]
struct PaneGetResult {
    pane: PaneInfo,
}

#[derive(Debug, Deserialize)]
struct WaitOutputResult {
    matched_line: String,
    pane_id: String,
    read: WaitReadInfo,
    revision: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    result: Option<T>,
    error: Option<EnvelopeError>,
}

#[derive(Debug, Deserialize)]
struct EnvelopeError {
    code: String,
    message: String,
}

pub fn discover_herdr(config_bin: &str) -> Result<PathBuf> {
    discover_herdr_with(
        config_bin,
        |key| env::var_os(key),
        |name| find_in_path(name, env::var_os("PATH")),
    )
}

pub fn discover_herdr_with<FEnv, FPath>(
    config_bin: &str,
    env_lookup: FEnv,
    path_search: FPath,
) -> Result<PathBuf>
where
    FEnv: Fn(&str) -> Option<OsString>,
    FPath: Fn(&str) -> Option<PathBuf>,
{
    if !config_bin.trim().is_empty() {
        let path = PathBuf::from(config_bin);
        return path.exists().then_some(path).ok_or(HerdrError::NotFound);
    }
    if let Some(value) = env_lookup("ORCR_HERDR_BIN") {
        let path = PathBuf::from(value);
        return path.exists().then_some(path).ok_or(HerdrError::NotFound);
    }
    path_search("herdr").ok_or(HerdrError::NotFound)
}

pub fn find_in_path(binary: &str, path: Option<OsString>) -> Option<PathBuf> {
    let path = path?;
    env::split_paths(&path)
        .map(|dir| dir.join(binary))
        .find(|candidate| is_executable_file(candidate))
}

pub fn parse_json_envelope<T>(bytes: &[u8]) -> Result<T>
where
    T: DeserializeOwned,
{
    let envelope: Envelope<T> = serde_json::from_slice(bytes)?;
    if let Some(error) = envelope.error {
        return Err(HerdrError::Command {
            code: error.code,
            message: error.message,
        });
    }
    envelope.result.ok_or_else(|| {
        HerdrError::CommandFailed("herdr json envelope had no result or error".to_string())
    })
}

pub fn parse_json_value_or_envelope<T>(bytes: &[u8]) -> Result<T>
where
    T: DeserializeOwned,
{
    let value: Value = serde_json::from_slice(bytes)?;
    if let Some(error) = value.get("error") {
        let error: EnvelopeError = serde_json::from_value(error.clone())?;
        return Err(HerdrError::Command {
            code: error.code,
            message: error.message,
        });
    }
    if let Some(result) = value.get("result") {
        return serde_json::from_value(result.clone()).map_err(HerdrError::Json);
    }
    serde_json::from_value(value).map_err(HerdrError::Json)
}

fn parse_json_command_output<T>(stdout: &[u8], stderr: &[u8], success: bool) -> Result<T>
where
    T: DeserializeOwned,
{
    if !stdout.is_empty() {
        return parse_json_value_or_envelope(stdout);
    }
    if !stderr.is_empty() {
        if let Ok(value) = parse_json_value_or_envelope(stderr) {
            return Ok(value);
        }
    }
    if success {
        serde_json::from_str("null").map_err(HerdrError::Json)
    } else {
        command_failed(stderr)
    }
}

fn command_failed<T>(stderr: &[u8]) -> Result<T> {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    Err(HerdrError::CommandFailed(if stderr.is_empty() {
        "command exited unsuccessfully".to_string()
    } else {
        stderr
    }))
}

fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_result_envelope() {
        let value: serde_json::Value =
            parse_json_envelope(br#"{"id":"x","result":{"ok":true}}"#).unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn parses_error_envelope_with_missing_id() {
        let error = parse_json_envelope::<Value>(
            br#"{"error":{"code":"pane_not_found","message":"missing"}}"#,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            HerdrError::Command {
                code,
                message
            } if code == "pane_not_found" && message == "missing"
        ));
    }

    #[test]
    fn parses_plain_json_when_not_enveloped() {
        let value: serde_json::Value = parse_json_value_or_envelope(br#"{"ok":true}"#).unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn non_json_is_tolerated_as_json_error() {
        let error = parse_json_envelope::<Value>(b"Error: Os { code: 2 }").unwrap_err();
        assert!(matches!(error, HerdrError::Json(_)));
    }

    #[test]
    fn decodes_agent_start_fixture() {
        let json = br#"{
          "id":"cli:agent:start",
          "result":{
            "agent":{
              "agent_status":"unknown",
              "cwd":"/tmp",
              "focused":true,
              "foreground_cwd":"/tmp",
              "name":"label",
              "pane_id":"w1:p1",
              "revision":0,
              "tab_id":"w1:t1",
              "terminal_id":"term_1",
              "workspace_id":"w1"
            },
            "argv":["bash"],
            "type":"agent_started"
          }
        }"#;
        let result: AgentStartResult = parse_json_envelope(json).unwrap();
        assert_eq!(result.agent.pane_id, "w1:p1");
        assert_eq!(result.agent.terminal_id.as_deref(), Some("term_1"));
        assert_eq!(result.agent.agent_status, "unknown");
    }

    #[test]
    fn decodes_pane_get_fixture_with_agent_session() {
        let json = br#"{
          "id":"cli:pane:get",
          "result":{
            "pane":{
              "agent_status":"working",
              "cwd":"/tmp",
              "focused":true,
              "foreground_cwd":"/tmp",
              "label":"label",
              "pane_id":"w1:p1",
              "revision":1,
              "tab_id":"w1:t1",
              "terminal_id":"term_1",
              "workspace_id":"w1",
              "agent_session":{"source":"transcript","agent":"codex","kind":"file","value":"abc"}
            },
            "type":"pane_info"
          }
        }"#;
        let result: PaneGetResult = parse_json_envelope(json).unwrap();
        assert_eq!(result.pane.label.as_deref(), Some("label"));
        assert_eq!(
            result.pane.agent_session.as_ref().unwrap().value.as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn discovery_prefers_config_then_env_then_path() {
        let temp = tempdir().unwrap();
        let config = temp.path().join("config-herdr");
        let env_bin = temp.path().join("env-herdr");
        let path_bin = temp.path().join("path-herdr");
        fs::write(&config, "").unwrap();
        fs::write(&env_bin, "").unwrap();
        fs::write(&path_bin, "").unwrap();

        let found = discover_herdr_with(
            config.to_str().unwrap(),
            |_| Some(OsString::from(&env_bin)),
            |_| Some(path_bin.clone()),
        )
        .unwrap();
        assert_eq!(found, config);

        let found = discover_herdr_with(
            "",
            |_| Some(OsString::from(&env_bin)),
            |_| Some(path_bin.clone()),
        )
        .unwrap();
        assert_eq!(found, env_bin);

        let found = discover_herdr_with("", |_| None, |_| Some(path_bin.clone())).unwrap();
        assert_eq!(found, path_bin);
    }

    #[test]
    fn discovery_reports_not_found() {
        let error = discover_herdr_with("", |_| None, |_| None).unwrap_err();
        assert!(matches!(error, HerdrError::NotFound));
    }

    #[test]
    fn status_transition_requires_working_before_idle() {
        let mut tracker = CompletionTracker::status_transition();
        assert_eq!(tracker.observe(AgentStatus::Idle, 0), None);
        assert_eq!(tracker.observe(AgentStatus::Idle, 750), None);
        assert_eq!(tracker.observe(AgentStatus::Working, 1500), None);
        assert_eq!(
            tracker.observe(AgentStatus::Idle, 2250),
            Some(CompletionOutcome::Done)
        );
    }

    #[test]
    fn idle_without_working_is_not_done_without_grace() {
        let mut tracker = CompletionTracker::status_transition();
        assert_eq!(tracker.observe(AgentStatus::Idle, 0), None);
        assert_eq!(tracker.observe(AgentStatus::Idle, 10_000), None);
    }

    #[test]
    fn grace_variant_accepts_stable_idle() {
        let mut tracker = CompletionTracker::with_grace(5_000);
        assert_eq!(tracker.observe(AgentStatus::Idle, 0), None);
        assert_eq!(tracker.observe(AgentStatus::Idle, 4_999), None);
        assert_eq!(
            tracker.observe(AgentStatus::Idle, 5_000),
            Some(CompletionOutcome::Done)
        );
    }

    #[test]
    fn blocked_status_wins() {
        let mut tracker = CompletionTracker::status_transition();
        assert_eq!(tracker.observe(AgentStatus::Working, 0), None);
        assert_eq!(
            tracker.observe(AgentStatus::Blocked, 750),
            Some(CompletionOutcome::Blocked)
        );
    }
}
