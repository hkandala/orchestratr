//! The socket method registry and the self-describing schema.
//!
//! **The socket API is the API.** The full method namespace is registered here in M1 with
//! typed param/result schemas; later milestones replace the `unimplemented` stub handlers
//! but never change this contract without a protocol bump. `orcr api schema` is generated
//! from this registry — never hand-written — so the CLI, the TS SDK, and any other client
//! derive from one source and cannot drift.

use crate::error::ErrorCode;
use crate::wire::ORCR_PROTOCOL;
use serde_json::{json, Value};

/// A registered socket method: its name, a one-line summary, JSON-Schema fragments for its
/// params and result, and whether it is live in the current build (a `false` handler
/// returns `server_error {cause: unimplemented}`).
#[derive(Debug, Clone)]
pub struct MethodDef {
    pub name: &'static str,
    pub summary: &'static str,
    /// JSON Schema for the params object.
    pub params: Value,
    /// JSON Schema for the result object.
    pub result: Value,
    /// True once a real handler exists; false = stub in this build.
    pub implemented: bool,
    /// True for subscription-style methods that stream `{subscription,seq,event}` frames
    /// after the initial response.
    pub streaming: bool,
}

/// The event kinds a subscriber may receive. Names are stable; the payload
/// schemas grow as producers land in later milestones.
pub const EVENT_KINDS: &[&str] = &[
    "agent.created",
    "agent.status_changed",
    "agent.turn_completed",
    "agent.response_captured",
    "agent.location_changed",
    "agent.ended",
    // Queue membership is derived by subscribers from agent.created / agent.ended /
    // queue.promoted — there is no separate queue.changed event.
    "queue.promoted",
    "attach.started",
    "attach.ended",
    "loop.created",
    "loop.fired",
    "loop.coalesced",
    "loop.skipped",
    "loop.paused",
    "loop.resumed",
    "loop.removed",
    "loop.ended",
    "loop_run.started",
    "loop_run.ended",
    "loop_run.stopping",
    // Control frame emitted to every open subscription on graceful shutdown.
    "server_stopping",
];

/// The stable error codes with their process exit codes, for the schema. Derived
/// from [`ErrorCode`] so the wire strings and exit codes have a single source of truth.
pub fn error_codes() -> Vec<(&'static str, i32)> {
    ErrorCode::ALL
        .iter()
        .map(|c| (c.as_str(), c.exit_code()))
        .collect()
}

/// A permissive object schema (any properties allowed) — used where a shape is not yet
/// nailed down (stub methods land their real schemas with their handlers).
fn any_object() -> Value {
    json!({ "type": "object", "additionalProperties": true })
}

/// An object schema with the given properties; `additionalProperties` stays open for
/// additive evolution.
fn object(props: Value) -> Value {
    json!({ "type": "object", "properties": props, "additionalProperties": true })
}

fn str_schema() -> Value {
    json!({ "type": "string" })
}
fn int_schema() -> Value {
    json!({ "type": "integer" })
}
fn bool_schema() -> Value {
    json!({ "type": "boolean" })
}
fn array_of(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

/// The full method registry. Order is stable (registration order) so the generated schema
/// is deterministic.
pub fn methods() -> Vec<MethodDef> {
    let mut m: Vec<MethodDef> = Vec::new();

    let mut add = |name, summary, params, result, implemented, streaming| {
        m.push(MethodDef {
            name,
            summary,
            params,
            result,
            implemented,
            streaming,
        });
    };

    // --- server / api / events (live in M1) ---
    add(
        "server.handshake",
        "Readiness probe: returns pid, protocol, and store path.",
        any_object(),
        object(json!({
            "pid": int_schema(),
            "protocol": int_schema(),
            "store": str_schema(),
            "ready": bool_schema(),
        })),
        true,
        false,
    );
    add(
        "server.status",
        "Server health: version, protocol, paths, herdr reachability, counts, integrations.",
        any_object(),
        object(json!({
            "version": str_schema(),
            "protocol": int_schema(),
            "pid": int_schema(),
            "uptime_ms": int_schema(),
            "socket": str_schema(),
            "store": str_schema(),
            "herdr": any_object(),
            "integrations": any_object(),
            "counts": any_object(),
            "loops_firing": bool_schema(),
            "loops": array_of(any_object()),
            "drift": any_object(),
        })),
        true,
        false,
    );
    add(
        "server.stop",
        "Graceful control-plane stop: stop accepting, close subscriptions, release socket. Never touches panes.",
        any_object(),
        object(json!({ "status": str_schema() })),
        true,
        false,
    );
    add(
        "api.schema",
        "The versioned JSON Schema of the whole socket protocol.",
        any_object(),
        any_object(),
        true,
        false,
    );
    add(
        "api.snapshot",
        "One consistent runtime-state document stamped with snapshot_seq.",
        any_object(),
        object(json!({
            "snapshot_seq": int_schema(),
            "agents": array_of(any_object()),
            "loops": array_of(any_object()),
            "queue": array_of(any_object()),
        })),
        true,
        false,
    );
    add(
        "events.subscribe",
        "Subscribe to the event stream from since_seq; replay then live. Streams event frames.",
        object(json!({ "since_seq": int_schema() })),
        object(json!({ "subscription": str_schema(), "from_seq": int_schema() })),
        true,
        true,
    );
    add(
        "watch.open",
        "Snapshot + subscription under one cursor pin (no re-snapshot livelock). Streams event frames.",
        any_object(),
        object(json!({
            "subscription": str_schema(),
            "snapshot_seq": int_schema(),
            "snapshot": any_object(),
        })),
        true,
        true,
    );

    // --- agent namespace (stubs until M2/M3) ---
    add(
        "agent.run",
        "Enqueue a new managed agent; returns its path + uuid.",
        object(json!({
            "name": str_schema(), "path": str_schema(), "agent": str_schema(),
            "prompt": str_schema(), "gc": str_schema(), "model": str_schema(),
            "effort": str_schema(), "cwd": str_schema(), "timeout": str_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({ "agent": any_object(), "permissions": str_schema() })),
        true,
        false,
    );
    add(
        "agent.ask",
        "run --gc immediate → wait → last response, in one call.",
        object(json!({
            "name": str_schema(), "path": str_schema(), "agent": str_schema(),
            "prompt": str_schema(), "model": str_schema(), "effort": str_schema(),
            "cwd": str_schema(), "timeout": str_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({ "uuid": str_schema(), "path": str_schema(), "response": any_object() })),
        true,
        false,
    );
    add(
        "agent.send",
        "Deliver a prompt to an existing agent's TUI.",
        object(json!({
            "target": str_schema(), "prompt": str_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({
            "uuid": str_schema(), "path": str_schema(),
            "delivered_while": str_schema(), "input_seq": int_schema(),
        })),
        true,
        false,
    );
    add(
        "agent.logs",
        "Read an agent's native transcript (optionally only the last response).",
        object(json!({
            "target": str_schema(), "last_response": bool_schema(),
            "tail": int_schema(), "follow": bool_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({
            "uuid": str_schema(), "path": str_schema(),
            "resolved": str_schema(), "entries": array_of(any_object()),
            "response": any_object(),
        })),
        true,
        false,
    );
    add(
        "agent.wait",
        "Block until every target agent settles.",
        object(json!({
            "targets": array_of(str_schema()), "timeout": str_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({
            "targets": array_of(object(json!({
                "uuid": str_schema(), "path": str_schema(), "status": str_schema(),
                "ok": bool_schema(), "reason": str_schema(), "exit_reason": str_schema(),
                "next": object(json!({ "kind": str_schema(), "command": str_schema() })),
            }))),
            "all_ok": bool_schema(),
            "timed_out": bool_schema(), "decision_seq": int_schema(),
        })),
        true,
        false,
    );
    add(
        "agent.attach.prepare",
        "Insert an attach lease and return the herdr attach exec command.",
        object(json!({
            "target": str_schema(), "takeover": bool_schema(), "client_pid": int_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({
            "uuid": str_schema(), "path": str_schema(), "lease_id": str_schema(),
            "takeover": bool_schema(), "ttl_ms": int_schema(),
            "command": array_of(str_schema()),
        })),
        true,
        false,
    );
    add(
        "agent.attach.heartbeat",
        "Refresh an attach lease so GC keeps deferring while the terminal is attached.",
        object(json!({ "lease_id": str_schema() })),
        object(json!({ "ok": bool_schema() })),
        true,
        false,
    );
    add(
        "agent.attach.release",
        "Release an attach lease on detach (GC resumes).",
        object(json!({ "lease_id": str_schema() })),
        object(json!({ "released": bool_schema() })),
        true,
        false,
    );
    add(
        "agent.kill",
        "Kill matched agents (graceful per-integration shutdown; panes closed).",
        object(json!({
            "targets": array_of(str_schema()), "force": bool_schema(),
            "preview": bool_schema(), "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({
            "killed": array_of(any_object()), "skipped": array_of(any_object()),
            "all_killed": bool_schema(),
        })),
        true,
        false,
    );
    add(
        "agent.ls",
        "List active (and, with all, ended) agents as flat rows.",
        object(json!({
            "pattern": str_schema(), "agent": str_schema(), "status": str_schema(),
            "managed": bool_schema(), "unmanaged": bool_schema(), "all": bool_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        object(json!({ "agents": array_of(any_object()) })),
        true,
        false,
    );

    // --- loop namespace ---
    add(
        "loop.create",
        "Create a durable cron loop over an argv command.",
        object(json!({
            "name": str_schema(), "cron": str_schema(), "once_at": str_schema(),
            "max_concurrency": int_schema(), "overlap": str_schema(),
            "timeout": str_schema(), "command": array_of(str_schema()), "cwd": str_schema(),
        })),
        object(json!({ "loop": any_object() })),
        true,
        false,
    );
    add(
        "loop.pause",
        "Pause loop(s): no new fires.",
        object(json!({ "names": array_of(str_schema()) })),
        any_object(),
        true,
        false,
    );
    add(
        "loop.resume",
        "Resume paused loop(s).",
        object(json!({ "names": array_of(str_schema()) })),
        any_object(),
        true,
        false,
    );
    add(
        "loop.rm",
        "Remove loop(s); optionally kill active runs.",
        object(json!({
            "names": array_of(str_schema()), "kill_active": bool_schema(),
            "caller_id": str_schema(), "caller_path": str_schema(),
        })),
        any_object(),
        true,
        false,
    );
    add(
        "loop.ls",
        "List loops.",
        object(
            json!({ "names": array_of(str_schema()), "status": str_schema(), "all": bool_schema() }),
        ),
        object(json!({ "loops": array_of(any_object()) })),
        true,
        false,
    );
    add(
        "loop.logs",
        "Interleaved loop command output + scheduler actions.",
        object(json!({
            "name": str_schema(), "run": str_schema(), "source": str_schema(),
            "tail": int_schema(), "follow": bool_schema(),
        })),
        object(json!({ "lines": array_of(any_object()) })),
        true,
        false,
    );
    add(
        "loop.run.start",
        "Manually trigger a loop run.",
        object(json!({ "name": str_schema() })),
        object(json!({ "run": any_object() })),
        true,
        false,
    );
    add(
        "loop.run.stop",
        "Stop run(s) of a loop without touching the definition.",
        object(json!({ "name": str_schema(), "run": str_schema() })),
        object(json!({ "stopped": array_of(any_object()), "skipped": array_of(any_object()) })),
        true,
        false,
    );
    add(
        "loop.run.ls",
        "List a loop's runs.",
        object(json!({ "name": str_schema(), "status": str_schema(), "all": bool_schema() })),
        object(json!({ "runs": array_of(any_object()) })),
        true,
        false,
    );

    m
}

/// Look up a method by name.
pub fn find_method(name: &str) -> Option<MethodDef> {
    methods().into_iter().find(|m| m.name == name)
}

/// Generate the versioned JSON Schema document of the whole socket protocol.
/// The top-level object is itself a valid JSON Schema; `methods`/`events`/`errorCodes` are
/// descriptive extensions. Every registered method appears under `methods`.
pub fn schema_document() -> Value {
    let mut method_map = serde_json::Map::new();
    for m in methods() {
        method_map.insert(
            m.name.to_string(),
            json!({
                "summary": m.summary,
                "implemented": m.implemented,
                "streaming": m.streaming,
                "params": m.params,
                "result": m.result,
            }),
        );
    }

    let mut event_map = serde_json::Map::new();
    for k in EVENT_KINDS {
        event_map.insert(
            k.to_string(),
            json!({ "type": "object", "additionalProperties": true }),
        );
    }

    let error_codes: Vec<Value> = error_codes()
        .iter()
        .map(|(code, exit)| json!({ "code": code, "exit": exit }))
        .collect();

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "orchestratr socket protocol",
        "type": "object",
        "x-orcr": {
            "protocol": ORCR_PROTOCOL,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "envelope": {
            "request": { "type": "object", "properties": {
                "protocol": int_schema(), "id": {}, "method": str_schema(), "params": { "type": "object" }
            }, "required": ["method"] },
            "response": { "type": "object", "properties": {
                "id": {}, "ok": bool_schema(), "result": {}, "error": any_object()
            } },
            "event": { "type": "object", "properties": {
                "subscription": str_schema(), "seq": int_schema(), "event": any_object()
            } }
        },
        "methods": Value::Object(method_map),
        "events": Value::Object(event_map),
        "errorCodes": error_codes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_methods_are_registered() {
        for name in [
            "server.handshake",
            "server.status",
            "server.stop",
            "api.schema",
            "api.snapshot",
            "events.subscribe",
            "watch.open",
        ] {
            let m = find_method(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(m.implemented, "{name} should be implemented");
        }
    }

    #[test]
    fn full_namespace_registered() {
        for name in [
            "agent.run",
            "agent.ask",
            "agent.send",
            "agent.logs",
            "agent.wait",
            "agent.attach.prepare",
            "agent.kill",
            "agent.ls",
            "loop.create",
            "loop.pause",
            "loop.resume",
            "loop.rm",
            "loop.ls",
            "loop.logs",
            "loop.run.start",
            "loop.run.stop",
            "loop.run.ls",
        ] {
            assert!(find_method(name).is_some(), "{name} missing from registry");
        }
    }

    #[test]
    fn method_names_unique() {
        let ms = methods();
        let mut names: Vec<&str> = ms.iter().map(|m| m.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(n, names.len(), "duplicate method names in the registry");
    }

    #[test]
    fn schema_covers_every_method() {
        let doc = schema_document();
        let schema_methods = doc["methods"].as_object().unwrap();
        for m in methods() {
            assert!(
                schema_methods.contains_key(m.name),
                "schema missing method {}",
                m.name
            );
        }
        assert_eq!(schema_methods.len(), methods().len());
        assert_eq!(doc["x-orcr"]["protocol"], ORCR_PROTOCOL);
    }

    #[test]
    fn schema_lists_events_and_errors() {
        let doc = schema_document();
        assert_eq!(doc["events"].as_object().unwrap().len(), EVENT_KINDS.len());
        assert_eq!(
            doc["errorCodes"].as_array().unwrap().len(),
            error_codes().len()
        );
    }

    #[test]
    fn error_codes_match_error_module() {
        // The schema table is derived from ErrorCode, so every entry must agree with the
        // canonical exit_code() mapping in error.rs.
        for (code, exit) in error_codes() {
            let ec = ErrorCode::ALL
                .iter()
                .find(|c| c.as_str() == code)
                .unwrap_or_else(|| panic!("{code} not in ErrorCode::ALL"));
            assert_eq!(ec.exit_code(), exit, "exit code mismatch for {code}");
        }
    }
}
