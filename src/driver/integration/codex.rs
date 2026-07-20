//! Built-in Codex CLI integration.

use super::{AgentIntegration, LaunchPlan, TranscriptFormat, TuningParams};
use crate::error::{OrcrError, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);
const CATALOG_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelRoute {
    name: String,
    efforts: Vec<String>,
}

type CachedCatalog = Option<(Instant, Vec<ModelRoute>)>;
type CatalogCache = OnceLock<Mutex<CachedCatalog>>;

static MODEL_CACHE: CatalogCache = OnceLock::new();

pub(super) struct CodexIntegration;

impl AgentIntegration for CodexIntegration {
    fn provider(&self) -> &'static str {
        "codex"
    }

    fn validate_routing(&self, model: &str, effort: &str) -> Result<()> {
        let catalog = model_catalog()?;
        let valid: Vec<&str> = catalog.iter().map(|m| m.name.as_str()).collect();
        let Some(route) = catalog.iter().find(|m| m.name == model) else {
            return Err(OrcrError::invalid_request(
                format!(
                    "invalid Codex model `{model}`; valid models: {}",
                    valid.join(", ")
                ),
                "invalid_model",
            )
            .with_details(json!({
                "reason": "invalid_model",
                "provider": self.provider(),
                "model": model,
                "valid_models": valid
            })));
        };
        if !route.efforts.iter().any(|e| e == effort) {
            return Err(OrcrError::invalid_request(
                format!(
                    "invalid effort `{effort}` for Codex model `{model}`; valid efforts: {}",
                    route.efforts.join(", ")
                ),
                "invalid_effort",
            )
            .with_details(json!({
                "reason": "invalid_effort",
                "provider": self.provider(),
                "model": model,
                "effort": effort,
                "valid_efforts": route.efforts
            })));
        }
        Ok(())
    }

    fn launch_plan(&self, model: Option<&str>, effort: Option<&str>) -> Result<LaunchPlan> {
        let mut argv = vec![
            "codex".to_string(),
            "--dangerously-bypass-approvals-and-sandbox".to_string(),
        ];
        if let Some(model) = model.filter(|s| !s.is_empty()) {
            argv.extend(["--model".to_string(), model.to_string()]);
        }
        if let Some(effort) = effort.filter(|s| !s.is_empty()) {
            argv.extend(["-c".to_string(), format!("model_reasoning_effort={effort}")]);
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
        TranscriptFormat::Codex
    }
}

fn model_catalog() -> Result<Vec<ModelRoute>> {
    let cache = MODEL_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = cache.lock().unwrap();
    if let Some((fetched, catalog)) = guard.as_ref() {
        if fetched.elapsed() < CATALOG_TTL {
            return Ok(catalog.clone());
        }
    }
    let catalog = query_model_catalog()?;
    *guard = Some((Instant::now(), catalog.clone()));
    Ok(catalog)
}

fn query_model_catalog() -> Result<Vec<ModelRoute>> {
    let mut child = Command::new("codex")
        .args(["app-server", "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| catalog_error(format!("cannot start `codex app-server`: {e}")))?;

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let write_result = (|| -> std::io::Result<()> {
        for request in [
            json!({
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "orcr", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"experimentalApi": true}
                }
            }),
            json!({"method": "initialized", "params": {}}),
            json!({
                "id": 2,
                "method": "model/list",
                "params": {"includeHidden": true, "limit": 100}
            }),
        ] {
            writeln!(stdin, "{request}")?;
        }
        stdin.flush()
    })();
    if let Err(e) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(catalog_error(format!(
            "cannot query the Codex model catalog: {e}"
        )));
    }

    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(std::result::Result::ok) {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value.get("id").and_then(|id| id.as_i64()) == Some(2) {
                let _ = tx.send(value);
                return;
            }
        }
    });

    let response = match rx.recv_timeout(CATALOG_TIMEOUT) {
        Ok(response) => response,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(catalog_error(
                "timed out querying `codex app-server` for valid models",
            ));
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    parse_model_catalog(&response)
}

fn parse_model_catalog(response: &Value) -> Result<Vec<ModelRoute>> {
    let data = response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .ok_or_else(|| catalog_error(format!("invalid catalog response: {response}")))?;
    let catalog: Vec<ModelRoute> = data
        .iter()
        .filter_map(|item| {
            let name = item.get("model")?.as_str()?.to_owned();
            let efforts = item
                .get("supportedReasoningEfforts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|e| e.get("reasoningEffort").and_then(Value::as_str))
                .map(str::to_owned)
                .collect();
            Some(ModelRoute { name, efforts })
        })
        .collect();
    if catalog.is_empty() {
        return Err(catalog_error("Codex returned an empty model catalog"));
    }
    Ok(catalog)
}

fn catalog_error(message: impl Into<String>) -> OrcrError {
    OrcrError::environment("model_catalog_unavailable", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_models_and_model_specific_efforts() {
        let response = json!({
            "id": 2,
            "result": {"data": [
                {
                    "model": "gpt-a",
                    "supportedReasoningEfforts": [
                        {"reasoningEffort": "low"},
                        {"reasoningEffort": "high"}
                    ]
                },
                {
                    "model": "gpt-b",
                    "supportedReasoningEfforts": [{"reasoningEffort": "medium"}]
                }
            ]}
        });
        assert_eq!(
            parse_model_catalog(&response).unwrap(),
            vec![
                ModelRoute {
                    name: "gpt-a".into(),
                    efforts: vec!["low".into(), "high".into()]
                },
                ModelRoute {
                    name: "gpt-b".into(),
                    efforts: vec!["medium".into()]
                }
            ]
        );
    }
}
