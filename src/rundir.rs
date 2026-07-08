use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMeta {
    pub id: String,
    pub name: Option<String>,
    pub parent_id: Option<String>,
    pub harness: String,
    pub model: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub ended_at: Option<String>,
    pub cwd: Option<String>,
    pub pane_id: Option<String>,
    pub terminal_id: Option<String>,
    pub response_source: Option<String>,
    pub turns: Vec<RunMetaTurn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMetaTurn {
    pub n: i64,
    pub prompt_paths: Vec<String>,
    pub response_path: String,
    pub response_source: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptInput<'a> {
    Inline(&'a str),
    File(&'a Path),
}

pub fn run_dir_for_id(store_root: &Path, agent_id: &str) -> PathBuf {
    store_root.join("runs").join(agent_id)
}

pub fn create_run_dir(store_root: &Path, agent_id: &str) -> Result<PathBuf> {
    let run_dir = run_dir_for_id(store_root, agent_id);
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create run dir {}", run_dir.display()))?;
    Ok(run_dir)
}

pub fn write_meta(run_dir: &Path, meta: &RunMeta) -> Result<PathBuf> {
    fs::create_dir_all(run_dir)
        .with_context(|| format!("failed to create run dir {}", run_dir.display()))?;
    let path = run_dir.join("meta.json");
    let json = serde_json::to_string_pretty(meta)?;
    fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

pub fn prompt_path(run_dir: &Path, turn: u32) -> PathBuf {
    run_dir.join(format!("{turn:03}-prompt.md"))
}

pub fn steer_prompt_path(run_dir: &Path, turn: u32, steer_index: u32) -> Result<PathBuf> {
    if steer_index < 2 {
        bail!("steer prompt index must be 2 or greater");
    }
    Ok(run_dir.join(format!("{turn:03}-prompt.{steer_index}.md")))
}

pub fn response_path(run_dir: &Path, turn: u32) -> PathBuf {
    run_dir.join(format!("{turn:03}-response.md"))
}

pub fn build_preamble(response_path: &Path) -> String {
    format!(
        "When you are completely finished, write your full final answer as markdown to the file: {}. Do not consider the task done until that file is written.",
        response_path.display()
    )
}

pub fn append_preamble(prompt_text: &str, response_path: &Path) -> String {
    format!("{prompt_text}\n\n{}", build_preamble(response_path))
}

pub fn persist_prompt(run_dir: &Path, turn: u32, input: PromptInput<'_>) -> Result<PathBuf> {
    persist_prompt_to_path(
        prompt_path(run_dir, turn),
        response_path(run_dir, turn),
        input,
    )
}

pub fn persist_steer_prompt(
    run_dir: &Path,
    turn: u32,
    steer_index: u32,
    input: PromptInput<'_>,
) -> Result<PathBuf> {
    persist_prompt_to_path(
        steer_prompt_path(run_dir, turn, steer_index)?,
        response_path(run_dir, turn),
        input,
    )
}

fn persist_prompt_to_path(
    destination: PathBuf,
    response_path: PathBuf,
    input: PromptInput<'_>,
) -> Result<PathBuf> {
    let prompt_text = match input {
        PromptInput::Inline(text) => text.to_string(),
        PromptInput::File(path) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
    };
    let delivered_prompt = append_preamble(&prompt_text, &response_path);
    fs::write(&destination, delivered_prompt)
        .with_context(|| format!("failed to write prompt {}", destination.display()))?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_flat_run_dir() {
        let temp = tempdir().unwrap();
        let run_dir = create_run_dir(temp.path(), "a7").unwrap();

        assert_eq!(run_dir.parent().unwrap(), temp.path().join("runs"));
        assert_eq!(
            run_dir.file_name().and_then(|name| name.to_str()),
            Some("a7")
        );
        assert!(run_dir.is_dir());
    }

    #[test]
    fn writes_meta_json() {
        let temp = tempdir().unwrap();
        let meta = RunMeta {
            id: "a1".to_string(),
            name: None,
            parent_id: None,
            harness: "mock".to_string(),
            model: String::new(),
            status: "starting".to_string(),
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            ended_at: None,
            cwd: Some("/tmp".to_string()),
            pane_id: Some("w1:p1".to_string()),
            terminal_id: None,
            response_source: None,
            turns: Vec::new(),
        };

        let path = write_meta(temp.path(), &meta).unwrap();
        let parsed: RunMeta = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(parsed, meta);
    }

    #[test]
    fn path_naming_for_turns_and_steers() {
        let run_dir = Path::new("/tmp/run");

        assert_eq!(prompt_path(run_dir, 1), Path::new("/tmp/run/001-prompt.md"));
        assert_eq!(
            steer_prompt_path(run_dir, 1, 2).unwrap(),
            Path::new("/tmp/run/001-prompt.2.md")
        );
        assert_eq!(
            steer_prompt_path(run_dir, 12, 3).unwrap(),
            Path::new("/tmp/run/012-prompt.3.md")
        );
        assert_eq!(
            response_path(run_dir, 2),
            Path::new("/tmp/run/002-response.md")
        );
        assert!(steer_prompt_path(run_dir, 1, 1).is_err());
    }

    #[test]
    fn preamble_text_matches_contract() {
        let response = Path::new("/abs/001-response.md");
        assert_eq!(
            build_preamble(response),
            "When you are completely finished, write your full final answer as markdown to the file: /abs/001-response.md. Do not consider the task done until that file is written."
        );
    }

    #[test]
    fn prompt_persistence_inline_includes_preamble() {
        let temp = tempdir().unwrap();
        let path = persist_prompt(temp.path(), 1, PromptInput::Inline("do work")).unwrap();
        let content = fs::read_to_string(path).unwrap();

        assert_eq!(
            content,
            format!(
                "do work\n\n{}",
                build_preamble(&temp.path().join("001-response.md"))
            )
        );
    }

    #[test]
    fn prompt_persistence_file_copy_includes_preamble() {
        let temp = tempdir().unwrap();
        let source = temp.path().join("source.md");
        fs::write(&source, "from file").unwrap();

        let path = persist_prompt(temp.path(), 2, PromptInput::File(&source)).unwrap();
        let content = fs::read_to_string(path).unwrap();

        assert_eq!(
            content,
            format!(
                "from file\n\n{}",
                build_preamble(&temp.path().join("002-response.md"))
            )
        );
    }

    #[test]
    fn steer_prompt_persistence_uses_same_turn_response() {
        let temp = tempdir().unwrap();
        let path = persist_steer_prompt(temp.path(), 1, 2, PromptInput::Inline("steer")).unwrap();
        let content = fs::read_to_string(&path).unwrap();

        assert_eq!(path, temp.path().join("001-prompt.2.md"));
        assert!(content.contains("steer\n\nWhen you are completely finished"));
        assert!(content.contains("001-response.md"));
    }
}
