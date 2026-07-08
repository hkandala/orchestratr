use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

pub mod claude;
pub mod codex;
pub mod mock;
pub mod opencode;
pub mod pi;

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
pub enum Completion {
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
pub struct StartupStep {
    pub detect_substring: String,
    pub actions: Vec<RecipeAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownStep {
    pub action: RecipeAction,
    pub deadline_ms: u64,
}

pub trait TranscriptAdapter: Send + Sync {
    fn extract_last_response(
        &self,
        home: &Path,
        session_ref: &str,
    ) -> TranscriptResult<Option<String>>;

    fn tokens(&self, _home: &Path, _session_ref: &str) -> TranscriptResult<Option<(u64, u64)>> {
        Ok(None)
    }
}

pub trait Profile: Send + Sync {
    fn harness(&self) -> &'static str;
    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String>;
    fn exec_argv(&self, model: &str, effort: &str, prompt: &str) -> Option<Vec<String>>;
    fn startup_recipe(&self) -> &[StartupStep];
    fn completion(&self) -> Completion;
    fn shutdown_recipe(&self) -> &[ShutdownStep];
    fn transcript(&self) -> Option<&dyn TranscriptAdapter>;
    fn limit_screen_markers(&self) -> &[&'static str];
}

pub fn lookup(name: &str) -> Option<Box<dyn Profile>> {
    match name {
        "claude" => Some(Box::new(claude::ClaudeProfile)),
        "codex" => Some(Box::new(codex::CodexProfile)),
        "pi" => Some(Box::new(pi::PiProfile)),
        "opencode" => Some(Box::new(opencode::OpenCodeProfile)),
        "mock" => Some(Box::new(mock::mock_profile())),
        _ => None,
    }
}

pub fn actions_for_screen(recipe: &[StartupStep], screen: &str) -> Vec<RecipeAction> {
    recipe
        .iter()
        .filter(|step| screen.contains(&step.detect_substring))
        .flat_map(|step| step.actions.clone())
        .collect()
}

pub fn push_flag_value(argv: &mut Vec<String>, flag: &str, value: &str) {
    if !value.is_empty() {
        argv.push(flag.to_string());
        argv.push(value.to_string());
    }
}

pub fn find_matching_jsonl(root: &Path, session_ref: &str) -> TranscriptResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    visit_files(root, &mut |path| {
        if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.contains(session_ref))
        {
            paths.push(path.to_path_buf());
        }
        Ok(())
    })?;
    paths.sort();
    Ok(paths)
}

pub fn read_jsonl_values(path: &Path) -> TranscriptResult<Vec<Value>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(TranscriptError::from))
        .collect()
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn launch_argv_by_profile() {
        assert_eq!(
            claude::ClaudeProfile.launch_argv("sonnet", "high", true),
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
            claude::ClaudeProfile.launch_argv("", "", false),
            vec!["claude"]
        );
        assert_eq!(
            codex::CodexProfile.launch_argv("gpt-5", "medium", true),
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
            pi::PiProfile.launch_argv("m", "deep", false),
            vec!["pi", "--model", "m", "--thinking", "deep"]
        );
        assert_eq!(
            opencode::OpenCodeProfile.launch_argv("m", "ignored", true),
            vec!["opencode", "--model", "m"]
        );
        assert_eq!(
            mock::mock_profile_with_path(PathBuf::from("/tmp/orcr-mock-agent"))
                .launch_argv("", "", false),
            vec!["/tmp/orcr-mock-agent"]
        );
    }

    #[test]
    fn completion_by_profile() {
        assert_eq!(
            claude::ClaudeProfile.completion(),
            Completion::StatusTransition
        );
        assert_eq!(
            opencode::OpenCodeProfile.completion(),
            Completion::StatusWithGrace(5_000)
        );
        assert_eq!(
            mock::mock_profile_with_path(PathBuf::from("mock")).completion(),
            Completion::OutputMarker {
                done: "MOCK_DONE".to_string(),
                blocked: "MOCK_BLOCKED".to_string()
            }
        );
    }

    #[test]
    fn startup_recipe_detection() {
        let actions = actions_for_screen(
            codex::CodexProfile.startup_recipe(),
            "A newer version of Codex is available\n1. update\n2. skip",
        );
        assert_eq!(
            actions,
            vec![
                RecipeAction::SendText("2".to_string()),
                RecipeAction::SendKey("enter".to_string())
            ]
        );

        let actions = actions_for_screen(
            opencode::OpenCodeProfile.startup_recipe(),
            "Update available",
        );
        assert_eq!(
            actions,
            vec![
                RecipeAction::SendKey("escape".to_string()),
                RecipeAction::SendKey("escape".to_string())
            ]
        );
        assert!(actions_for_screen(claude::ClaudeProfile.startup_recipe(), "anything").is_empty());
    }

    #[test]
    fn claude_transcript_extracts_last_assistant_text() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".claude/projects/project-a");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("session-abc.jsonl"),
            include_str!("../../tests/fixtures/claude_session.jsonl"),
        )
        .unwrap();

        let text = claude::ClaudeTranscript
            .extract_last_response(temp.path(), "abc")
            .unwrap();
        assert_eq!(text.as_deref(), Some("final assistant answer"));
    }

    #[test]
    fn claude_transcript_extracts_tokens() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".claude/projects/project-a");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("session-abc.jsonl"),
            include_str!("../../tests/fixtures/claude_session.jsonl"),
        )
        .unwrap();

        let tokens = claude::ClaudeTranscript.tokens(temp.path(), "abc").unwrap();
        assert_eq!(tokens, Some((11, 7)));
    }

    #[test]
    fn codex_transcript_extracts_by_priority() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-priority-session.jsonl"),
            include_str!("../../tests/fixtures/codex_priority_session.jsonl"),
        )
        .unwrap();

        let text = codex::CodexTranscript
            .extract_last_response(temp.path(), "priority-session")
            .unwrap();
        assert_eq!(text.as_deref(), Some("task complete wins"));
    }

    #[test]
    fn codex_transcript_falls_back_to_agent_message() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-agent-session.jsonl"),
            include_str!("../../tests/fixtures/codex_agent_session.jsonl"),
        )
        .unwrap();

        let text = codex::CodexTranscript
            .extract_last_response(temp.path(), "agent-session")
            .unwrap();
        assert_eq!(text.as_deref(), Some("agent message wins"));
    }

    #[test]
    fn codex_transcript_extracts_tokens() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-agent-session.jsonl"),
            include_str!("../../tests/fixtures/codex_agent_session.jsonl"),
        )
        .unwrap();

        let tokens = codex::CodexTranscript
            .tokens(temp.path(), "agent-session")
            .unwrap();
        assert_eq!(tokens, Some((13, 5)));
    }

    #[test]
    fn codex_transcript_falls_back_to_response_item() {
        let temp = tempdir().unwrap();
        let dir = temp.path().join(".codex/rollouts");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-response-session.jsonl"),
            include_str!("../../tests/fixtures/codex_response_session.jsonl"),
        )
        .unwrap();

        let text = codex::CodexTranscript
            .extract_last_response(temp.path(), "response-session")
            .unwrap();
        assert_eq!(text.as_deref(), Some("response item fallback"));
    }

    #[test]
    fn stubs_are_typed_not_implemented() {
        assert!(matches!(
            pi::PiTranscript.extract_last_response(Path::new("/tmp"), "s"),
            Err(TranscriptError::NotImplemented(_))
        ));
        assert!(matches!(
            opencode::OpenCodeTranscript.extract_last_response(Path::new("/tmp"), "s"),
            Err(TranscriptError::NotImplemented(_))
        ));
    }
}
