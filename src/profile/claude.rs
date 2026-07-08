use std::path::Path;
use std::sync::LazyLock;

use serde_json::Value;

use super::{
    find_matching_jsonl, push_flag_value, read_jsonl_values, Completion, Profile, RecipeAction,
    ShutdownStep, StartupStep, TranscriptAdapter, TranscriptResult,
};

pub struct ClaudeProfile;

pub struct ClaudeTranscript;

static STARTUP: &[StartupStep] = &[];
static SHUTDOWN: LazyLock<Vec<ShutdownStep>> = LazyLock::new(|| {
    vec![
        ShutdownStep {
            action: RecipeAction::SendText("/exit".to_string()),
            deadline_ms: 5_000,
        },
        ShutdownStep {
            action: RecipeAction::SendKey("enter".to_string()),
            deadline_ms: 5_000,
        },
    ]
});
static LIMITS: &[&str] = &[];

impl Profile for ClaudeProfile {
    fn harness(&self) -> &'static str {
        "claude"
    }

    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String> {
        let mut argv = vec!["claude".to_string()];
        if bypass {
            argv.push("--dangerously-skip-permissions".to_string());
        }
        push_flag_value(&mut argv, "--model", model);
        push_flag_value(&mut argv, "--effort", effort);
        argv
    }

    fn exec_argv(&self, _model: &str, _effort: &str, _prompt: &str) -> Option<Vec<String>> {
        None
    }

    fn startup_recipe(&self) -> &[StartupStep] {
        STARTUP
    }

    fn completion(&self) -> Completion {
        Completion::StatusTransition
    }

    fn shutdown_recipe(&self) -> &[ShutdownStep] {
        SHUTDOWN.as_slice()
    }

    fn transcript(&self) -> Option<&dyn TranscriptAdapter> {
        Some(&ClaudeTranscript)
    }

    fn limit_screen_markers(&self) -> &[&'static str] {
        LIMITS
    }
}

impl TranscriptAdapter for ClaudeTranscript {
    fn extract_last_response(
        &self,
        home: &Path,
        session_ref: &str,
    ) -> TranscriptResult<Option<String>> {
        let root = home.join(".claude").join("projects");
        let mut latest = None;
        for path in find_matching_jsonl(&root, session_ref)? {
            for line in read_jsonl_values(&path)? {
                if let Some(text) = claude_assistant_text(&line) {
                    latest = Some(text);
                }
            }
        }
        Ok(latest)
    }
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
