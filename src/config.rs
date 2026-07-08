use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(skip)]
    pub store_root: PathBuf,
    pub defaults: DefaultsConfig,
    pub limits: LimitsConfig,
    pub herdr: HerdrConfig,
    pub viewer: ViewerConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub agent: String,
    pub model: String,
    pub effort: String,
    pub timeout_s: u64,
    pub keep: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub max_depth: u32,
    pub max_agents_per_tree: u32,
    pub max_concurrent: u32,
    pub idle_reap_min: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HerdrConfig {
    pub bin: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    pub auto: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            store_root: PathBuf::new(),
            defaults: DefaultsConfig::default(),
            limits: LimitsConfig::default(),
            herdr: HerdrConfig::default(),
            viewer: ViewerConfig::default(),
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            agent: "claude".to_string(),
            model: String::new(),
            effort: String::new(),
            timeout_s: 600,
            keep: false,
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_agents_per_tree: 10,
            max_concurrent: 4,
            idle_reap_min: 15,
        }
    }
}

impl Default for HerdrConfig {
    fn default() -> Self {
        Self {
            bin: String::new(),
            session: "orcr".to_string(),
        }
    }
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self { auto: true }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let store_root = resolve_store_root()?;
        Self::load_from(&store_root)
    }

    pub fn load_from(store_root: &Path) -> Result<Self> {
        fs::create_dir_all(store_root)
            .with_context(|| format!("failed to create store root {}", store_root.display()))?;

        let config_path = store_root.join("config.toml");
        let mut config = if config_path.exists() {
            let toml = fs::read_to_string(&config_path)
                .with_context(|| format!("failed to read config file {}", config_path.display()))?;
            toml::from_str::<Self>(&toml)
                .with_context(|| format!("failed to parse config file {}", config_path.display()))?
        } else {
            Self::default()
        };
        config.store_root = store_root.to_path_buf();
        Ok(config)
    }
}

fn resolve_store_root() -> Result<PathBuf> {
    match env::var_os("ORCR_STORE") {
        Some(value) => Ok(PathBuf::from(value)),
        None => dirs::home_dir()
            .map(|home| home.join(".orcr"))
            .context("could not resolve home directory for default store root"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn defaults_when_file_missing() {
        let temp = tempdir().unwrap();
        let config = Config::load_from(temp.path()).unwrap();

        assert_eq!(config.store_root, temp.path());
        assert_eq!(config.defaults, DefaultsConfig::default());
        assert_eq!(config.limits, LimitsConfig::default());
        assert_eq!(config.herdr, HerdrConfig::default());
        assert_eq!(config.viewer, ViewerConfig::default());
    }

    #[test]
    fn defaults_for_missing_keys() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("config.toml"),
            r#"
[defaults]
agent = "codex"

[viewer]
auto = false
"#,
        )
        .unwrap();

        let config = Config::load_from(temp.path()).unwrap();
        assert_eq!(config.defaults.agent, "codex");
        assert_eq!(config.defaults.timeout_s, 600);
        assert_eq!(config.limits.max_depth, 3);
        assert!(!config.viewer.auto);
    }

    #[test]
    fn explicit_values_parse() {
        let temp = tempdir().unwrap();
        fs::write(
            temp.path().join("config.toml"),
            r#"
[defaults]
agent = "mock"
model = "m"
effort = "high"
timeout_s = 12
keep = true

[limits]
max_depth = 7
max_agents_per_tree = 8
max_concurrent = 9
idle_reap_min = 10

[herdr]
bin = "/bin/herdr"
session = "custom"

[viewer]
auto = false
"#,
        )
        .unwrap();

        let config = Config::load_from(temp.path()).unwrap();
        assert_eq!(config.defaults.agent, "mock");
        assert_eq!(config.defaults.model, "m");
        assert_eq!(config.defaults.effort, "high");
        assert_eq!(config.defaults.timeout_s, 12);
        assert!(config.defaults.keep);
        assert_eq!(config.limits.max_depth, 7);
        assert_eq!(config.limits.max_agents_per_tree, 8);
        assert_eq!(config.limits.max_concurrent, 9);
        assert_eq!(config.limits.idle_reap_min, 10);
        assert_eq!(config.herdr.bin, "/bin/herdr");
        assert_eq!(config.herdr.session, "custom");
        assert!(!config.viewer.auto);
    }

    #[test]
    fn malformed_toml_is_error() {
        let temp = tempdir().unwrap();
        fs::write(temp.path().join("config.toml"), "[defaults\nagent =").unwrap();

        let error = Config::load_from(temp.path()).unwrap_err();
        assert!(error.to_string().contains("failed to parse config file"));
    }

    #[test]
    fn load_from_creates_root() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("nested");
        let config = Config::load_from(&root).unwrap();

        assert!(root.is_dir());
        assert_eq!(config.store_root, root);
    }
}
