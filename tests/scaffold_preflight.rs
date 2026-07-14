//! `orcr scaffold` preflight (spec §6.6 / M7 acceptance): a missing Node must fail
//! `environment_error` (exit 2) with **nothing created**.
//!
//! This runs the real `orcr` binary with a PATH scrubbed of `node`/`npm`, so it verifies the
//! acceptance item deterministically regardless of whether the host has Node installed. It needs
//! no herdr and no server (scaffold is purely local), so it runs in the default suite.

use serde_json::Value;
use std::process::Command;

#[test]
fn scaffold_missing_node_fails_environment_error_nothing_created() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // A non-existent target dir: preflight runs *before* the dir is created, so it must stay
    // absent on failure (nothing created).
    let proj = tmp.path().join("proj");
    // An empty dir as the entire PATH: `node`/`npm` can't be resolved, so preflight_node fails.
    let empty_path = tmp.path().join("emptybin");
    std::fs::create_dir_all(&empty_path).unwrap();

    let orcr = env!("CARGO_BIN_EXE_orcr");
    let out = Command::new(orcr)
        .args(["--json", "scaffold", proj.to_str().unwrap()])
        .env("PATH", &empty_path)
        // Point at a throwaway home just in case; scaffold never touches it.
        .env("ORCR_HOME", tmp.path().join("home"))
        .output()
        .expect("run orcr scaffold");

    // §13: environment_error → exit 2.
    assert_eq!(
        out.status.code(),
        Some(2),
        "expected exit 2 (environment_error); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let env: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "environment_error");
    // The cause is node-related (missing, since PATH has no node).
    let cause = env["error"]["details"]["cause"]
        .as_str()
        .unwrap_or_default();
    assert!(
        cause == "node_missing" || cause == "node_too_old",
        "unexpected cause `{cause}`"
    );

    // Nothing created: the target dir must not exist.
    assert!(
        !proj.exists(),
        "scaffold created files despite failed preflight"
    );
}
