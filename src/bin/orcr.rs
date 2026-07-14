//! The `orcr` binary.
//!
//! M0 ships only foundations — there are no user-facing agent verbs yet (server, agent,
//! loop, top land in later milestones). The one thing wired up here is an internal
//! self-check (`orcr __m0-selfcheck`) that exercises the foundation modules end-to-end
//! against the real environment: home layout + safety, config load/validate, store
//! open/version, herdr binary discovery, session enumeration, and a live schema
//! conformance check. It never touches the user's `default` herdr session.

use orchestratr::config::Config;
use orchestratr::driver::contract::{
    schema_methods, schema_protocol, schema_result_types, DRIVER_CONTRACT,
};
use orchestratr::driver::{HerdrBinary, IntegrationState};
use orchestratr::home::Home;
use orchestratr::store::Store;
use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("__m0-selfcheck") => std::process::exit(selfcheck()),
        _ => {
            eprintln!(
                "orcr — orchestratr (M0: foundations only)\n\
                 User-facing commands (server, agent, loop, top) land in later milestones.\n\
                 Internal: orcr __m0-selfcheck"
            );
            std::process::exit(0);
        }
    }
}

/// Run the foundation self-check; returns a process exit code.
fn selfcheck() -> i32 {
    let mut ok = true;

    macro_rules! step {
        ($label:expr, $body:expr) => {{
            match $body {
                Ok(v) => {
                    println!("[ ok ] {}", $label);
                    Some(v)
                }
                Err(e) => {
                    println!("[FAIL] {}: {}", $label, e);
                    ok = false;
                    None
                }
            }
        }};
    }

    let home = step!("home layout ensured + safe", Home::ensure());
    if let Some(home) = &home {
        println!("       home = {}", home.root().display());
        let loaded = step!("config load + validate", Config::load(home));
        if let Some(loaded) = &loaded {
            for w in &loaded.warnings {
                println!("       warning: {w}");
            }
        }
        step!(
            "store open + schema version",
            Store::open(home.store_path())
        );
    }

    let cfg = Config::load(&Home::resolve().unwrap_or_else(|_| Home::at("/tmp/orcr")))
        .map(|l| l.config)
        .unwrap_or_default();

    let bin = step!(
        "herdr binary discovery",
        HerdrBinary::discover(Some(cfg.herdr.bin.as_str()))
    );
    if let Some(bin) = &bin {
        println!("       herdr = {}", bin.path().display());
        if let Some(sessions) = step!("herdr session enumeration", bin.session_list()) {
            println!("       sessions: {}", sessions.len());
        }
        if let Some(raw) = step!("herdr integration status", bin.integration_status_raw()) {
            let st = IntegrationState::from_herdr_status(&raw);
            println!("       supported providers: {:?}", st.supported());
        }
        step!("live schema conformance", live_conformance(bin));
    }

    if ok {
        println!("\nM0 self-check: PASS");
        0
    } else {
        println!("\nM0 self-check: FAIL");
        2
    }
}

/// Verify every pinned driver method + result type exists in the live herdr schema and
/// the protocol matches what orcr is built against.
fn live_conformance(bin: &HerdrBinary) -> Result<(), String> {
    let out = Command::new(bin.path())
        .args(["api", "schema", "--json"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to run `herdr api schema --json`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`herdr api schema --json` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let schema: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad schema json: {e}"))?;

    let fx_protocol = orchestratr::driver::protocol::MIN_HERDR_PROTOCOL;
    let live_protocol =
        schema_protocol(&schema).ok_or_else(|| "schema has no protocol field".to_string())?;
    if live_protocol != fx_protocol {
        return Err(format!(
            "herdr protocol {live_protocol} != expected {fx_protocol} (version drift)"
        ));
    }
    let methods = schema_methods(&schema);
    let types = schema_result_types(&schema);
    for op in DRIVER_CONTRACT {
        if !methods.iter().any(|m| m == op.method) {
            return Err(format!("method `{}` absent from live schema", op.method));
        }
        if !types.iter().any(|t| t == op.result_type) {
            return Err(format!(
                "result type `{}` absent from live schema",
                op.result_type
            ));
        }
    }
    Ok(())
}
