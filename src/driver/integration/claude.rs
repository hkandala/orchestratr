//! Built-in Claude Code integration.

use super::{AgentIntegration, LaunchPlan, TranscriptFormat, TuningParams};
use crate::error::{OrcrError, Result};
use serde_json::json;

pub(super) const MODELS: &[&str] = &["sonnet", "opus", "fable", "haiku"];
pub(super) const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultracode"];

pub(super) struct ClaudeIntegration;

impl AgentIntegration for ClaudeIntegration {
    fn provider(&self) -> &'static str {
        "claude"
    }

    fn validate_routing(&self, model: &str, effort: &str) -> Result<()> {
        if !MODELS.contains(&model) {
            return Err(OrcrError::invalid_request(
                format!(
                    "invalid Claude model `{model}`; valid models: {}",
                    MODELS.join(", ")
                ),
                "invalid_model",
            )
            .with_details(json!({
                "reason": "invalid_model",
                "provider": self.provider(),
                "model": model,
                "valid_models": MODELS
            })));
        }
        if !EFFORTS.contains(&effort) {
            return Err(OrcrError::invalid_request(
                format!(
                    "invalid effort `{effort}` for Claude model `{model}`; valid efforts: {}",
                    EFFORTS.join(", ")
                ),
                "invalid_effort",
            )
            .with_details(json!({
                "reason": "invalid_effort",
                "provider": self.provider(),
                "model": model,
                "effort": effort,
                "valid_efforts": EFFORTS
            })));
        }
        Ok(())
    }

    fn launch_plan(&self, model: Option<&str>, effort: Option<&str>) -> Result<LaunchPlan> {
        let mut argv = vec![
            "claude".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        if let Some(model) = model.filter(|s| !s.is_empty()) {
            argv.extend(["--model".to_string(), model.to_string()]);
        }
        if let Some(effort) = effort.filter(|s| !s.is_empty()) {
            argv.extend(["--effort".to_string(), effort.to_string()]);
        }
        Ok(LaunchPlan {
            argv,
            shutdown_line: None,
        })
    }

    fn tuning_defaults(&self) -> TuningParams {
        TuningParams::real_provider_defaults()
    }

    fn transcript_format(&self) -> TranscriptFormat {
        TranscriptFormat::Claude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_is_strict_and_lists_valid_values() {
        let integration = ClaudeIntegration;
        for model in MODELS {
            for effort in EFFORTS {
                integration.validate_routing(model, effort).unwrap();
            }
        }

        let model = integration
            .validate_routing("sonnet-5", "medium")
            .unwrap_err();
        assert_eq!(model.details["reason"], json!("invalid_model"));
        assert_eq!(model.details["valid_models"], json!(MODELS));
        assert!(model.message.contains("sonnet, opus, fable, haiku"));

        let effort = integration
            .validate_routing("sonnet", "extreme")
            .unwrap_err();
        assert_eq!(effort.details["reason"], json!("invalid_effort"));
        assert_eq!(effort.details["valid_efforts"], json!(EFFORTS));
        assert!(effort.message.contains("ultracode"));
    }
}
