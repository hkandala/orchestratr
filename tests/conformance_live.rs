//! Live driver conformance suite (spec §11.7): every pinned herdr method + result-type
//! tag in the driver contract must exist in the installed herdr's `api schema`, and the
//! declared protocol must match what orcr is built against. Version drift fails here.
//!
//! Gated behind `ORCR_E2E=1` because it shells the herdr binary. The offline half — that
//! the contract table matches the checked-in fixture — runs unconditionally as a unit
//! test in `src/driver/contract.rs`.

use orchestratr::driver::contract::{
    schema_methods, schema_protocol, schema_result_types, Fixture, DRIVER_CONTRACT,
};
use orchestratr::driver::protocol::MIN_HERDR_PROTOCOL;
use orchestratr::driver::HerdrBinary;
use std::process::{Command, Stdio};

fn e2e_enabled() -> bool {
    std::env::var("ORCR_E2E").as_deref() == Ok("1")
}

#[test]
fn live_schema_matches_contract() {
    if !e2e_enabled() {
        eprintln!("skipping (set ORCR_E2E=1)");
        return;
    }
    let bin = HerdrBinary::discover(None).expect("herdr binary on PATH");
    let out = Command::new(bin.path())
        .args(["api", "schema", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("run herdr api schema");
    assert!(out.status.success(), "herdr api schema failed");
    let schema: serde_json::Value = serde_json::from_slice(&out.stdout).expect("schema json");

    // Protocol must match both the fixture and MIN_HERDR_PROTOCOL — drift fails.
    let live = schema_protocol(&schema).expect("schema protocol");
    assert_eq!(
        live, MIN_HERDR_PROTOCOL,
        "live herdr protocol {live} != expected {MIN_HERDR_PROTOCOL} (version drift)"
    );
    assert_eq!(
        Fixture::load().protocol,
        live,
        "fixture protocol drifted from live herdr"
    );

    // Every pinned method + result type must exist in the live schema.
    let methods = schema_methods(&schema);
    let types = schema_result_types(&schema);
    for op in DRIVER_CONTRACT {
        assert!(
            methods.iter().any(|m| m == op.method),
            "herdr method `{}` (op `{}`) absent from live schema",
            op.method,
            op.op
        );
        assert!(
            types.iter().any(|t| t == op.result_type),
            "herdr result type `{}` (op `{}`) absent from live schema",
            op.result_type,
            op.op
        );
    }
}
