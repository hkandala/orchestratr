//! Per-provider integration state (spec §11.4). A provider is *supported* only when both
//! layers are present: orcr's built-in integration **and** herdr's integration.
//!
//! herdr's integration state is read by parsing `herdr integration status` (no dedicated
//! socket method exists in protocol 16 — see the driver reference). orcr's built-in set
//! (claude + codex in the first release) is known statically.

use serde::Serialize;
use std::collections::BTreeMap;

/// Providers with an orcr built-in integration (spec §11.4: claude + codex ship first).
pub const ORCR_BUILTIN_PROVIDERS: &[&str] = &["claude", "codex"];

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
}
