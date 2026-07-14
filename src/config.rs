//! Configuration (spec §14): `~/.orcr/config.json`, strict JSON, every key optional
//! with a built-in default.
//!
//! Validation happens at load: unknown keys **warn and are ignored** (suggesting the
//! nearest valid name), while known keys are validated strictly — durations require
//! units and must be positive, `concurrency.max >= 1`, per-provider caps are clamped to
//! `max` (with a warning), and `herdr.session` must be a valid session name.

use crate::duration::parse_duration;
use crate::error::OrcrError;
use crate::home::Home;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Fully-resolved configuration (defaults merged with any `config.json` values).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub defaults: Defaults,
    pub herdr: HerdrConfig,
    pub concurrency: ConcurrencyConfig,
    pub timings: Timings,
    pub logs: LogsConfig,
    /// Optional per-provider completion-tuning overrides (`integrations.<provider>.*`,
    /// spec §14 / M3). Defaults ship inside each integration; this lets a user (or tests)
    /// override the named ms knobs. Empty = all integration defaults.
    pub integrations: BTreeMap<String, IntegrationTuning>,
}

/// Per-provider completion-tuning overrides (all optional; `None` = use the integration
/// default). Values are milliseconds (spec §5.6 named parameters).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationTuning {
    pub fast_turn_grace_ms: Option<u64>,
    pub idle_stable_ms: Option<u64>,
    pub transcript_settle_ms: Option<u64>,
    pub transcript_freshness_timeout_ms: Option<u64>,
    pub shutdown_grace_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    pub agent: String,
    /// Empty string = provider default.
    pub model: String,
    /// Empty string = provider default.
    pub effort: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HerdrConfig {
    /// Empty = discover via `$ORCR_HERDR_BIN` → `$PATH`.
    pub bin: String,
    /// The owned session name.
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrencyConfig {
    /// Global ceiling (RAM protection); `>= 1`.
    pub max: u32,
    /// Per-provider caps beneath `max`; any provider name may be a key.
    pub per_provider: BTreeMap<String, u32>,
}

impl ConcurrencyConfig {
    /// The effective cap for a provider: its per-provider cap if set, else `max`.
    pub fn cap_for(&self, provider: &str) -> u32 {
        self.per_provider.get(provider).copied().unwrap_or(self.max)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timings {
    pub idle_after: Duration,
    pub kill_after: Duration,
    pub gc_tick: Duration,
    pub max_starting: Duration,
    pub attach_lease_ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsConfig {
    pub max_bytes: u64,
    pub max_files: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            defaults: Defaults {
                agent: "claude".to_string(),
                model: String::new(),
                effort: String::new(),
            },
            herdr: HerdrConfig {
                bin: String::new(),
                session: "orcr".to_string(),
            },
            concurrency: ConcurrencyConfig {
                max: 25,
                per_provider: BTreeMap::from([("claude".to_string(), 10)]),
            },
            timings: Timings {
                idle_after: Duration::from_secs(300),
                kill_after: Duration::from_secs(600),
                gc_tick: Duration::from_secs(30),
                max_starting: Duration::from_secs(120),
                attach_lease_ttl: Duration::from_secs(30),
            },
            logs: LogsConfig {
                max_bytes: 10_485_760,
                max_files: 5,
            },
            integrations: BTreeMap::new(),
        }
    }
}

/// A loaded config plus any non-fatal warnings (unknown keys, clamped caps).
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: Config,
    pub warnings: Vec<String>,
}

impl Config {
    /// Load config from a home's `config.json`. A missing file yields all defaults with
    /// no warnings. A present file is parsed as strict JSON and validated.
    pub fn load(home: &Home) -> Result<LoadedConfig, OrcrError> {
        let path = home.config_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedConfig {
                    config: Config::default(),
                    warnings: Vec::new(),
                });
            }
            Err(e) => {
                return Err(OrcrError::environment(
                    "config_invalid",
                    format!("cannot read {}: {e}", path.display()),
                ));
            }
        };
        Config::parse(&text)
    }

    /// Parse + validate config from a JSON string. Used by [`Config::load`] and tests.
    pub fn parse(text: &str) -> Result<LoadedConfig, OrcrError> {
        let root: Value = serde_json::from_str(text)
            .map_err(|e| config_invalid(format!("config.json is not valid JSON: {e}")))?;
        let obj = root
            .as_object()
            .ok_or_else(|| config_invalid("config.json must be a JSON object"))?;

        let mut cfg = Config::default();
        let mut warnings = Vec::new();

        const TOP_KEYS: &[&str] = &[
            "defaults",
            "herdr",
            "concurrency",
            "timings",
            "logs",
            "integrations",
        ];
        for key in obj.keys() {
            if !TOP_KEYS.contains(&key.as_str()) {
                warnings.push(unknown_key_warning(key, "", TOP_KEYS));
            }
        }

        if let Some(v) = obj.get("defaults") {
            parse_defaults(v, &mut cfg.defaults, &mut warnings)?;
        }
        if let Some(v) = obj.get("herdr") {
            parse_herdr(v, &mut cfg.herdr, &mut warnings)?;
        }
        if let Some(v) = obj.get("concurrency") {
            parse_concurrency(v, &mut cfg.concurrency, &mut warnings)?;
        }
        if let Some(v) = obj.get("timings") {
            parse_timings(v, &mut cfg.timings, &mut warnings)?;
        }
        if let Some(v) = obj.get("logs") {
            parse_logs(v, &mut cfg.logs, &mut warnings)?;
        }
        if let Some(v) = obj.get("integrations") {
            parse_integrations(v, &mut cfg.integrations, &mut warnings)?;
        }

        Ok(LoadedConfig {
            config: cfg,
            warnings,
        })
    }
}

fn parse_defaults(
    v: &Value,
    out: &mut Defaults,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "defaults")?;
    const KEYS: &[&str] = &["agent", "model", "effort"];
    warn_unknown(obj, "defaults", KEYS, warnings);
    if let Some(s) = obj.get("agent") {
        out.agent = as_string(s, "defaults.agent")?;
    }
    if let Some(s) = obj.get("model") {
        out.model = as_string(s, "defaults.model")?;
    }
    if let Some(s) = obj.get("effort") {
        out.effort = as_string(s, "defaults.effort")?;
    }
    Ok(())
}

fn parse_herdr(
    v: &Value,
    out: &mut HerdrConfig,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "herdr")?;
    const KEYS: &[&str] = &["bin", "session"];
    warn_unknown(obj, "herdr", KEYS, warnings);
    if let Some(s) = obj.get("bin") {
        out.bin = as_string(s, "herdr.bin")?;
    }
    if let Some(s) = obj.get("session") {
        let session = as_string(s, "herdr.session")?;
        if !is_valid_session_name(&session) {
            return Err(config_invalid(format!(
                "herdr.session `{session}` is not a valid session name \
                 (use letters, digits, `_`, `-`, `.`)"
            )));
        }
        out.session = session;
    }
    Ok(())
}

fn parse_concurrency(
    v: &Value,
    out: &mut ConcurrencyConfig,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "concurrency")?;
    // In `concurrency`, `max` is the one reserved key; everything else is a provider cap,
    // so there is no "unknown key" concept here.
    if let Some(m) = obj.get("max") {
        out.max = as_u32(m, "concurrency.max")?;
    }
    if out.max < 1 {
        return Err(config_invalid("concurrency.max must be >= 1"));
    }
    // Rebuild per-provider from the file so an explicit config replaces the default seed.
    if obj.keys().any(|k| k != "max") {
        out.per_provider.clear();
    }
    for (k, pv) in obj {
        if k == "max" {
            continue;
        }
        let mut cap = as_u32(pv, &format!("concurrency.{k}"))?;
        if cap < 1 {
            return Err(config_invalid(format!("concurrency.{k} must be >= 1")));
        }
        if cap > out.max {
            warnings.push(format!(
                "concurrency.{k} ({cap}) exceeds concurrency.max ({}); clamping to {}",
                out.max, out.max
            ));
            cap = out.max;
        }
        out.per_provider.insert(k.clone(), cap);
    }
    Ok(())
}

fn parse_timings(
    v: &Value,
    out: &mut Timings,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "timings")?;
    const KEYS: &[&str] = &[
        "idle_after",
        "kill_after",
        "gc_tick",
        "max_starting",
        "attach_lease_ttl",
    ];
    warn_unknown(obj, "timings", KEYS, warnings);
    for (key, slot) in [
        ("idle_after", &mut out.idle_after),
        ("kill_after", &mut out.kill_after),
        ("gc_tick", &mut out.gc_tick),
        ("max_starting", &mut out.max_starting),
        ("attach_lease_ttl", &mut out.attach_lease_ttl),
    ] {
        if let Some(dv) = obj.get(key) {
            let s = as_string(dv, &format!("timings.{key}"))?;
            *slot = parse_duration(&s)
                .map_err(|e| config_invalid(format!("timings.{key}: {}", e.message)))?;
        }
    }
    Ok(())
}

fn parse_logs(
    v: &Value,
    out: &mut LogsConfig,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "logs")?;
    const KEYS: &[&str] = &["max_bytes", "max_files"];
    warn_unknown(obj, "logs", KEYS, warnings);
    if let Some(m) = obj.get("max_bytes") {
        out.max_bytes = as_u64(m, "logs.max_bytes")?;
        if out.max_bytes < 1 {
            return Err(config_invalid("logs.max_bytes must be >= 1"));
        }
    }
    if let Some(m) = obj.get("max_files") {
        out.max_files = as_u32(m, "logs.max_files")?;
        if out.max_files < 1 {
            return Err(config_invalid("logs.max_files must be >= 1"));
        }
    }
    Ok(())
}

fn parse_integrations(
    v: &Value,
    out: &mut BTreeMap<String, IntegrationTuning>,
    warnings: &mut Vec<String>,
) -> Result<(), OrcrError> {
    let obj = section_obj(v, "integrations")?;
    const KEYS: &[&str] = &[
        "fast_turn_grace_ms",
        "idle_stable_ms",
        "transcript_settle_ms",
        "transcript_freshness_timeout_ms",
        "shutdown_grace_ms",
    ];
    for (provider, pv) in obj {
        let pobj = section_obj(pv, &format!("integrations.{provider}"))?;
        warn_unknown(pobj, &format!("integrations.{provider}"), KEYS, warnings);
        let ms = |key: &str| -> Result<Option<u64>, OrcrError> {
            match pobj.get(key) {
                Some(x) => Ok(Some(as_u64(x, &format!("integrations.{provider}.{key}"))?)),
                None => Ok(None),
            }
        };
        out.insert(
            provider.clone(),
            IntegrationTuning {
                fast_turn_grace_ms: ms("fast_turn_grace_ms")?,
                idle_stable_ms: ms("idle_stable_ms")?,
                transcript_settle_ms: ms("transcript_settle_ms")?,
                transcript_freshness_timeout_ms: ms("transcript_freshness_timeout_ms")?,
                shutdown_grace_ms: ms("shutdown_grace_ms")?,
            },
        );
    }
    Ok(())
}

// --- helpers ---

fn section_obj<'a>(
    v: &'a Value,
    name: &str,
) -> Result<&'a serde_json::Map<String, Value>, OrcrError> {
    v.as_object()
        .ok_or_else(|| config_invalid(format!("`{name}` must be a JSON object")))
}

fn as_string(v: &Value, field: &str) -> Result<String, OrcrError> {
    v.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| config_invalid(format!("`{field}` must be a string")))
}

fn as_u32(v: &Value, field: &str) -> Result<u32, OrcrError> {
    let n = v
        .as_u64()
        .ok_or_else(|| config_invalid(format!("`{field}` must be a non-negative integer")))?;
    u32::try_from(n).map_err(|_| config_invalid(format!("`{field}` is too large")))
}

fn as_u64(v: &Value, field: &str) -> Result<u64, OrcrError> {
    v.as_u64()
        .ok_or_else(|| config_invalid(format!("`{field}` must be a non-negative integer")))
}

fn warn_unknown(
    obj: &serde_json::Map<String, Value>,
    section: &str,
    known: &[&str],
    warnings: &mut Vec<String>,
) {
    for key in obj.keys() {
        if !known.contains(&key.as_str()) {
            warnings.push(unknown_key_warning(key, section, known));
        }
    }
}

fn unknown_key_warning(key: &str, section: &str, known: &[&str]) -> String {
    let qualified = if section.is_empty() {
        key.to_string()
    } else {
        format!("{section}.{key}")
    };
    match nearest_key(key, known) {
        Some(sugg) => {
            format!("unknown config key `{qualified}` (did you mean `{sugg}`?); ignoring")
        }
        None => format!("unknown config key `{qualified}`; ignoring"),
    }
}

/// The nearest known key within Levenshtein distance 2, if any.
fn nearest_key(key: &str, known: &[&str]) -> Option<String> {
    known
        .iter()
        .map(|k| (*k, levenshtein(key, k)))
        .filter(|(_, d)| *d <= 2)
        .min_by_key(|(_, d)| *d)
        .map(|(k, _)| k.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn is_valid_session_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn config_invalid(msg: impl Into<String>) -> OrcrError {
    OrcrError::environment("config_invalid", msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let lc = Config::parse("{}").unwrap();
        assert_eq!(lc.config, Config::default());
        assert!(lc.warnings.is_empty());
        assert_eq!(lc.config.concurrency.max, 25);
        assert_eq!(lc.config.concurrency.cap_for("claude"), 10);
        assert_eq!(lc.config.concurrency.cap_for("codex"), 25);
        assert_eq!(lc.config.timings.idle_after, Duration::from_secs(300));
    }

    #[test]
    fn overrides_merge() {
        let lc = Config::parse(
            r#"{"defaults":{"agent":"codex"},"herdr":{"session":"orcr_x"},
                "timings":{"idle_after":"10m"}}"#,
        )
        .unwrap();
        assert_eq!(lc.config.defaults.agent, "codex");
        assert_eq!(lc.config.herdr.session, "orcr_x");
        assert_eq!(lc.config.timings.idle_after, Duration::from_secs(600));
        // untouched keys keep defaults
        assert_eq!(lc.config.timings.kill_after, Duration::from_secs(600));
    }

    #[test]
    fn unknown_top_key_warns_with_suggestion() {
        let lc = Config::parse(r#"{"timeings":{}}"#).unwrap();
        assert_eq!(lc.warnings.len(), 1);
        assert!(lc.warnings[0].contains("timeings"));
        assert!(lc.warnings[0].contains("did you mean `timings`"));
    }

    #[test]
    fn unknown_section_key_warns() {
        let lc = Config::parse(r#"{"defaults":{"agnet":"x"}}"#).unwrap();
        assert_eq!(lc.warnings.len(), 1);
        assert!(lc.warnings[0].contains("defaults.agnet"));
        assert!(lc.warnings[0].contains("agent"));
    }

    #[test]
    fn bad_duration_is_fatal() {
        let e = Config::parse(r#"{"timings":{"idle_after":"5"}}"#).unwrap_err();
        assert_eq!(e.details["cause"], "config_invalid");
        assert!(e.message.contains("idle_after"));
    }

    #[test]
    fn zero_duration_is_fatal() {
        assert!(Config::parse(r#"{"timings":{"gc_tick":"0s"}}"#).is_err());
    }

    #[test]
    fn concurrency_max_below_one_is_fatal() {
        let e = Config::parse(r#"{"concurrency":{"max":0}}"#).unwrap_err();
        assert!(e.message.contains("concurrency.max"));
    }

    #[test]
    fn per_provider_cap_clamped_with_warning() {
        let lc = Config::parse(r#"{"concurrency":{"max":10,"claude":50}}"#).unwrap();
        assert_eq!(lc.config.concurrency.cap_for("claude"), 10);
        assert_eq!(lc.warnings.len(), 1);
        assert!(lc.warnings[0].contains("clamping"));
    }

    #[test]
    fn explicit_concurrency_replaces_default_seed() {
        let lc = Config::parse(r#"{"concurrency":{"max":30,"codex":5}}"#).unwrap();
        // claude default seed is dropped once the file specifies providers
        assert_eq!(lc.config.concurrency.cap_for("codex"), 5);
        assert_eq!(lc.config.concurrency.cap_for("claude"), 30); // falls back to max
    }

    #[test]
    fn invalid_session_name_is_fatal() {
        let e = Config::parse(r#"{"herdr":{"session":"bad/name"}}"#).unwrap_err();
        assert!(e.message.contains("session"));
    }

    #[test]
    fn wrong_type_is_fatal() {
        assert!(Config::parse(r#"{"concurrency":{"max":"lots"}}"#).is_err());
        assert!(Config::parse(r#"{"defaults":{"agent":5}}"#).is_err());
        assert!(Config::parse(r#"{"defaults":"nope"}"#).is_err());
    }

    #[test]
    fn not_json_is_fatal() {
        assert!(Config::parse("{not json}").is_err());
        assert!(Config::parse("[]").is_err());
    }

    #[test]
    fn levenshtein_basic() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("agnet", "agent"), 2);
        assert_eq!(levenshtein("same", "same"), 0);
    }
}
