//! Identity paths, scope resolution, and glob patterns (spec §5.1, §5.3).
//!
//! The single grammar block from §5.1 lives here and nowhere else — CLI validation, the
//! socket handlers, and the store matcher all derive from it:
//!
//! ```text
//! segment   = [a-z0-9_]{1,64}
//! path      = segment ("/" segment)*        # ≤ 8 segments, ≤ 256 chars; last = name
//! abs_path  = "/" path                       # anchored to the root
//! pattern   = path where any segment may be "*" or "**"  (whole segments only)
//! {rand}    = creation-only placeholder      # 5 random [a-z0-9] chars
//! ```
//!
//! Reserved level-1 names are `idle` and `unmanaged` (active-loop-name reservation lands
//! with loops in M5).

use crate::error::{OrcrError, Result};
use serde_json::json;

/// Maximum path segments (§5.1).
pub const MAX_SEGMENTS: usize = 8;
/// Maximum total path length in chars (§5.1).
pub const MAX_PATH_LEN: usize = 256;
/// Maximum length of a single segment (§5.1).
pub const MAX_SEGMENT_LEN: usize = 64;

/// Level-1 segments owned by orcr that users may never create paths under (§5.1). Active
/// loop names are additionally reserved once loops exist (M5).
pub const RESERVED_LEVEL1: &[&str] = &["idle", "unmanaged"];

/// One of `--name` or `--path` (exactly one; naming is mandatory, §5.1/§6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameOrPath {
    /// `--name <name>` — a single segment landing directly in the caller's scope.
    Name(String),
    /// `--path <path>` — relative to the caller's scope; leading `/` = absolute.
    Path(String),
}

/// True if a segment is a legal identity segment (`[a-z0-9_]{1,64}`).
pub fn valid_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= MAX_SEGMENT_LEN
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Replace every `{rand}` placeholder with 5 random `[a-z0-9]` chars (creation only, §5.1).
pub fn expand_rand(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find("{rand}") {
        out.push_str(&rest[..idx]);
        out.push_str(&random_token());
        rest = &rest[idx + "{rand}".len()..];
    }
    out.push_str(rest);
    out
}

/// A 5-char lowercase alphanumeric token (used by `{rand}` and, prefixed with `r`, by loop
/// run ids in M5).
pub fn random_token() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let raw = uuid::Uuid::new_v4();
    let bytes = raw.as_bytes();
    (0..5)
        .map(|i| ALPHABET[(bytes[i] as usize) % ALPHABET.len()] as char)
        .collect()
}

/// Resolve a creation target (`--name`/`--path`) into an **absolute** effective path,
/// applying scope resolution and `{rand}` expansion, then validating grammar + depth +
/// reserved-level-1 (spec §5.1 enforcement order).
///
/// `scope` is the caller's scope (absolute, no leading slash) or `None` at a plain shell.
pub fn resolve_create(scope: Option<&str>, input: &NameOrPath) -> Result<String> {
    let effective = match input {
        NameOrPath::Name(name) => {
            let name = expand_rand(name);
            if name.contains('/') {
                return Err(invalid(
                    format!("--name `{name}` must be a single segment (no `/`); use --path"),
                    "invalid_name",
                ));
            }
            join_scope(scope, &name)
        }
        NameOrPath::Path(path) => {
            let path = expand_rand(path);
            if let Some(abs) = path.strip_prefix('/') {
                abs.to_string()
            } else {
                join_scope(scope, &path)
            }
        }
    };
    validate_path(&effective)?;
    check_reserved_level1(&effective)?;
    Ok(effective)
}

/// Join a (possibly absent) scope with a relative path fragment.
fn join_scope(scope: Option<&str>, rel: &str) -> String {
    match scope.filter(|s| !s.is_empty()) {
        Some(s) => format!("{s}/{rel}"),
        None => rel.to_string(),
    }
}

/// Validate an absolute path's grammar, depth, and length (§5.1). A trailing/leading/empty
/// segment or bad character → `invalid_request`; too many segments → `path_too_deep`.
pub fn validate_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(invalid("path is empty", "empty_path"));
    }
    if path.len() > MAX_PATH_LEN {
        return Err(OrcrError::invalid_request(
            format!("path `{path}` exceeds {MAX_PATH_LEN} chars"),
            "path_too_long",
        )
        .with_details(json!({ "reason": "path_too_long", "path": path, "len": path.len() })));
    }
    let segments: Vec<&str> = path.split('/').collect();
    if segments.len() > MAX_SEGMENTS {
        return Err(OrcrError::invalid_request(
            format!(
                "path `{path}` has {} segments, exceeding the limit of {MAX_SEGMENTS}",
                segments.len()
            ),
            "path_too_deep",
        )
        .with_details(json!({
            "reason": "path_too_deep", "path": path, "segments": segments.len(),
        })));
    }
    for seg in &segments {
        if !valid_segment(seg) {
            return Err(invalid(
                format!(
                    "path `{path}` has an invalid segment `{seg}` \
                     (segments are [a-z0-9_], 1–{MAX_SEGMENT_LEN} chars)"
                ),
                "invalid_segment",
            ));
        }
    }
    Ok(())
}

/// Reject creation under a reserved level-1 segment (§5.1). Reserved only at level 1.
pub fn check_reserved_level1(path: &str) -> Result<()> {
    let first = path.split('/').next().unwrap_or("");
    if RESERVED_LEVEL1.contains(&first) {
        return Err(OrcrError::invalid_request(
            format!("`{first}` is a reserved level-1 name owned by orcr"),
            "reserved_name",
        )
        .with_details(json!({ "reason": "reserved_name", "name": first })));
    }
    Ok(())
}

/// The agent's name = the last path segment (§5.1).
pub fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// An agent's scope = its path minus its name (§5.3). `None` for a single-segment path.
pub fn scope_of_agent(path: &str) -> Option<String> {
    path.rfind('/').map(|i| path[..i].to_string())
}

/// The home workspace = the first segment when the path has ≥ 2 segments, else `default`
/// (§5.2, "derived, never stored").
pub fn home_workspace(path: &str) -> String {
    let mut it = path.splitn(2, '/');
    let first = it.next().unwrap_or("");
    match it.next() {
        Some(_) => first.to_string(),
        None => "default".to_string(),
    }
}

/// The herdr agent `name` (and pane `label`) for a path. herdr 0.7.2 enforces that an
/// agent's `name` is **unique across the whole session**, not scoped to its workspace, so the
/// §5.2 tab label (the path after the first segment) is *not* usable as the herdr name: two
/// agents in different top-level scopes (e.g. `review_a/fanout/file_0` and
/// `review_b/fanout/file_0`) would collide with `agent_name_taken`. orcr already guarantees
/// full paths are unique among active agents, so the **full effective path** is the correct
/// session-unique herdr name. (See m7-sdk-skill/notes.md — this is why the visible tab shows
/// the full path rather than the path-after-first-segment that §5.2 sketches.)
pub fn herdr_name(path: &str) -> String {
    path.to_string()
}

/// True if `s` (a resolved path) contains a wildcard segment (`*` or `**`).
pub fn is_pattern(s: &str) -> bool {
    s.split('/').any(|seg| seg == "*" || seg == "**")
}

/// Resolve a selector (path or pattern) against the caller's scope into an absolute form
/// (leading `/` = absolute), validating pattern grammar. Does **not** enforce the depth
/// limit (patterns may legitimately be short) but every literal segment must be valid.
pub fn resolve_selector(scope: Option<&str>, raw: &str) -> Result<String> {
    let effective = if let Some(abs) = raw.strip_prefix('/') {
        abs.to_string()
    } else {
        join_scope(scope, raw)
    };
    if effective.is_empty() {
        return Err(invalid("selector is empty", "empty_selector"));
    }
    for seg in effective.split('/') {
        if seg == "*" || seg == "**" {
            continue;
        }
        if !valid_segment(seg) {
            return Err(invalid(
                format!("selector `{effective}` has an invalid segment `{seg}`"),
                "invalid_segment",
            ));
        }
    }
    Ok(effective)
}

/// A compiled glob pattern (§5.1): whole-segment `*` (one level) / `**` (any depth ≥ 1),
/// matched **anchored** against a full path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    segments: Vec<Seg>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Seg {
    Literal(String),
    Star,
    DoubleStar,
}

impl Pattern {
    /// Compile an (already scope-resolved, absolute) pattern string.
    pub fn compile(pattern: &str) -> Result<Pattern> {
        if pattern.is_empty() {
            return Err(invalid("empty pattern", "empty_pattern"));
        }
        let mut segments = Vec::new();
        for seg in pattern.split('/') {
            let s = match seg {
                "*" => Seg::Star,
                "**" => Seg::DoubleStar,
                lit if valid_segment(lit) => Seg::Literal(lit.to_string()),
                bad => {
                    return Err(invalid(
                        format!("pattern segment `{bad}` is invalid (whole-segment `*`/`**` only)"),
                        "invalid_pattern",
                    ))
                }
            };
            segments.push(s);
        }
        Ok(Pattern { segments })
    }

    /// True if this pattern contains a wildcard segment.
    pub fn has_wildcard(&self) -> bool {
        self.segments
            .iter()
            .any(|s| matches!(s, Seg::Star | Seg::DoubleStar))
    }

    /// Match `path` anchored against the whole pattern.
    pub fn matches(&self, path: &str) -> bool {
        let segs: Vec<&str> = path.split('/').collect();
        self.match_from(0, &segs, 0)
    }

    fn match_from(&self, pi: usize, path: &[&str], si: usize) -> bool {
        if pi == self.segments.len() {
            return si == path.len();
        }
        match &self.segments[pi] {
            Seg::DoubleStar => {
                // `**` consumes one or more segments (never zero — so `a/**` never matches
                // `a` itself, per §5.1).
                let remaining = path.len().saturating_sub(si);
                (1..=remaining).any(|t| self.match_from(pi + 1, path, si + t))
            }
            Seg::Star => si < path.len() && self.match_from(pi + 1, path, si + 1),
            Seg::Literal(lit) => {
                si < path.len() && path[si] == lit && self.match_from(pi + 1, path, si + 1)
            }
        }
    }
}

fn invalid(msg: impl Into<String>, reason: &str) -> OrcrError {
    OrcrError::invalid_request(msg, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_grammar() {
        assert!(valid_segment("file_1"));
        assert!(valid_segment("a"));
        assert!(valid_segment(&"a".repeat(64)));
        assert!(!valid_segment(""));
        assert!(!valid_segment(&"a".repeat(65)));
        assert!(!valid_segment("File")); // uppercase
        assert!(!valid_segment("a-b")); // dash
        assert!(!valid_segment("a/b")); // slash
    }

    #[test]
    fn name_lands_in_scope() {
        let p = resolve_create(Some("review"), &NameOrPath::Name("worker".into())).unwrap();
        assert_eq!(p, "review/worker");
        let p = resolve_create(None, &NameOrPath::Name("worker".into())).unwrap();
        assert_eq!(p, "worker");
    }

    #[test]
    fn relative_path_resolves_against_scope() {
        let p = resolve_create(Some("review"), &NameOrPath::Path("fanout/file_1".into())).unwrap();
        assert_eq!(p, "review/fanout/file_1");
    }

    #[test]
    fn absolute_path_ignores_scope() {
        let p = resolve_create(Some("review"), &NameOrPath::Path("/verify/file_1".into())).unwrap();
        assert_eq!(p, "verify/file_1");
    }

    #[test]
    fn name_with_slash_rejected() {
        let e = resolve_create(None, &NameOrPath::Name("a/b".into())).unwrap_err();
        assert_eq!(e.details["reason"], "invalid_name");
    }

    #[test]
    fn depth_limit_enforced() {
        let deep = (0..9)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let e = validate_path(&deep).unwrap_err();
        assert_eq!(e.details["reason"], "path_too_deep");
        assert_eq!(e.details["segments"], 9);
    }

    #[test]
    fn reserved_level1_rejected_only_at_level1() {
        assert!(resolve_create(None, &NameOrPath::Path("idle/x".into())).is_err());
        assert!(resolve_create(None, &NameOrPath::Path("unmanaged/x".into())).is_err());
        // reserved word deeper is fine
        assert!(resolve_create(Some("review"), &NameOrPath::Name("idle".into())).is_ok());
    }

    #[test]
    fn rand_expands_to_five_chars() {
        let expanded = expand_rand("review_{rand}/file_1");
        assert!(expanded.starts_with("review_"));
        assert!(expanded.ends_with("/file_1"));
        let mid = &expanded["review_".len()..expanded.len() - "/file_1".len()];
        assert_eq!(mid.len(), 5);
        assert!(mid
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        // two expansions differ (overwhelmingly likely)
        assert_ne!(expand_rand("{rand}"), expand_rand("{rand}"));
    }

    #[test]
    fn derived_helpers() {
        assert_eq!(name_of("review/fanout/file_1"), "file_1");
        assert_eq!(name_of("worker"), "worker");
        assert_eq!(
            scope_of_agent("review/fanout/file_1").as_deref(),
            Some("review/fanout")
        );
        assert_eq!(scope_of_agent("worker"), None);
        assert_eq!(home_workspace("refactor/phase_1/review/worker"), "refactor");
        assert_eq!(home_workspace("worker"), "default");
        // The herdr agent name must be the full, session-unique path (herdr 0.7.2 enforces
        // session-global name uniqueness — see `herdr_name`).
        assert_eq!(
            herdr_name("refactor/phase_1/review/worker"),
            "refactor/phase_1/review/worker"
        );
        assert_eq!(herdr_name("worker"), "worker");
    }

    #[test]
    fn star_matches_one_level() {
        let p = Pattern::compile("review/*").unwrap();
        assert!(p.matches("review/worker"));
        assert!(!p.matches("review/fanout/worker")); // too deep
        assert!(!p.matches("review")); // needs a child
        assert!(!p.matches("reviewer/x")); // anchored — no prefix match
    }

    #[test]
    fn doublestar_matches_any_depth_but_not_self() {
        let p = Pattern::compile("review/**").unwrap();
        assert!(p.matches("review/worker"));
        assert!(p.matches("review/fanout/file_1"));
        assert!(!p.matches("review")); // never the node itself
        assert!(!p.matches("reviewer/x"));
    }

    #[test]
    fn doublestar_between() {
        let p = Pattern::compile("a/**/b").unwrap();
        assert!(p.matches("a/x/b"));
        assert!(p.matches("a/x/y/b"));
        assert!(!p.matches("a/b")); // ** needs ≥1 between
        assert!(!p.matches("a/x/c"));
    }

    #[test]
    fn bare_doublestar_is_everything() {
        let p = Pattern::compile("**").unwrap();
        assert!(p.matches("a"));
        assert!(p.matches("a/b/c"));
    }

    #[test]
    fn mid_star() {
        let p = Pattern::compile("a/*/b").unwrap();
        assert!(p.matches("a/x/b"));
        assert!(!p.matches("a/x/y/b"));
    }

    #[test]
    fn underscore_is_literal_not_wildcard() {
        // `_` is a legal name char and must NOT behave like a SQL LIKE wildcard.
        let p = Pattern::compile("review/file_1").unwrap();
        assert!(p.matches("review/file_1"));
        assert!(!p.matches("review/fileX1"));
        assert!(!p.has_wildcard());
    }

    #[test]
    fn is_pattern_detects_wildcards() {
        assert!(is_pattern("review/*"));
        assert!(is_pattern("a/**/b"));
        assert!(!is_pattern("review/worker"));
    }

    #[test]
    fn selector_resolution() {
        assert_eq!(resolve_selector(Some("review"), "*").unwrap(), "review/*");
        assert_eq!(
            resolve_selector(Some("review"), "/verify/**").unwrap(),
            "verify/**"
        );
        assert_eq!(
            resolve_selector(None, "review/worker").unwrap(),
            "review/worker"
        );
    }
}
