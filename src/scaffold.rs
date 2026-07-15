//! `orcr scaffold`: generate a ready-to-run TypeScript workflow project.
//!
//! This is the one orcr feature that needs Node — and the only CLI verb that never talks to
//! the server (no store row, no auto-start). It writes exactly three files into `<dir>`
//! (default `.`, created if missing) and runs `npm install`:
//!
//! ```text
//! package.json    @orchestratr/sdk (pinned to this CLI's version) · tsx · typescript
//! tsconfig.json
//! workflow.ts     ~15-line runnable example + one skill-reference comment
//! ```
//!
//! Rules: requires Node ≥ 20 + npm (missing → `environment_error`, nothing created);
//! never overwrites an existing one of the three files (→ `state_conflict`, nothing touched);
//! purely local.

use crate::error::{ErrorCode, OrcrError, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The three files scaffold generates (never more, never fewer).
pub const GENERATED_FILES: &[&str] = &["package.json", "tsconfig.json", "workflow.ts"];

/// Minimum Node major version the generated project (and `npx tsx`) requires.
pub const MIN_NODE_MAJOR: u64 = 20;

/// Outcome of a successful scaffold, for the `--json` envelope and human output.
#[derive(Debug, Clone)]
pub struct ScaffoldOutcome {
    pub dir: PathBuf,
    pub files: Vec<String>,
    pub sdk_version: String,
    pub sdk_spec: String,
    pub npm_installed: bool,
}

impl ScaffoldOutcome {
    pub fn to_json(&self) -> Value {
        json!({
            "dir": self.dir.display().to_string(),
            "files": self.files,
            "sdk_version": self.sdk_version,
            "sdk_spec": self.sdk_spec,
            "npm_installed": self.npm_installed,
        })
    }
}

/// The SDK version this CLI pins scaffolds to — always the CLI's own version. The acceptance
/// check "pinned SDK version equals the CLI version" reads this.
pub fn sdk_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// The dependency spec written into `package.json` for `@orchestratr/sdk`.
///
/// By default this is the pinned CLI version (so the published, registry-resolved case works
/// and the version-pin acceptance holds). For local development / CI where the SDK is not
/// published, `ORCR_SDK_SPEC` overrides it with an installable spec (e.g. a `file:` path or a
/// tarball) so `npm install` + `npx tsx workflow.ts` run green against the local build. See
/// `m7-sdk-skill/notes.md`.
fn sdk_dependency_spec() -> String {
    std::env::var("ORCR_SDK_SPEC")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(sdk_version)
}

fn package_json(spec: &str) -> String {
    let doc = json!({
        "name": "orcr-workflow",
        "private": true,
        "type": "module",
        "version": "0.0.0",
        "scripts": {
            "start": "tsx workflow.ts"
        },
        "dependencies": {
            "@orchestratr/sdk": spec
        },
        "devDependencies": {
            "tsx": "^4",
            "typescript": "^5"
        }
    });
    let mut s = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

fn tsconfig_json() -> String {
    let doc = json!({
        "compilerOptions": {
            "target": "ES2022",
            "module": "ES2022",
            "moduleResolution": "bundler",
            "strict": true,
            "esModuleInterop": true,
            "skipLibCheck": true,
            "types": ["node"]
        },
        "include": ["*.ts"]
    });
    let mut s = serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

fn workflow_ts() -> String {
    // ~15-line runnable example: scope → run --name → wait → last-response, plus one comment
    // pointing at the skill reference for the full SDK surface.
    r#"import { orcr } from "@orchestratr/sdk";

// A minimal, runnable orchestration. For the full SDK surface (collections, scopes,
// watch, loops, the file convention) read the skill reference: skill/references/sdk.md
await orcr.scope("scaffold_demo", async () => {
  const a = await orcr.agent.run({
    // agent: "claude",           // optional — omitted here, so it uses config defaults.agent
    name: "hello",                // naming is mandatory — pass name OR path
    gc: "immediate",              // one-shot: settles on ended(completed)
    prompt: "Reply with a single friendly sentence, then say DONE.",
  });
  await a.wait();
  console.log(await a.lastResponse());
});
"#
    .to_string()
}

/// Run the scaffold. `dir` defaults to `.`; `run_install` controls the trailing
/// `npm install` (tests skip it for speed). Preflight and overwrite checks happen *before*
/// anything is created.
pub fn scaffold(dir: Option<&str>, run_install: bool) -> Result<ScaffoldOutcome> {
    let dir = PathBuf::from(dir.unwrap_or("."));

    // Preflight: Node ≥ 20 + npm present. On failure, nothing is created.
    preflight_node()?;
    preflight_npm()?;

    // Never overwrite: if any of the three files already exists, fail and touch nothing.
    for f in GENERATED_FILES {
        let p = dir.join(f);
        if p.exists() {
            return Err(OrcrError::new(
                ErrorCode::StateConflict,
                format!(
                    "`{}` already exists in {}; scaffold never overwrites",
                    f,
                    dir.display()
                ),
            )
            .with_details(json!({
                "reason": "file_exists",
                "file": f,
                "dir": dir.display().to_string(),
            })));
        }
    }

    // Create the dir (and parents) if missing.
    std::fs::create_dir_all(&dir).map_err(|e| {
        OrcrError::environment(
            "scaffold_failed",
            format!("cannot create directory {}: {e}", dir.display()),
        )
    })?;

    let spec = sdk_dependency_spec();
    write_file(&dir, "package.json", &package_json(&spec))?;
    write_file(&dir, "tsconfig.json", &tsconfig_json())?;
    write_file(&dir, "workflow.ts", &workflow_ts())?;

    let npm_installed = if run_install {
        npm_install(&dir)?;
        true
    } else {
        false
    };

    Ok(ScaffoldOutcome {
        dir,
        files: GENERATED_FILES.iter().map(|s| s.to_string()).collect(),
        sdk_version: sdk_version(),
        sdk_spec: spec,
        npm_installed,
    })
}

fn write_file(dir: &Path, name: &str, contents: &str) -> Result<()> {
    let p = dir.join(name);
    std::fs::write(&p, contents).map_err(|e| {
        OrcrError::environment(
            "scaffold_failed",
            format!("cannot write {}: {e}", p.display()),
        )
    })
}

/// Preflight Node ≥ 20. Missing/old → `environment_error` with an install pointer.
fn preflight_node() -> Result<()> {
    let out = std::process::Command::new("node")
        .arg("--version")
        .output()
        .map_err(|_| node_missing())?;
    if !out.status.success() {
        return Err(node_missing());
    }
    let v = String::from_utf8_lossy(&out.stdout);
    let major = parse_node_major(v.trim());
    match major {
        Some(m) if m >= MIN_NODE_MAJOR => Ok(()),
        Some(m) => Err(OrcrError::environment(
            "node_too_old",
            format!(
                "orcr scaffold requires Node \u{2265} {MIN_NODE_MAJOR}, but found Node {m}. \
                 Install a newer Node from https://nodejs.org/"
            ),
        )
        .with_details(json!({
            "cause": "node_too_old", "found_major": m, "required_major": MIN_NODE_MAJOR,
            "install": "https://nodejs.org/",
        }))),
        None => Err(node_missing()),
    }
}

fn preflight_npm() -> Result<()> {
    let out = std::process::Command::new("npm").arg("--version").output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err(OrcrError::environment(
            "npm_missing",
            "orcr scaffold requires npm (it ships with Node). Install Node from https://nodejs.org/",
        )
        .with_details(json!({ "cause": "npm_missing", "install": "https://nodejs.org/" }))),
    }
}

fn node_missing() -> OrcrError {
    OrcrError::environment(
        "node_missing",
        format!(
            "orcr scaffold requires Node \u{2265} {MIN_NODE_MAJOR}. \
             Install it from https://nodejs.org/ (this is the only orcr feature that needs Node)."
        ),
    )
    .with_details(json!({
        "cause": "node_missing", "required_major": MIN_NODE_MAJOR, "install": "https://nodejs.org/",
    }))
}

/// Parse a `node --version` string (`v24.2.0`) into its major number.
pub fn parse_node_major(s: &str) -> Option<u64> {
    let s = s.trim().trim_start_matches('v');
    s.split('.').next()?.parse::<u64>().ok()
}

/// Run `npm install` in `dir`, inheriting stdio. Failure → `environment_error`.
fn npm_install(dir: &Path) -> Result<()> {
    let status = std::process::Command::new("npm")
        .arg("install")
        .current_dir(dir)
        .status()
        .map_err(|e| {
            OrcrError::environment(
                "npm_install_failed",
                format!("failed to run npm install: {e}"),
            )
        })?;
    if !status.success() {
        return Err(OrcrError::environment(
            "npm_install_failed",
            format!("npm install exited with status {status}"),
        )
        .with_details(json!({ "cause": "npm_install_failed" })));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_major() {
        assert_eq!(parse_node_major("v24.2.0"), Some(24));
        assert_eq!(parse_node_major("20.0.0"), Some(20));
        assert_eq!(parse_node_major("v18.19.1"), Some(18));
        assert_eq!(parse_node_major("garbage"), None);
    }

    #[test]
    fn package_json_pins_sdk_version() {
        // Default (no ORCR_SDK_SPEC) pins to the CLI version.
        let s = package_json(&sdk_version());
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["dependencies"]["@orchestratr/sdk"], json!(sdk_version()));
        assert_eq!(v["type"], "module");
        assert!(v["devDependencies"]["tsx"].is_string());
        assert!(v["devDependencies"]["typescript"].is_string());
    }

    #[test]
    fn workflow_names_the_agent() {
        let w = workflow_ts();
        // The example must carry --name/path (naming is mandatory) and point at the skill ref.
        assert!(w.contains("name:"));
        assert!(w.contains("skill/references/sdk.md"));
        assert!(w.contains("orcr.scope"));
    }

    #[test]
    fn state_conflict_when_file_present() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("package.json"), "{}").unwrap();
        // Skip real node preflight by pointing at a dir with an existing file — but preflight
        // runs first. If node is present this returns state_conflict; if not, environment_error.
        // We only assert the overwrite guard when node preflight passes.
        if preflight_node().is_ok() && preflight_npm().is_ok() {
            let e = scaffold(Some(tmp.path().to_str().unwrap()), false).unwrap_err();
            assert_eq!(e.code, ErrorCode::StateConflict);
            assert_eq!(e.details["reason"], "file_exists");
            // Nothing else created.
            assert!(!tmp.path().join("workflow.ts").exists());
        }
    }

    #[test]
    fn generates_three_files() {
        if preflight_node().is_err() || preflight_npm().is_err() {
            return; // no node in this environment
        }
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("proj");
        let out = scaffold(Some(sub.to_str().unwrap()), false).unwrap();
        assert_eq!(out.files.len(), 3);
        for f in GENERATED_FILES {
            assert!(sub.join(f).exists(), "{f} not created");
        }
        assert_eq!(out.sdk_version, sdk_version());
    }
}
