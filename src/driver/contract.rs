//! The herdr driver contract table (spec §11.7): every orcr driver operation is pinned
//! to a named herdr socket method with a fixed result-type tag. This table is the source
//! of truth; a conformance fixture (`fixtures/herdr-contract.json`, generated from the
//! installed herdr's `api schema`) pins the herdr-side facts. Version drift between the
//! table, the fixture, and the live herdr schema fails the conformance suite.

use serde::Deserialize;

/// One pinned driver operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverOp {
    /// orcr's operation name (a `HerdrDriver` method).
    pub op: &'static str,
    /// The herdr socket method it maps to.
    pub method: &'static str,
    /// The tagged-union `type` of the herdr result orcr expects.
    pub result_type: &'static str,
}

/// The complete set of herdr methods orcr's M0 driver depends on.
pub const DRIVER_CONTRACT: &[DriverOp] = &[
    DriverOp {
        op: "ping",
        method: "ping",
        result_type: "pong",
    },
    DriverOp {
        op: "session_snapshot",
        method: "session.snapshot",
        result_type: "session_snapshot",
    },
    DriverOp {
        op: "agent_list",
        method: "agent.list",
        result_type: "agent_list",
    },
    DriverOp {
        op: "pane_list",
        method: "pane.list",
        result_type: "pane_list",
    },
    DriverOp {
        op: "pane_get",
        method: "pane.get",
        result_type: "pane_info",
    },
    DriverOp {
        op: "workspace_list",
        method: "workspace.list",
        result_type: "workspace_list",
    },
    DriverOp {
        op: "workspace_create",
        method: "workspace.create",
        result_type: "workspace_created",
    },
    DriverOp {
        op: "agent_start",
        method: "agent.start",
        result_type: "agent_started",
    },
    DriverOp {
        op: "pane_send_text",
        method: "pane.send_text",
        result_type: "ok",
    },
    DriverOp {
        op: "pane_send_keys",
        method: "pane.send_keys",
        result_type: "ok",
    },
    DriverOp {
        op: "pane_read",
        method: "pane.read",
        result_type: "pane_read",
    },
    DriverOp {
        op: "pane_move",
        method: "pane.move",
        result_type: "pane_move",
    },
    DriverOp {
        op: "pane_close",
        method: "pane.close",
        result_type: "ok",
    },
    DriverOp {
        op: "notification_show",
        method: "notification.show",
        result_type: "notification_show",
    },
    DriverOp {
        op: "pane_report_agent",
        method: "pane.report_agent",
        result_type: "ok",
    },
];

/// The checked-in conformance fixture (generated from the installed herdr's `api schema`).
pub const FIXTURE_JSON: &str = include_str!("../../fixtures/herdr-contract.json");

#[derive(Debug, Deserialize)]
pub struct Fixture {
    pub protocol: u32,
    pub ops: Vec<FixtureOp>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct FixtureOp {
    pub op: String,
    pub method: String,
    pub result_type: String,
}

impl Fixture {
    /// Load the checked-in fixture.
    pub fn load() -> Fixture {
        serde_json::from_str(FIXTURE_JSON).expect("fixtures/herdr-contract.json is valid JSON")
    }
}

/// Extract the set of request method consts from a live `herdr api schema` document.
pub fn schema_methods(schema: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(variants) = schema
        .pointer("/schemas/request/oneOf")
        .and_then(|v| v.as_array())
    {
        for v in variants {
            if let Some(m) = v
                .pointer("/properties/method/const")
                .and_then(|x| x.as_str())
            {
                out.push(m.to_string());
            }
        }
    }
    out
}

/// Extract the set of result `type` consts from a live `herdr api schema` document.
pub fn schema_result_types(schema: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(variants) = schema
        .pointer("/schemas/success_response/$defs/ResponseResult/oneOf")
        .and_then(|v| v.as_array())
    {
        for v in variants {
            if let Some(t) = v.pointer("/properties/type/const").and_then(|x| x.as_str()) {
                out.push(t.to_string());
            }
        }
    }
    out
}

/// The herdr protocol number a live schema declares.
pub fn schema_protocol(schema: &serde_json::Value) -> Option<u32> {
    schema
        .get("protocol")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::protocol::MIN_HERDR_PROTOCOL;

    /// The contract table and the checked-in fixture must agree exactly, and the fixture's
    /// protocol must match what the driver is built against. This runs without herdr.
    #[test]
    fn contract_table_matches_fixture() {
        let fx = Fixture::load();
        assert_eq!(
            fx.protocol, MIN_HERDR_PROTOCOL,
            "fixture protocol must match MIN_HERDR_PROTOCOL"
        );
        assert_eq!(
            fx.ops.len(),
            DRIVER_CONTRACT.len(),
            "fixture op count differs from the contract table"
        );
        for op in DRIVER_CONTRACT {
            let f = fx
                .ops
                .iter()
                .find(|f| f.op == op.op)
                .unwrap_or_else(|| panic!("op `{}` missing from fixture", op.op));
            assert_eq!(f.method, op.method, "method drift for op `{}`", op.op);
            assert_eq!(
                f.result_type, op.result_type,
                "result_type drift for op `{}`",
                op.op
            );
        }
    }

    #[test]
    fn extracts_methods_and_types_from_schema() {
        let schema = serde_json::json!({
            "protocol": 16,
            "schemas": {
                "request": {"oneOf": [
                    {"properties": {"method": {"const": "ping"}}},
                    {"properties": {"method": {"const": "agent.list"}}}
                ]},
                "success_response": {"$defs": {"ResponseResult": {"oneOf": [
                    {"properties": {"type": {"const": "pong"}}},
                    {"properties": {"type": {"const": "agent_list"}}}
                ]}}}
            }
        });
        assert_eq!(schema_protocol(&schema), Some(16));
        let methods = schema_methods(&schema);
        assert!(methods.contains(&"ping".to_string()));
        assert!(methods.contains(&"agent.list".to_string()));
        let types = schema_result_types(&schema);
        assert!(types.contains(&"pong".to_string()));
        assert!(types.contains(&"agent_list".to_string()));
    }
}
