use std::path::Path;
use std::sync::LazyLock;

use serde_json::Value;

use super::{
    find_matching_jsonl, push_flag_value, read_jsonl_values, Completion, Profile, RecipeAction,
    ShutdownStep, StartupStep, TranscriptAdapter, TranscriptResult,
};

pub struct CodexProfile;

pub struct CodexTranscript;

static STARTUP: LazyLock<Vec<StartupStep>> = LazyLock::new(|| {
    vec![StartupStep {
        detect_substring: "A newer version of Codex is available".to_string(),
        actions: vec![
            RecipeAction::SendText("2".to_string()),
            RecipeAction::SendKey("enter".to_string()),
        ],
    }]
});
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

impl Profile for CodexProfile {
    fn harness(&self) -> &'static str {
        "codex"
    }

    fn launch_argv(&self, model: &str, effort: &str, bypass: bool) -> Vec<String> {
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

    fn exec_argv(&self, _model: &str, _effort: &str, _prompt: &str) -> Option<Vec<String>> {
        None
    }

    fn startup_recipe(&self) -> &[StartupStep] {
        STARTUP.as_slice()
    }

    fn completion(&self) -> Completion {
        Completion::StatusTransition
    }

    fn shutdown_recipe(&self) -> &[ShutdownStep] {
        SHUTDOWN.as_slice()
    }

    fn transcript(&self) -> Option<&dyn TranscriptAdapter> {
        Some(&CodexTranscript)
    }

    fn limit_screen_markers(&self) -> &[&'static str] {
        LIMITS
    }
}

impl TranscriptAdapter for CodexTranscript {
    fn extract_last_response(
        &self,
        home: &Path,
        session_ref: &str,
    ) -> TranscriptResult<Option<String>> {
        let root = home.join(".codex");
        let mut task_complete = None;
        let mut agent_message = None;
        let mut response_item = None;
        for path in find_matching_jsonl(&root, session_ref)? {
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

    fn tokens(&self, home: &Path, session_ref: &str) -> TranscriptResult<Option<(u64, u64)>> {
        let root = home.join(".codex");
        let mut totals = (0_u64, 0_u64);
        for path in find_matching_jsonl(&root, session_ref)? {
            for line in read_jsonl_values(&path)? {
                if let Some((input, output)) = usage_tokens(&line) {
                    totals.0 = totals.0.saturating_add(input);
                    totals.1 = totals.1.saturating_add(output);
                }
            }
        }
        Ok((totals != (0, 0)).then_some(totals))
    }
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

fn usage_tokens(value: &Value) -> Option<(u64, u64)> {
    let usage = value
        .get("usage")
        .or_else(|| value.get("token_usage"))
        .or_else(|| value.get("tokens"))
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
        })?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    ((input, output) != (0, 0)).then_some((input, output))
}
