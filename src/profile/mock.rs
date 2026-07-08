use std::path::PathBuf;
use std::sync::LazyLock;

use super::{Completion, Profile, RecipeAction, ShutdownStep, StartupStep, TranscriptAdapter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockProfile {
    bin_path: PathBuf,
}

static STARTUP: &[StartupStep] = &[];
static SHUTDOWN: LazyLock<Vec<ShutdownStep>> = LazyLock::new(|| {
    vec![
        ShutdownStep {
            action: RecipeAction::SendText("[[exit]]".to_string()),
            deadline_ms: 5_000,
        },
        ShutdownStep {
            action: RecipeAction::SendKey("enter".to_string()),
            deadline_ms: 5_000,
        },
    ]
});
static LIMITS: &[&str] = &[];

impl Profile for MockProfile {
    fn harness(&self) -> &'static str {
        "mock"
    }

    fn launch_argv(&self, _model: &str, _effort: &str, _bypass: bool) -> Vec<String> {
        vec![self.bin_path.display().to_string()]
    }

    fn exec_argv(&self, _model: &str, _effort: &str, _prompt: &str) -> Option<Vec<String>> {
        None
    }

    fn startup_recipe(&self) -> &[StartupStep] {
        STARTUP
    }

    fn completion(&self) -> Completion {
        Completion::OutputMarker {
            done: "MOCK_DONE".to_string(),
            blocked: "MOCK_BLOCKED".to_string(),
        }
    }

    fn shutdown_recipe(&self) -> &[ShutdownStep] {
        SHUTDOWN.as_slice()
    }

    fn transcript(&self) -> Option<&dyn TranscriptAdapter> {
        None
    }

    fn limit_screen_markers(&self) -> &[&'static str] {
        LIMITS
    }
}

pub fn mock_profile() -> MockProfile {
    MockProfile {
        bin_path: default_mock_agent_path(),
    }
}

pub fn mock_profile_with_path(path: PathBuf) -> MockProfile {
    MockProfile { bin_path: path }
}

pub fn default_mock_agent_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("orcr-mock-agent")))
        .unwrap_or_else(|| PathBuf::from("orcr-mock-agent"))
}
