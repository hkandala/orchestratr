//! Per-provider integration state (spec §11.4). A provider is *supported* only when both
//! layers are present: orcr's built-in integration **and** herdr's integration.
//!
//! herdr's integration state is read by parsing `herdr integration status` (no dedicated
//! socket method exists in protocol 16 — see the driver reference). orcr's built-in set
//! (claude + codex in the first release) is known statically.

use crate::error::{OrcrError, Result};
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;

/// Providers with an orcr built-in integration (spec §11.4: claude + codex ship first).
pub const ORCR_BUILTIN_PROVIDERS: &[&str] = &["claude", "codex"];

/// A test-only provider name (`mock`) enabled when `ORCR_ALLOW_MOCK_PROVIDER=1`, backed by
/// the `orcr-mock-agent` binary at `$ORCR_MOCK_AGENT_BIN`. It stands in for a real provider
/// in the e2e gate (it self-reports via `pane.report_agent`, so both observation layers are
/// effectively present). Never available in a normal build.
pub const MOCK_PROVIDER: &str = "mock";

/// True if the test-only mock provider is enabled for this process.
pub fn mock_provider_enabled() -> bool {
    std::env::var("ORCR_ALLOW_MOCK_PROVIDER").as_deref() == Ok("1")
}

/// The orcr-side integration for a provider (spec §11.4): how orcr *drives* it — launch
/// argv (bypass-permissions flags + model/effort), a startup recipe for known modals, and a
/// graceful-shutdown recipe. The transcript adapter + `blocked_kind` classification land in
/// M3.
#[derive(Debug, Clone)]
pub struct LaunchPlan {
    /// The full argv (provider binary + flags) handed to herdr `agent.start`.
    pub argv: Vec<String>,
    /// A best-effort text line to send before closing the pane on graceful shutdown
    /// (`None` = just close the pane). The pane close is the hard guarantee (§5.2).
    pub shutdown_line: Option<String>,
}

/// Build the launch plan for a provider, mapping `model`/`effort` per its CLI (spec §11.4).
/// Empty `model`/`effort` mean "provider default". Unknown providers → `integration_missing`.
pub fn launch_plan(
    provider: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<LaunchPlan> {
    let model = model.filter(|s| !s.is_empty());
    let effort = effort.filter(|s| !s.is_empty());
    match provider {
        "claude" => {
            // Bypass permissions so the agent runs unattended (spec §11.4; everything runs
            // bypass-permissions in this release, §17).
            let mut argv = vec![
                "claude".to_string(),
                "--dangerously-skip-permissions".to_string(),
            ];
            if let Some(m) = model {
                argv.push("--model".to_string());
                argv.push(m.to_string());
            }
            // claude has no separate effort knob; effort is ignored (documented).
            let _ = effort;
            Ok(LaunchPlan {
                argv,
                shutdown_line: None,
            })
        }
        "codex" => {
            let mut argv = vec![
                "codex".to_string(),
                "--dangerously-bypass-approvals-and-sandbox".to_string(),
            ];
            if let Some(m) = model {
                argv.push("--model".to_string());
                argv.push(m.to_string());
            }
            if let Some(e) = effort {
                argv.push("-c".to_string());
                argv.push(format!("model_reasoning_effort={e}"));
            }
            Ok(LaunchPlan {
                argv,
                shutdown_line: None,
            })
        }
        MOCK_PROVIDER if mock_provider_enabled() => {
            let bin = std::env::var("ORCR_MOCK_AGENT_BIN").map_err(|_| {
                OrcrError::server_error(
                    "mock_bin_unset",
                    "ORCR_ALLOW_MOCK_PROVIDER=1 but ORCR_MOCK_AGENT_BIN is not set",
                )
            })?;
            Ok(LaunchPlan {
                argv: vec![bin],
                shutdown_line: Some("/quit".to_string()),
            })
        }
        other => Err(integration_missing(other, &["orcr"])),
    }
}

/// Enforce the both-layers-required rule (spec §11.4): a provider is supported only when
/// orcr's built-in integration **and** herdr's integration are both present. Fails fast with
/// `integration_missing` naming the missing layer(s) and the exact install command; nothing
/// is spawned. The mock provider (test flag) bypasses this check.
pub fn ensure_supported(state: &IntegrationState, provider: &str) -> Result<()> {
    if provider == MOCK_PROVIDER && mock_provider_enabled() {
        return Ok(());
    }
    let orcr = ORCR_BUILTIN_PROVIDERS.contains(&provider);
    let herdr = state.get(provider).map(|p| p.herdr).unwrap_or(false);
    if orcr && herdr {
        return Ok(());
    }
    let mut missing = Vec::new();
    if !orcr {
        missing.push("orcr");
    }
    if !herdr {
        missing.push("herdr");
    }
    Err(integration_missing(provider, &missing))
}

/// The `integration_missing` error (spec §11.4, §13): names the missing layer(s) and the
/// exact fix (exit 2).
fn integration_missing(provider: &str, missing: &[&str]) -> OrcrError {
    let install = if missing.contains(&"herdr") && missing.contains(&"orcr") {
        format!(
            "provider `{provider}` is not yet supported by orcr (see `orcr integration add`, \
             planned) and its herdr integration is not installed"
        )
    } else if missing.contains(&"orcr") {
        format!(
            "provider `{provider}` has no orcr integration yet \
             (supported: claude, codex; more via `orcr integration add`, planned)"
        )
    } else {
        format!("run `herdr integration install {provider}` to install herdr's integration")
    };
    OrcrError::new(
        crate::error::ErrorCode::IntegrationMissing,
        format!("provider `{provider}` is not fully supported: missing {missing:?} integration"),
    )
    .with_details(json!({ "provider": provider, "missing": missing, "install": install }))
}

/// Whether each integration layer is present for a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderIntegration {
    pub provider: String,
    /// orcr has a built-in integration for this provider.
    pub orcr: bool,
    /// herdr's integration is installed (and current/outdated, i.e. not "not installed").
    pub herdr: bool,
}

impl ProviderIntegration {
    /// Supported iff both layers are present.
    pub fn supported(&self) -> bool {
        self.orcr && self.herdr
    }
}

/// The full per-provider integration picture, for `server status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntegrationState {
    pub providers: Vec<ProviderIntegration>,
}

impl IntegrationState {
    /// Build the state from the raw `herdr integration status` output. The union of
    /// providers seen in the herdr output and orcr's built-in set is reported.
    pub fn from_herdr_status(raw: &str) -> IntegrationState {
        let herdr_installed = parse_herdr_status(raw);

        // Union of providers herdr knows about and orcr's built-ins.
        let mut names: Vec<String> = herdr_installed.keys().cloned().collect();
        for p in ORCR_BUILTIN_PROVIDERS {
            if !names.iter().any(|n| n == p) {
                names.push(p.to_string());
            }
        }
        names.sort();

        let providers = names
            .into_iter()
            .map(|provider| {
                let orcr = ORCR_BUILTIN_PROVIDERS.contains(&provider.as_str());
                let herdr = herdr_installed.get(&provider).copied().unwrap_or(false);
                ProviderIntegration {
                    provider,
                    orcr,
                    herdr,
                }
            })
            .collect();
        IntegrationState { providers }
    }

    /// The set of fully-supported provider names.
    pub fn supported(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|p| p.supported())
            .map(|p| p.provider.clone())
            .collect()
    }

    pub fn get(&self, provider: &str) -> Option<&ProviderIntegration> {
        self.providers.iter().find(|p| p.provider == provider)
    }
}

/// Parse lines like `claude: current (v7) (/path)` / `omp: not installed (/path)` into a
/// map of provider → herdr-integration-installed. "current" and "outdated" both count as
/// installed; "not installed" counts as absent.
fn parse_herdr_status(raw: &str) -> BTreeMap<String, bool> {
    let mut map = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let rest = rest.trim();
        let installed = !rest.starts_with("not installed");
        map.insert(name.to_string(), installed);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
pi: current (v4) (/p)
omp: not installed (/p)
claude: current (v7) (/p)
codex: current (v6) (/p)
opencode: current (v8) (/p)
cursor: current (v1) (/p)
";

    #[test]
    fn parses_status_lines() {
        let m = parse_herdr_status(SAMPLE);
        assert_eq!(m.get("claude"), Some(&true));
        assert_eq!(m.get("codex"), Some(&true));
        assert_eq!(m.get("omp"), Some(&false));
        assert_eq!(m.get("pi"), Some(&true));
    }

    #[test]
    fn claude_and_codex_supported_when_both_layers_present() {
        let st = IntegrationState::from_herdr_status(SAMPLE);
        let claude = st.get("claude").unwrap();
        assert!(claude.orcr && claude.herdr && claude.supported());
        let codex = st.get("codex").unwrap();
        assert!(codex.supported());
        // pi has herdr but no orcr built-in → not supported
        let pi = st.get("pi").unwrap();
        assert!(pi.herdr && !pi.orcr && !pi.supported());
        let mut sup = st.supported();
        sup.sort();
        assert_eq!(sup, vec!["claude", "codex"]);
    }

    #[test]
    fn orcr_builtin_reported_even_if_herdr_absent() {
        // codex missing from herdr output entirely.
        let raw = "claude: current (v7) (/p)\n";
        let st = IntegrationState::from_herdr_status(raw);
        let codex = st.get("codex").unwrap();
        assert!(codex.orcr && !codex.herdr && !codex.supported());
    }

    #[test]
    fn not_installed_herdr_makes_unsupported() {
        let raw = "claude: not installed (/p)\ncodex: current (v6) (/p)\n";
        let st = IntegrationState::from_herdr_status(raw);
        assert!(!st.get("claude").unwrap().supported());
        assert!(st.get("codex").unwrap().supported());
    }

    #[test]
    fn launch_plan_maps_model_and_effort() {
        let claude = launch_plan("claude", Some("opus"), None).unwrap();
        assert_eq!(claude.argv[0], "claude");
        assert!(claude
            .argv
            .iter()
            .any(|a| a == "--dangerously-skip-permissions"));
        assert!(claude.argv.windows(2).any(|w| w == ["--model", "opus"]));

        let codex = launch_plan("codex", Some("gpt-5"), Some("high")).unwrap();
        assert!(codex
            .argv
            .iter()
            .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"));
        assert!(codex.argv.windows(2).any(|w| w == ["--model", "gpt-5"]));
        assert!(codex
            .argv
            .iter()
            .any(|a| a == "model_reasoning_effort=high"));

        // Empty model/effort → provider defaults (no flags added).
        let bare = launch_plan("claude", Some(""), Some("")).unwrap();
        assert!(!bare.argv.iter().any(|a| a == "--model"));
    }

    #[test]
    fn launch_plan_unknown_provider_is_integration_missing() {
        let e = launch_plan("pi", None, None).unwrap_err();
        assert_eq!(e.code, crate::error::ErrorCode::IntegrationMissing);
    }

    #[test]
    fn ensure_supported_enforces_both_layers() {
        let both = IntegrationState::from_herdr_status("claude: current (v7) (/p)\n");
        assert!(ensure_supported(&both, "claude").is_ok());

        // herdr layer missing → integration_missing naming herdr + install command.
        let no_herdr = IntegrationState::from_herdr_status("claude: not installed (/p)\n");
        let e = ensure_supported(&no_herdr, "claude").unwrap_err();
        assert_eq!(e.code, crate::error::ErrorCode::IntegrationMissing);
        assert_eq!(e.details["missing"], serde_json::json!(["herdr"]));
        assert!(e.details["install"]
            .as_str()
            .unwrap()
            .contains("herdr integration install claude"));
        assert_eq!(e.exit_code(), 2);

        // orcr layer missing (pi has herdr but no orcr built-in).
        let pi = IntegrationState::from_herdr_status("pi: current (v4) (/p)\n");
        let e = ensure_supported(&pi, "pi").unwrap_err();
        assert_eq!(e.details["missing"], serde_json::json!(["orcr"]));
    }
}
