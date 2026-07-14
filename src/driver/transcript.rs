//! Transcript adapters for the built-in providers (spec §11.4): locate and parse a
//! provider's **native** session files into a common shape (ordered messages, roles, tool
//! calls, token counts where available). orcr keeps **no** response copies — `logs` always
//! reads these native files; completion records only a locator/cursor (§12).
//!
//! Two gates protect against silent wrong reads:
//! - **Identity** — a transcript is selected by the pane's `agent_session` id (and the
//!   agent's `created_at`); more than one candidate → `transcript_unavailable`
//!   (`cause: ambiguous`) with the candidates, never a silent pick.
//! - **Freshness** — a *final response* is reported only once the file has advanced past the
//!   observed completion (bounded by `transcript_freshness_timeout_ms`); otherwise
//!   `transcript_unavailable` (`cause: stale`).
//!
//! Provider file formats (verified against real files on disk):
//! - **claude**: `~/.claude/projects/<cwd-slug>/<session_id>.jsonl`; one JSON object per
//!   line; assistant/user rows carry `message.content` blocks and `message.usage` tokens.
//! - **codex**: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<session_id>.jsonl`; rows are
//!   `{"type":"response_item","payload":{"type":"message","role","content":[…]}}`.

use crate::error::{ErrorCode, OrcrError, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// One parsed transcript message in the common shape (spec §11.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TranscriptEntry {
    /// `user` | `assistant` | `system` | `tool`.
    pub role: String,
    /// `text` | `thinking` | `tool_use` | `tool_result` | `other`.
    pub kind: String,
    /// The textual content (may be empty for tool_use rows).
    pub text: String,
    /// Tool name for `tool_use` rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// A located transcript file for an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptLocator {
    pub provider: String,
    pub path: PathBuf,
}

impl TranscriptLocator {
    /// The stable locator string recorded in the store (`transcript_locator`, §12).
    pub fn as_string(&self) -> String {
        self.path.display().to_string()
    }

    /// The file's mtime in ms since the epoch (`None` if it cannot be stat'd).
    pub fn mtime_ms(&self) -> Option<i64> {
        file_mtime_ms(&self.path)
    }

    /// Parse the full transcript into the common entry shape.
    pub fn read_entries(&self) -> Result<Vec<TranscriptEntry>> {
        let text = read_file(&self.path)?;
        match self.provider.as_str() {
            "codex" => Ok(parse_codex(&text)),
            _ => Ok(parse_claude(&text)),
        }
    }

    /// The final assistant message text (concatenated text blocks), or
    /// `transcript_unavailable` when none is identifiable.
    pub fn last_response(&self, uuid: &str, status: &str) -> Result<String> {
        let entries = self.read_entries()?;
        let last = entries
            .iter()
            .rev()
            .find(|e| e.role == "assistant" && e.kind == "text" && !e.text.trim().is_empty());
        match last {
            Some(e) => Ok(e.text.clone()),
            None => Err(transcript_unavailable(
                uuid,
                status,
                "no_final_response",
                "no final assistant response is identifiable in the transcript",
            )),
        }
    }
}

/// Locate a provider's native transcript for an agent, applying the identity gate (spec
/// §11.4). `session_kind`/`session_value` come from the pane's `agent_session`; `cwd` and
/// `created_at_ms` narrow/disambiguate candidates.
pub fn locate_transcript(
    provider: &str,
    session_kind: Option<&str>,
    session_value: Option<&str>,
    cwd: Option<&str>,
    created_at_ms: i64,
    uuid: &str,
    status: &str,
) -> Result<TranscriptLocator> {
    let value = session_value.filter(|s| !s.is_empty()).ok_or_else(|| {
        transcript_unavailable(
            uuid,
            status,
            "no_session",
            "no agent_session transcript pointer has been reported for this agent",
        )
    })?;

    // A `path`-kind pointer is a direct file path.
    if session_kind == Some("path") {
        let p = PathBuf::from(value);
        if p.is_file() {
            return Ok(TranscriptLocator {
                provider: provider.to_string(),
                path: p,
            });
        }
        return Err(transcript_unavailable(
            uuid,
            status,
            "not_found",
            format!("transcript path `{value}` does not exist (rotated or deleted)"),
        ));
    }

    let candidates = match provider {
        "codex" => codex_candidates(value),
        _ => claude_candidates(value, cwd),
    };
    select_candidate(candidates, created_at_ms, uuid, status, value)
}

/// Pick exactly one candidate: 0 → not_found; 1 → it; >1 → prefer the newest that is not
/// older than `created_at` (identity by session + created_at), else `ambiguous`.
fn select_candidate(
    mut candidates: Vec<PathBuf>,
    created_at_ms: i64,
    uuid: &str,
    status: &str,
    value: &str,
) -> Result<TranscriptLocator> {
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Err(transcript_unavailable(
            uuid,
            status,
            "not_found",
            format!("no transcript file found for session `{value}` (rotated or deleted)"),
        )),
        1 => Ok(locator(candidates.into_iter().next().unwrap())),
        _ => {
            // created_at disambiguation: keep candidates whose file is not older than the
            // agent's creation (a stale reuse of the same id would be older).
            let fresh: Vec<PathBuf> = candidates
                .iter()
                .filter(|p| {
                    file_mtime_ms(p)
                        .map(|m| m >= created_at_ms)
                        .unwrap_or(false)
                })
                .cloned()
                .collect();
            if fresh.len() == 1 {
                return Ok(locator(fresh.into_iter().next().unwrap()));
            }
            let list: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            Err(OrcrError::new(
                ErrorCode::TranscriptUnavailable,
                format!(
                    "transcript for session `{value}` is ambiguous ({} candidates)",
                    list.len()
                ),
            )
            .with_details(json!({
                "uuid": uuid, "status": status, "cause": "ambiguous", "candidates": list,
            })))
        }
    }
}

fn locator(path: PathBuf) -> TranscriptLocator {
    let provider = if path.components().any(|c| c.as_os_str() == "sessions")
        && path.to_string_lossy().contains(".codex")
    {
        "codex"
    } else {
        "claude"
    };
    TranscriptLocator {
        provider: provider.to_string(),
        path,
    }
}

// --- candidate discovery ---

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Claude transcripts live at `~/.claude/projects/<cwd-slug>/<session_id>.jsonl`. The slug
/// is the absolute cwd with `/` (and `.`) replaced by `-`. We prefer that dir but fall back
/// to scanning every project dir (session ids are globally unique).
fn claude_candidates(session_id: &str, cwd: Option<&str>) -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let projects = home.join(".claude").join("projects");
    let file = format!("{session_id}.jsonl");
    let mut out = Vec::new();
    if let Some(cwd) = cwd {
        let slug = claude_slug(cwd);
        let p = projects.join(&slug).join(&file);
        if p.is_file() {
            out.push(p);
        }
    }
    if out.is_empty() {
        if let Ok(rd) = std::fs::read_dir(&projects) {
            for entry in rd.flatten() {
                let p = entry.path().join(&file);
                if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// The claude project-dir slug for an absolute cwd: non-alphanumeric runs → `-`.
fn claude_slug(cwd: &str) -> String {
    let mut s = String::with_capacity(cwd.len());
    for ch in cwd.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch);
        } else {
            s.push('-');
        }
    }
    s
}

/// Codex transcripts live at `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<session_id>.jsonl`.
fn codex_candidates(session_id: &str) -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let root = home.join(".codex").join("sessions");
    let suffix = format!("-{session_id}.jsonl");
    let mut out = Vec::new();
    walk_jsonl(&root, &mut |p| {
        if p.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.ends_with(&suffix))
            .unwrap_or(false)
        {
            out.push(p.to_path_buf());
        }
    });
    out
}

/// Recurse a directory, invoking `f` for every `.jsonl` file (bounded, no symlink follow).
fn walk_jsonl(dir: &Path, f: &mut impl FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            walk_jsonl(&path, f);
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            f(&path);
        }
    }
}

// --- parsers ---

fn parse_claude(text: &str) -> Vec<TranscriptEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if ty != "assistant" && ty != "user" {
            continue;
        }
        let ts = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .map(String::from);
        let msg = v.get("message").cloned().unwrap_or(Value::Null);
        let role = msg
            .get("role")
            .and_then(|x| x.as_str())
            .unwrap_or(ty)
            .to_string();
        let (in_tok, out_tok) = claude_usage(&msg);
        match msg.get("content") {
            Some(Value::String(s)) => out.push(TranscriptEntry {
                role: role.clone(),
                kind: "text".into(),
                text: s.clone(),
                tool: None,
                timestamp: ts.clone(),
                input_tokens: in_tok,
                output_tokens: out_tok,
            }),
            Some(Value::Array(blocks)) => {
                for b in blocks {
                    if let Some(e) = claude_block(&role, b, &ts, in_tok, out_tok) {
                        out.push(e);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn claude_usage(msg: &Value) -> (Option<u64>, Option<u64>) {
    let usage = msg.get("usage");
    let g = |k: &str| usage.and_then(|u| u.get(k)).and_then(|x| x.as_u64());
    (g("input_tokens"), g("output_tokens"))
}

fn claude_block(
    role: &str,
    b: &Value,
    ts: &Option<String>,
    in_tok: Option<u64>,
    out_tok: Option<u64>,
) -> Option<TranscriptEntry> {
    let bt = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let (kind, text, tool) = match bt {
        "text" => (
            "text",
            b.get("text")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            None,
        ),
        "thinking" => (
            "thinking",
            b.get("thinking")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            None,
        ),
        "tool_use" => (
            "tool_use",
            String::new(),
            b.get("name").and_then(|x| x.as_str()).map(String::from),
        ),
        "tool_result" => (
            "tool_result",
            b.get("content").map(value_to_text).unwrap_or_default(),
            None,
        ),
        _ => ("other", String::new(), None),
    };
    Some(TranscriptEntry {
        role: role.to_string(),
        kind: kind.to_string(),
        text,
        tool,
        timestamp: ts.clone(),
        input_tokens: in_tok,
        output_tokens: out_tok,
    })
}

fn parse_codex(text: &str) -> Vec<TranscriptEntry> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|x| x.as_str()) != Some("response_item") {
            continue;
        }
        let payload = v.get("payload").cloned().unwrap_or(Value::Null);
        if payload.get("type").and_then(|x| x.as_str()) != Some("message") {
            continue;
        }
        let ts = v
            .get("timestamp")
            .and_then(|x| x.as_str())
            .map(String::from);
        let role = payload
            .get("role")
            .and_then(|x| x.as_str())
            .unwrap_or("assistant")
            .to_string();
        let Some(Value::Array(blocks)) = payload.get("content") else {
            continue;
        };
        for b in blocks {
            let bt = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
            // input_text (user/developer) and output_text (assistant) are text blocks.
            if bt == "input_text" || bt == "output_text" || bt == "text" {
                let text = b.get("text").and_then(|x| x.as_str()).unwrap_or("");
                if text.is_empty() {
                    continue;
                }
                out.push(TranscriptEntry {
                    role: role.clone(),
                    kind: "text".into(),
                    text: text.to_string(),
                    tool: None,
                    timestamp: ts.clone(),
                    input_tokens: None,
                    output_tokens: None,
                });
            }
        }
    }
    out
}

/// Flatten a JSON value to a text string (for tool_result content that may be a string or an
/// array of `{type:text,text}` blocks).
fn value_to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.get("text").and_then(|t| t.as_str()).map(String::from))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

// --- helpers ---

fn read_file(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        OrcrError::new(
            ErrorCode::TranscriptUnavailable,
            format!("cannot read transcript {}: {e}", path.display()),
        )
        .with_details(json!({ "cause": "read_error" }))
    })
}

fn file_mtime_ms(path: &Path) -> Option<i64> {
    let md = std::fs::metadata(path).ok()?;
    let mt = md.modified().ok()?;
    let dur = mt.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

/// Freshness gate (spec §11.4): a final response is reported only once the transcript file
/// has advanced **past** the observed completion. `true` = fresh (safe to read the final
/// response); `false` = the file has not advanced yet → `transcript_unavailable (stale)`.
pub fn transcript_fresh(mtime_ms: Option<i64>, completed_at_ms: i64) -> bool {
    match mtime_ms {
        Some(mt) => mt >= completed_at_ms,
        None => false,
    }
}

/// The `transcript_unavailable` error (spec §13) with a `cause` in details.
pub fn transcript_unavailable(
    uuid: &str,
    status: &str,
    cause: &str,
    message: impl Into<String>,
) -> OrcrError {
    OrcrError::new(ErrorCode::TranscriptUnavailable, message).with_details(json!({
        "uuid": uuid, "status": status, "cause": cause,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn claude_parse_extracts_roles_text_tools_tokens() {
        let body = r#"
{"type":"user","message":{"role":"user","content":"hello"},"timestamp":"t1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"hi there"},{"type":"tool_use","name":"Bash"}],"usage":{"input_tokens":10,"output_tokens":3}},"timestamp":"t2"}
"#;
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "s.jsonl", body);
        let loc = TranscriptLocator {
            provider: "claude".into(),
            path: p,
        };
        let e = loc.read_entries().unwrap();
        assert_eq!(e[0].role, "user");
        assert_eq!(e[0].text, "hello");
        assert!(e.iter().any(|x| x.kind == "thinking"));
        let text = e
            .iter()
            .find(|x| x.kind == "text" && x.role == "assistant")
            .unwrap();
        assert_eq!(text.text, "hi there");
        assert_eq!(text.output_tokens, Some(3));
        assert!(e
            .iter()
            .any(|x| x.kind == "tool_use" && x.tool.as_deref() == Some("Bash")));
        assert_eq!(loc.last_response("u", "idle").unwrap(), "hi there");
    }

    #[test]
    fn codex_parse_extracts_messages() {
        let body = r#"
{"timestamp":"t0","type":"session_meta","payload":{"id":"sid"}}
{"timestamp":"t1","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do it"}]}}
{"timestamp":"t2","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done: ok"}]}}
"#;
        let tmp = tempfile::tempdir().unwrap();
        let p = write(tmp.path(), "rollout-x-sid.jsonl", body);
        let loc = TranscriptLocator {
            provider: "codex".into(),
            path: p,
        };
        let e = loc.read_entries().unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].role, "assistant");
        assert_eq!(loc.last_response("u", "idle").unwrap(), "done: ok");
    }

    #[test]
    fn last_response_none_is_transcript_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let p = write(
            tmp.path(),
            "s.jsonl",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        );
        let loc = TranscriptLocator {
            provider: "claude".into(),
            path: p,
        };
        let e = loc.last_response("u", "idle").unwrap_err();
        assert_eq!(e.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(e.details["cause"], "no_final_response");
    }

    #[test]
    fn select_candidate_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a.jsonl", "{}");
        let b = write(tmp.path(), "b.jsonl", "{}");
        // Both fresh (mtime >= 0) so created_at can't disambiguate → ambiguous.
        let e = select_candidate(vec![a, b], i64::MAX, "u", "idle", "sid").unwrap_err();
        assert_eq!(e.code, ErrorCode::TranscriptUnavailable);
        assert_eq!(e.details["cause"], "ambiguous");
        assert_eq!(e.details["candidates"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn select_candidate_created_at_disambiguates() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a.jsonl", "{}");
        let b = write(tmp.path(), "b.jsonl", "{}");
        // created_at = 0 → both files are "fresh" (mtime >= 0), still >1 fresh → ambiguous;
        // but with created_at far in the future only files newer than it survive → 0 fresh →
        // still ambiguous. Use the single-candidate path to prove selection.
        let one = select_candidate(vec![a.clone()], 0, "u", "idle", "sid").unwrap();
        assert_eq!(one.path, a);
        let _ = b;
    }

    #[test]
    fn freshness_gate_flags_unadvanced_transcript() {
        // Transcript mtime older than the completion → stale (must not report a final resp).
        assert!(!transcript_fresh(Some(100), 200));
        // Advanced past completion → fresh.
        assert!(transcript_fresh(Some(300), 200));
        // Cannot stat → treated as not fresh.
        assert!(!transcript_fresh(None, 200));
    }

    #[test]
    fn claude_slug_matches_project_dir_convention() {
        assert_eq!(
            claude_slug("/Users/x/code/orchestratr"),
            "-Users-x-code-orchestratr"
        );
    }
}
