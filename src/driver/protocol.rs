//! herdr socket wire protocol types (protocol 16), mirroring the shapes published by
//! `herdr api schema --json`. Requests are `{protocol, id, method, params}`; success
//! responses are `{id, result:{type:"<tag>", ...}}` (a tagged union on `type`); errors
//! are `{id, error:{code, message}}`. Newline-delimited JSON, one request per connection.

use serde::{Deserialize, Serialize};

/// The herdr socket protocol version orcr is built against and requires as a minimum.
pub const MIN_HERDR_PROTOCOL: u32 = 16;

/// Raw agent lifecycle state as reported by herdr. This is
/// the only vocabulary herdr emits; orcr normalizes it (see [`normalize_done`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }
}

/// Normalize a herdr-reported status for orcr's completion check: a herdr
/// `done` is treated as `idle` (and only becomes `ended` when pane closure is also
/// observed, which the caller handles). Everything else passes through unchanged.
pub fn normalize_done(status: AgentStatus) -> AgentStatus {
    match status {
        AgentStatus::Done => AgentStatus::Idle,
        other => other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionRefKind {
    Id,
    Path,
}

impl AgentSessionRefKind {
    /// The stable string used when persisting the transcript-pointer kind (`id`/`path`).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentSessionRefKind::Id => "id",
            AgentSessionRefKind::Path => "path",
        }
    }
}

/// The transcript pointer herdr reports per pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSessionInfo {
    pub source: String,
    pub agent: String,
    pub kind: AgentSessionRefKind,
    pub value: String,
}

/// A herdr agent row (from `agent.list`, `session.snapshot.agents`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub terminal_id: String,
    pub agent_status: AgentStatus,
    pub workspace_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub focused: bool,
    pub revision: u64,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// A herdr pane row (from `pane.list`, `pane.get`, `session.snapshot.panes`). Includes
/// non-agent panes (plain shells report `agent_status: unknown`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    pub agent_status: AgentStatus,
    pub revision: u64,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSessionInfo>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub number: i64,
    pub label: String,
    pub focused: bool,
    pub pane_count: i64,
    pub tab_count: i64,
    pub active_tab_id: String,
    pub agent_status: AgentStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: i64,
    pub label: String,
    pub focused: bool,
    pub pane_count: i64,
    pub agent_status: AgentStatus,
}

/// The whole session tree in one document (from `session.snapshot`). Scoped to the
/// socket's session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
}

/// Where `pane.read` reads content from (delivery verification).
/// `Visible` = the current viewport (what a real TUI shows, incl. the input box).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

impl ReadSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadSource::Visible => "visible",
            ReadSource::Recent => "recent",
            ReadSource::RecentUnwrapped => "recent_unwrapped",
            ReadSource::Detection => "detection",
        }
    }
}

/// Result of `pane.read` — the rendered pane content (ANSI stripped). Used to detect TUI
/// readiness and verify that a delivered prompt actually left the input box.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneReadResult {
    pub pane_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    /// `source`/`format` come back as their enum strings; kept as `String` for leniency.
    pub source: String,
    pub format: String,
    pub text: String,
    pub revision: u64,
    pub truncated: bool,
}

/// Result of `pane.move`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaneMoveResult {
    pub changed: bool,
    pub pane: PaneInfo,
    pub previous_pane_id: String,
    pub previous_workspace_id: String,
    pub previous_tab_id: String,
    pub focused_pane_id: String,
    #[serde(default)]
    pub closed_tab_id: Option<String>,
    #[serde(default)]
    pub closed_workspace_id: Option<String>,
    #[serde(default)]
    pub created_tab: Option<TabInfo>,
    #[serde(default)]
    pub created_workspace: Option<WorkspaceInfo>,
}

/// The pong from `ping` — the handshake payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pong {
    pub version: String,
    pub protocol: u32,
}

// --- Request params ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

/// Params for `agent.start`: herdr creates the tab + pane and the returned
/// ids are authoritative — orcr does not pre-create tabs.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStartParams {
    pub name: String,
    pub argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    pub focus: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// A pane-move destination (tagged union on `type`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneMoveDestination {
    /// Move into an existing tab, splitting off an existing pane.
    Tab {
        tab_id: String,
        split: SplitDirection,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_pane_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ratio: Option<f64>,
    },
    /// Move into a new tab (optionally in a specific workspace) — used for GC parking.
    NewTab {
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Move into a brand-new workspace.
    NewWorkspace {
        #[serde(skip_serializing_if = "Option::is_none")]
        tab_label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// The agent state a pane may self-report via `pane.report_agent` (used by the mock).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl PaneAgentState {
    pub fn as_str(self) -> &'static str {
        match self {
            PaneAgentState::Idle => "idle",
            PaneAgentState::Working => "working",
            PaneAgentState::Blocked => "blocked",
            PaneAgentState::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_normalizes_to_idle() {
        assert_eq!(normalize_done(AgentStatus::Done), AgentStatus::Idle);
        assert_eq!(normalize_done(AgentStatus::Working), AgentStatus::Working);
        assert_eq!(normalize_done(AgentStatus::Blocked), AgentStatus::Blocked);
        assert_eq!(normalize_done(AgentStatus::Idle), AgentStatus::Idle);
        assert_eq!(normalize_done(AgentStatus::Unknown), AgentStatus::Unknown);
    }

    #[test]
    fn agent_status_deserializes_all_variants() {
        for (s, want) in [
            ("\"idle\"", AgentStatus::Idle),
            ("\"working\"", AgentStatus::Working),
            ("\"blocked\"", AgentStatus::Blocked),
            ("\"done\"", AgentStatus::Done),
            ("\"unknown\"", AgentStatus::Unknown),
        ] {
            let got: AgentStatus = serde_json::from_str(s).unwrap();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn agent_info_parses_real_shape() {
        let raw = r#"{
            "terminal_id":"term_abc","agent_status":"working","workspace_id":"w3",
            "tab_id":"w3:t1","pane_id":"w3:p1","focused":false,"revision":7,
            "agent":"claude","display_agent":null,
            "agent_session":{"source":"herdr:claude","agent":"claude","kind":"id","value":"uuid-x"},
            "cwd":"/tmp","foreground_cwd":"/tmp","name":null,"title":null,
            "state_labels":{},"custom_status":null,"screen_detection_skipped":false
        }"#;
        let a: AgentInfo = serde_json::from_str(raw).unwrap();
        assert_eq!(a.terminal_id, "term_abc");
        assert_eq!(a.agent_status, AgentStatus::Working);
        assert_eq!(a.agent_session.unwrap().value, "uuid-x");
    }

    #[test]
    fn move_destination_serializes_tagged() {
        let d = PaneMoveDestination::NewTab {
            workspace_id: Some("w9".to_string()),
            label: None,
        };
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["type"], "new_tab");
        assert_eq!(v["workspace_id"], "w9");
        assert!(v.get("label").is_none());
    }

    #[test]
    fn agent_start_params_omit_nulls() {
        let p = AgentStartParams {
            name: "x".into(),
            argv: vec!["claude".into()],
            cwd: None,
            env: Default::default(),
            focus: false,
            split: None,
            tab_id: None,
            workspace_id: Some("w1".into()),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["name"], "x");
        assert_eq!(v["focus"], false);
        assert_eq!(v["workspace_id"], "w1");
        assert!(v.get("cwd").is_none());
        assert!(v.get("tab_id").is_none());
    }
}
