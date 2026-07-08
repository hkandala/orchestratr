use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("transcript adapter is not implemented for {0}")]
    NotImplemented(&'static str),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type TranscriptResult<T> = std::result::Result<T, TranscriptError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionStrategy {
    StatusTransition,
    StatusWithGrace(u64),
    OutputMarker { done: String, blocked: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipeAction {
    SendText(String),
    SendKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipeRule {
    pub detect_substring: String,
    pub actions: Vec<RecipeAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartupRecipe {
    pub rules: Vec<RecipeRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownRecipe {
    pub actions: Vec<RecipeAction>,
    pub deadline_ms: u64,
}

pub trait Profile: Send + Sync {
    fn name(&self) -> &'static str;
    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String>;
    fn startup_recipe(&self) -> StartupRecipe;
    fn completion(&self) -> CompletionStrategy;
    fn shutdown_recipe(&self) -> ShutdownRecipe;
    fn transcript_adapter(&self, home: &Path, session_id: &str)
        -> TranscriptResult<Option<String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessProfile {
    Claude,
    Codex,
    Pi,
    OpenCode,
    Mock { bin_path: PathBuf },
}

impl Profile for HarnessProfile {
    fn name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::OpenCode => "opencode",
            Self::Mock { .. } => "mock",
        }
    }

    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String> {
        match self {
            Self::Claude => {
                let mut argv = vec!["claude".to_string()];
                if bypass {
                    argv.push("--dangerously-skip-permissions".to_string());
                }
                push_flag_value(&mut argv, "--model", model);
                push_flag_value(&mut argv, "--effort", effort);
                argv
            }
            Self::Codex => {
                let mut argv = vec!["codex".to_string()];
                if bypass {
                    argv.push("--dangerously-bypass-approvals-and-sandbox".to_string());
                }
                push_flag_value(&mut argv, "--model", model);
                if !effort.is_empty() {
                    argv.push("-c".to_string());
                    argv.push(format!("model_reasoning_effort=\"{effort}\""));
                }
                argv
            }
            Self::Pi => {
                let mut argv = vec!["pi".to_string()];
                push_flag_value(&mut argv, "--model", model);
                push_flag_value(&mut argv, "--thinking", effort);
                argv
            }
            Self::OpenCode => {
                let mut argv = vec!["opencode".to_string()];
                push_flag_value(&mut argv, "--model", model);
                argv
            }
            Self::Mock { bin_path } => vec![bin_path.display().to_string()],
        }
    }

    fn startup_recipe(&self) -> StartupRecipe {
        match self {
            Self::Codex => StartupRecipe {
                rules: vec![RecipeRule {
                    detect_substring: "A newer version of Codex is available".to_string(),
                    actions: vec![
                        RecipeAction::SendText("2".to_string()),
                        RecipeAction::SendKey("enter".to_string()),
                    ],
                }],
            },
            Self::OpenCode => StartupRecipe {
                rules: vec![RecipeRule {
                    detect_substring: "Update available".to_string(),
                    actions: vec![
                        RecipeAction::SendKey("escape".to_string()),
                        RecipeAction::SendKey("escape".to_string()),
                    ],
                }],
            },
            Self::Claude | Self::Pi | Self::Mock { .. } => StartupRecipe::default(),
        }
    }

    fn completion(&self) -> CompletionStrategy {
        match self {
            Self::OpenCode => CompletionStrategy::StatusWithGrace(5_000),
            Self::Mock { .. } => CompletionStrategy::OutputMarker {
                done: "MOCK_DONE".to_string(),
                blocked: "MOCK_BLOCKED".to_string(),
            },
            Self::Claude | Self::Codex | Self::Pi => CompletionStrategy::StatusTransition,
        }
    }

    fn shutdown_recipe(&self) -> ShutdownRecipe {
        let actions = match self {
            Self::Claude | Self::Codex | Self::Pi | Self::OpenCode => {
                vec![
                    RecipeAction::SendText("/exit".to_string()),
                    RecipeAction::SendKey("enter".to_string()),
                ]
            }
            Self::Mock { .. } => vec![
                RecipeAction::SendText("[[exit]]".to_string()),
                RecipeAction::SendKey("enter".to_string()),
            ],
        };
        ShutdownRecipe {
            actions,
            deadline_ms: 5_000,
        }
    }

    fn transcript_adapter(
        &self,
        home: &Path,
        session_id: &str,
    ) -> TranscriptResult<Option<String>> {
        match self {
            Self::Claude => extract_claude_transcript(home, session_id),
            Self::Codex => extract_codex_transcript(home, session_id),
            Self::Pi => Err(TranscriptError::NotImplemented(
                "pi transcript adapter for ~/.pi/agent/sessions/**/*.jsonl",
            )),
            Self::OpenCode => Err(TranscriptError::NotImplemented(
                "opencode transcript adapter via opencode export",
            )),
            Self::Mock { .. } => Ok(None),
        }
    }
}

impl StartupRecipe {
    pub fn actions_for_screen(&self, screen: &str) -> Vec<RecipeAction> {
        self.rules
            .iter()
            .filter(|rule| screen.contains(&rule.detect_substring))
            .flat_map(|rule| rule.actions.clone())
            .collect()
    }
}

pub fn lookup(name: &str) -> Option<HarnessProfile> {
    match name {
        "claude" => Some(HarnessProfile::Claude),
        "codex" => Some(HarnessProfile::Codex),
        "pi" => Some(HarnessProfile::Pi),
        "opencode" => Some(HarnessProfile::OpenCode),
        "mock" => Some(mock_profile()),
        _ => None,
    }
}

pub fn mock_profile() -> HarnessProfile {
    HarnessProfile::Mock {
        bin_path: default_mock_agent_path(),
    }
}

pub fn mock_profile_with_path(path: PathBuf) -> HarnessProfile {
    HarnessProfile::Mock { bin_path: path }
}

pub fn default_mock_agent_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("orcr-mock-agent")))
        .unwrap_or_else(|| PathBuf::from("orcr-mock-agent"))
}

pub fn extract_claude_transcript(
    home: &Path,
    session_id: &str,
) -> TranscriptResult<Option<String>> {
    let root = home.join(".claude").join("projects");
    let mut latest = None;
    for path in find_matching_jsonl(&root, session_id)? {
        for line in read_jsonl_values(&path)? {
            if let Some(text) = claude_assistant_text(&line) {
                latest = Some(text);
            }
        }
    }
    Ok(latest)
}

pub fn extract_codex_transcript(home: &Path, session_id: &str) -> TranscriptResult<Option<String>> {
    let root = home.join(".codex");
    let mut task_complete = None;
    let mut agent_message = None;
    let mut response_item = None;
    for path in find_matching_jsonl(&root, session_id)? {
        for line in read_jsonl_values(&path)? {
            if let Some(text) = line
                .get("task_complete")
                .and_then(|value| value.get("last_agent_message"))
                .and_then(Value::as_str)
            {
                task_complete = Some(text.to_string());
            }
            if let Some(text) = line.get("agent_message").and_then(Value::as_str) {
                agent_message = Some(text.to_string());
            }
            if let Some(text) = codex_response_item_text(&line) {
                response_item = Some(text);
            }
        }
    }
    Ok(task_complete.or(agent_message).or(response_item))
}

fn push_flag_value(argv: &mut Vec<String>, flag: &str, value: &str) {
    if !value.is_empty() {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

fn find_matching_jsonl(root: &Path, session_id: &str) -> TranscriptResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    visit_files(root, &mut |path| {
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_id))
        {
            paths.push(path.to_path_buf());
        }
        Ok(())
    })?;
    paths.sort();
    Ok(paths)
}

fn visit_files(
    root: &Path,
    visitor: &mut impl FnMut(&Path) -> TranscriptResult<()>,
) -> TranscriptResult<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            visit_files(&path, visitor)?;
        } else {
            visitor(&path)?;
        }
    }
    Ok(())
}

fn read_jsonl_values(path: &Path) -> TranscriptResult<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(TranscriptError::from))
        .collect()
}

fn claude_assistant_text(value: &Value) -> Option<String> {
    let message = value.get("message")?;
    (message.get("role")?.as_str()? == "assistant").then_some(())?;
    message
        .get("content")?
        .as_array()?
        .iter()
        .rev()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .next()
        .map(ToString::to_string)
}

fn codex_response_item_text(value: &Value) -> Option<String> {
    let item = value.get("response_item").or_else(|| {
        (value.get("type").and_then(Value::as_str) == Some("response_item")).then_some(value)
    })?;
    if item.get("type").and_then(Value::as_str) != Some("output_text") {
        return None;
    }
    item.get("text")
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn launch_argv_by_profile() {
        assert_eq!(
            HarnessProfile::Claude.launch_argv("sonnet", "high", true),
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "sonnet",
                "--effort",
                "high"
            ]
        );
        assert_eq!(
            HarnessProfile::Claude.launch_argv("", "", false),
            vec!["claude"]
        );
        assert_eq!(
            HarnessProfile::Codex.launch_argv("gpt-5", "medium", true),
            vec![
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--model",
                "gpt-5",
                "-c",
                "model_reasoning_effort=\"medium\""
            ]
        );
        assert_eq!(
            HarnessProfile::Pi.launch_argv("m", "deep", false),
            vec!["pi", "--model", "m", "--thinking", "deep"]
        );
        assert_eq!(
            HarnessProfile::OpenCode.launch_argv("m", "ignored", true),
            vec!["opencode", "--model", "m"]
        );
        assert_eq!(
            mock_profile_with_path(PathBuf::from("/tmp/orcr-mock-agent"))
                .launch_argv("", "", false),
            vec!["/tmp/orcr-mock-agent"]
        );
    }

    #[test]
    fn completion_strategy_by_profile() {
        assert_eq!(
            HarnessProfile::Claude.completion(),
            CompletionStrategy::StatusTransition
        );
        assert_eq!(
            HarnessProfile::OpenCode.completion(),
            CompletionStrategy::StatusWithGrace(5_000)
        );
        assert_eq!(
            mock_profile_with_path(PathBuf::from("mock")).completion(),
            CompletionStrategy::OutputMarker {
                done: "MOCK_DONE".to_string(),
                blocked: "MOCK_BLOCKED".to_string()
            }
        );
    }

    #[test]
    fn startup_recipe_detection() {
        let actions = HarnessProfile::Codex
            .startup_recipe()
            .actions_for_screen("A newer version of Codex is available\n1. update\n2. skip");
        assert_eq!(
            actions,
            vec![
                RecipeAction::SendText("2".to_string()),
                RecipeAction::SendKey("enter".to_string())
            ]
        );

        let actions = HarnessProfile::OpenCode
            .startup_recipe()
            .actions_for_screen("Update available");
        assert_eq!(
            actions,
            vec![
                RecipeAction::SendKey("escape".to_string()),
                RecipeAction::SendKey("escape".to_string())
            ]
        );
        assert!(HarnessProfile::Claude
            .startup_recipe()
            .actions_for_screen("anything")
            .is_empty());
    }

    #[test]
    fn claude_transcript_extracts_last_assistant_text() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".claude/projects/project-a");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("session-abc.jsonl"),
            include_str!("../tests/fixtures/claude_session.jsonl"),
        )
        .unwrap();

        let text = extract_claude_transcript(temp.path(), "abc").unwrap();
        assert_eq!(text.as_deref(), Some("final assistant answer"));
    }

    #[test]
    fn codex_transcript_extracts_by_priority() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-priority-session.jsonl"),
            include_str!("../tests/fixtures/codex_priority_session.jsonl"),
        )
        .unwrap();

        let text = extract_codex_transcript(temp.path(), "priority-session").unwrap();
        assert_eq!(text.as_deref(), Some("task complete wins"));
    }

    #[test]
    fn codex_transcript_falls_back_to_agent_message() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-agent-session.jsonl"),
            include_str!("../tests/fixtures/codex_agent_session.jsonl"),
        )
        .unwrap();

        let text = extract_codex_transcript(temp.path(), "agent-session").unwrap();
        assert_eq!(text.as_deref(), Some("agent message wins"));
    }

    #[test]
    fn codex_transcript_falls_back_to_response_item() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-response-session.jsonl"),
            include_str!("../tests/fixtures/codex_response_session.jsonl"),
        )
        .unwrap();

        let text = extract_codex_transcript(temp.path(), "response-session").unwrap();
        assert_eq!(text.as_deref(), Some("response item fallback"));
    }

    #[test]
    fn stubs_are_typed_not_implemented() {
        assert!(matches!(
            HarnessProfile::Pi.transcript_adapter(Path::new("/tmp"), "s"),
            Err(TranscriptError::NotImplemented(_))
        ));
        assert!(matches!(
            HarnessProfile::OpenCode.transcript_adapter(Path::new("/tmp"), "s"),
            Err(TranscriptError::NotImplemented(_))
        ));
    }
}
