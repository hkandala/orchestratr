use std::path::Path;
use std::sync::LazyLock;

use super::{
    push_flag_value, Completion, Profile, RecipeAction, ShutdownStep, StartupStep,
    TranscriptAdapter, TranscriptError, TranscriptResult,
};

pub struct PiProfile;

pub struct PiTranscript;

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

impl Profile for PiProfile {
    fn harness(&self) -> &'static str {
        "pi"
    }

    fn launch_argv(&self, model: &str, effort: &str, _bypass: bool) -> Vec<String> {
        let mut argv = vec!["pi".to_string()];
        push_flag_value(&mut argv, "--model", model);
        push_flag_value(&mut argv, "--thinking", effort);
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
        Some(&PiTranscript)
    }

    fn limit_screen_markers(&self) -> &[&'static str] {
        LIMITS
    }
}

impl TranscriptAdapter for PiTranscript {
    fn extract_last_response(
        &self,
        _home: &Path,
        _session_ref: &str,
    ) -> TranscriptResult<Option<String>> {
        Err(TranscriptError::NotImplemented(
            "pi transcript adapter for ~/.pi/agent/sessions/**/*.jsonl",
        ))
    }
}
