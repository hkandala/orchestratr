use std::path::Path;
use std::sync::LazyLock;

use super::{
    push_flag_value, Completion, Profile, RecipeAction, ShutdownStep, StartupStep,
    TranscriptAdapter, TranscriptError, TranscriptResult,
};

pub struct OpenCodeProfile;

pub struct OpenCodeTranscript;

static STARTUP: LazyLock<Vec<StartupStep>> = LazyLock::new(|| {
    vec![StartupStep {
        detect_substring: "Update available".to_string(),
        actions: vec![
            RecipeAction::SendKey("escape".to_string()),
            RecipeAction::SendKey("escape".to_string()),
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

impl Profile for OpenCodeProfile {
    fn harness(&self) -> &'static str {
        "opencode"
    }

    fn launch_argv(&self, model: &str, _effort: &str, _bypass: bool) -> Vec<String> {
        let mut argv = vec!["opencode".to_string()];
        push_flag_value(&mut argv, "--model", model);
        argv
    }

    fn exec_argv(&self, _model: &str, _effort: &str, _prompt: &str) -> Option<Vec<String>> {
        None
    }

    fn startup_recipe(&self) -> &[StartupStep] {
        STARTUP.as_slice()
    }

    fn completion(&self) -> Completion {
        Completion::StatusWithGrace(5_000)
    }

    fn shutdown_recipe(&self) -> &[ShutdownStep] {
        SHUTDOWN.as_slice()
    }

    fn transcript(&self) -> Option<&dyn TranscriptAdapter> {
        Some(&OpenCodeTranscript)
    }

    fn limit_screen_markers(&self) -> &[&'static str] {
        LIMITS
    }
}

impl TranscriptAdapter for OpenCodeTranscript {
    fn extract_last_response(
        &self,
        _home: &Path,
        _session_ref: &str,
    ) -> TranscriptResult<Option<String>> {
        Err(TranscriptError::NotImplemented(
            "opencode transcript adapter via opencode export",
        ))
    }
}
