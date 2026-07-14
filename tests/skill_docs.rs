//! Doc-tests for the skill (spec §10 acceptance): the reference files must contain **no stale
//! flags** (every long flag in an `orcr …` CLI example exists in the live `--help`), and every
//! `orcr agent run` / `orcr agent ask` sample must carry `--name` or `--path` (naming is
//! mandatory). These run in the default (fast) suite — no herdr, no server.

use std::collections::HashSet;
use std::process::Command;

fn orcr_bin() -> String {
    env!("CARGO_BIN_EXE_orcr").to_string()
}

/// Every command path whose `--help` we harvest long flags from.
const COMMAND_PATHS: &[&[&str]] = &[
    &[],
    &["agent", "run"],
    &["agent", "ask"],
    &["agent", "send"],
    &["agent", "wait"],
    &["agent", "logs"],
    &["agent", "kill"],
    &["agent", "ls"],
    &["agent", "attach"],
    &["loop", "create"],
    &["loop", "pause"],
    &["loop", "resume"],
    &["loop", "rm"],
    &["loop", "ls"],
    &["loop", "logs"],
    &["loop", "run", "start"],
    &["loop", "run", "stop"],
    &["loop", "run", "ls"],
    &["server", "start"],
    &["server", "stop"],
    &["server", "status"],
    &["server", "logs"],
    &["server", "enable"],
    &["server", "disable"],
    &["api", "schema"],
    &["api", "snapshot"],
    &["scaffold"],
    &["top"],
];

/// Harvest the set of long flags (`--foo`) valid across the whole CLI from `--help` output.
fn valid_flags() -> HashSet<String> {
    let mut flags = HashSet::new();
    // Always-valid globals.
    for g in ["--json", "--help", "--version"] {
        flags.insert(g.to_string());
    }
    for path in COMMAND_PATHS {
        let mut args: Vec<&str> = path.to_vec();
        args.push("--help");
        let out = match Command::new(orcr_bin()).args(&args).output() {
            Ok(o) => o,
            Err(_) => continue,
        };
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for tok in extract_long_flags(&text) {
            flags.insert(tok);
        }
    }
    flags
}

/// Extract every `--flag` token from a blob (stops at `=`, `,`, whitespace, `<`, `]`, `)`).
fn extract_long_flags(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' {
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() {
                let c = bytes[j];
                if c.is_ascii_alphanumeric() || c == b'-' {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > start + 2 {
                out.push(text[start..j].to_string());
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Join backslash-continued lines so a multi-line invocation is one logical line.
fn join_continuations(src: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for raw in src.lines() {
        let trimmed_end = raw.trim_end();
        if let Some(stripped) = trimmed_end.strip_suffix('\\') {
            cur.push_str(stripped);
            cur.push(' ');
        } else {
            cur.push_str(raw);
            lines.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn doc_files() -> Vec<std::path::PathBuf> {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skill");
    let mut files = vec![root.join("SKILL.md")];
    let refs = root.join("references");
    for name in ["cli.md", "sdk.md", "patterns.md", "loops.md", "files.md"] {
        files.push(refs.join(name));
    }
    files
}

/// A shell `orcr …` invocation on a line (not the TS `orcr.` accessor). Returns the substring
/// from `orcr ` onward, else None.
fn orcr_invocation(line: &str) -> Option<&str> {
    let mut search_from = 0;
    while let Some(rel) = line[search_from..].find("orcr ") {
        let idx = search_from + rel;
        // Must be a word boundary before `orcr` and not the `orcr.` SDK accessor.
        let before_ok = idx == 0 || !line.as_bytes()[idx - 1].is_ascii_alphanumeric();
        // `orcr ` (space after) already excludes `orcr.`; also skip if it's `orcr://` etc.
        if before_ok {
            return Some(&line[idx..]);
        }
        search_from = idx + 5;
    }
    None
}

#[test]
fn references_contain_no_stale_flags() {
    let valid = valid_flags();
    assert!(
        valid.contains("--name") && valid.contains("--gc"),
        "flag harvest failed (got {} flags)",
        valid.len()
    );
    let mut stale: Vec<String> = Vec::new();
    for file in doc_files() {
        let src = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for line in join_continuations(&src) {
            let Some(inv) = orcr_invocation(&line) else {
                continue;
            };
            for flag in extract_long_flags(inv) {
                if !valid.contains(&flag) {
                    stale.push(format!("{}: `{flag}` in `{}`", file.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale CLI flags in skill docs (not in --help):\n{}",
        stale.join("\n")
    );
}

#[test]
fn run_and_ask_samples_carry_name_or_path() {
    let mut offenders: Vec<String> = Vec::new();
    for file in doc_files() {
        let src = std::fs::read_to_string(&file).unwrap();
        for line in join_continuations(&src) {
            let Some(inv) = orcr_invocation(&line) else {
                continue;
            };
            let is_run = inv.starts_with("orcr agent run");
            let is_ask = inv.starts_with("orcr agent ask");
            // Only enforce on actual command *samples* (those invoked with flags/args), not
            // prose mentions like "run one `orcr agent ask`".
            let is_sample = inv.contains("--") || inv.contains(" -");
            if (is_run || is_ask)
                && is_sample
                && !(inv.contains("--name") || inv.contains("--path"))
            {
                offenders.push(format!("{}: `{}`", file.display(), line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "run/ask samples missing --name/--path (naming is mandatory):\n{}",
        offenders.join("\n")
    );
}
