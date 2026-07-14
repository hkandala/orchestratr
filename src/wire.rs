//! The orcr socket wire protocol (spec §11.6).
//!
//! orcr's server speaks its own newline-delimited JSON protocol over a Unix socket — the
//! same *shape* as herdr's, but a distinct protocol version. Requests are
//! `{protocol, id, method, params}`; responses correlate by `id` as
//! `{id, ok:true, result}` / `{id, ok:false, error:{code,message,details}}`; subscription
//! events interleave as `{subscription, seq, event}`.
//!
//! This module is the single source of truth for the envelope shapes, the framing
//! (newline-delimited, max-frame-size enforced), and version negotiation. It is
//! deliberately transport-agnostic: [`read_frame`]/[`write_frame`] work over any
//! `BufRead`/`Write`, so the same code serves the server, the client, and tests.

use crate::error::{ErrorCode, OrcrError};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// orcr's own socket protocol version (distinct from herdr's protocol 16). Bumped only on
/// a breaking change to the envelope/method contract; the SDK and CLI negotiate against it.
pub const ORCR_PROTOCOL: u32 = 1;

/// Maximum size of a single wire frame (request or response line), in bytes. Guards the
/// server against a client streaming an unbounded line, and the client against a runaway
/// server response.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// A decoded request envelope. `id` is echoed verbatim into the response so a multiplexed
/// client can correlate; `params` defaults to `{}` when omitted. Unknown top-level fields
/// are ignored (additive evolution, §11.6).
#[derive(Debug, Clone)]
pub struct Request {
    /// The protocol version the client declares. Absent = 0 (fails negotiation).
    pub protocol: u32,
    /// Opaque correlation id, echoed into the response. Any JSON value.
    pub id: Value,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// Parse a request from one decoded frame. Malformed JSON or a missing `method` is an
    /// `invalid_request`.
    pub fn from_slice(bytes: &[u8]) -> Result<Request, OrcrError> {
        let v: Value = serde_json::from_slice(bytes).map_err(|e| {
            OrcrError::invalid_request(format!("request is not valid JSON: {e}"), "bad_json")
        })?;
        let obj = v.as_object().ok_or_else(|| {
            OrcrError::invalid_request("request must be a JSON object", "bad_json")
        })?;
        let method = obj
            .get("method")
            .and_then(|m| m.as_str())
            .ok_or_else(|| {
                OrcrError::invalid_request("request is missing `method`", "missing_method")
            })?
            .to_string();
        let protocol = obj.get("protocol").and_then(|p| p.as_u64()).unwrap_or(0) as u32;
        let id = obj.get("id").cloned().unwrap_or(Value::Null);
        let params = obj.get("params").cloned().unwrap_or_else(|| json!({}));
        Ok(Request {
            protocol,
            id,
            method,
            params,
        })
    }
}

/// Build a success response envelope for a given request id.
pub fn ok_response(id: &Value, result: Value) -> Value {
    json!({ "id": id, "ok": true, "result": result })
}

/// Build an error response envelope for a given request id.
pub fn err_response(id: &Value, err: &OrcrError) -> Value {
    let mut e = json!({ "code": err.code.as_str(), "message": err.message });
    if !err.details.is_null() {
        e["details"] = err.details.clone();
    }
    json!({ "id": id, "ok": false, "error": e })
}

/// Build a subscription event frame.
pub fn event_frame(subscription: &str, seq: i64, event: Value) -> Value {
    json!({ "subscription": subscription, "seq": seq, "event": event })
}

/// The `unsupported_version` error for a rejected handshake (§11.6).
pub fn unsupported_version(declared: u32) -> OrcrError {
    OrcrError::new(
        ErrorCode::EnvironmentError,
        format!(
            "client declared socket protocol {declared} but this orcr server speaks {ORCR_PROTOCOL}"
        ),
    )
    .with_details(json!({
        "cause": "unsupported_version",
        "declared_protocol": declared,
        "server_protocol": ORCR_PROTOCOL,
    }))
}

/// Read one newline-delimited frame from `reader`, enforcing [`MAX_FRAME`]. Returns
/// `Ok(None)` at clean EOF (peer closed with no partial frame). Blank lines are skipped.
///
/// Uses `fill_buf`/`consume` so bytes past the newline stay buffered for the next frame,
/// and so the size cap is enforced *before* an oversized line is fully buffered (a
/// misbehaving peer can't force an unbounded allocation).
pub fn read_frame<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>, OrcrError> {
    loop {
        let mut buf: Vec<u8> = Vec::new();
        let complete = loop {
            let available = reader.fill_buf().map_err(io_err)?;
            if available.is_empty() {
                // EOF.
                break false;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..pos]);
                    reader.consume(pos + 1); // consume through the newline
                    break true;
                }
                None => {
                    buf.extend_from_slice(available);
                    let consumed = available.len();
                    reader.consume(consumed);
                    if buf.len() > MAX_FRAME {
                        return Err(frame_too_large());
                    }
                }
            }
            if buf.len() > MAX_FRAME {
                return Err(frame_too_large());
            }
        };
        if !complete {
            if buf.is_empty() {
                return Ok(None);
            }
            return Err(OrcrError::invalid_request(
                "connection closed mid-frame",
                "truncated_frame",
            ));
        }
        if buf.len() > MAX_FRAME {
            return Err(frame_too_large());
        }
        // Skip blank lines.
        if buf.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        return Ok(Some(buf));
    }
}

/// Write one JSON value as a newline-terminated frame and flush.
pub fn write_frame<W: Write>(writer: &mut W, value: &Value) -> Result<(), OrcrError> {
    let mut line = serde_json::to_vec(value).map_err(|e| {
        OrcrError::server_error("encode", format!("failed to encode wire frame: {e}"))
    })?;
    line.push(b'\n');
    writer.write_all(&line).map_err(io_err)?;
    writer.flush().map_err(io_err)?;
    Ok(())
}

fn frame_too_large() -> OrcrError {
    OrcrError::invalid_request(
        format!("frame exceeds the maximum size of {MAX_FRAME} bytes"),
        "frame_too_large",
    )
}

fn io_err(e: std::io::Error) -> OrcrError {
    OrcrError::new(ErrorCode::ServerError, format!("socket io error: {e}"))
        .with_details(json!({ "cause": "socket_io" }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn parses_request_defaults() {
        let r =
            Request::from_slice(br#"{"protocol":1,"id":"x","method":"server.status"}"#).unwrap();
        assert_eq!(r.protocol, 1);
        assert_eq!(r.id, json!("x"));
        assert_eq!(r.method, "server.status");
        assert_eq!(r.params, json!({}));
    }

    #[test]
    fn missing_method_is_invalid() {
        let e = Request::from_slice(br#"{"id":1}"#).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidRequest);
        assert_eq!(e.details["reason"], "missing_method");
    }

    #[test]
    fn absent_protocol_defaults_zero() {
        let r = Request::from_slice(br#"{"method":"m"}"#).unwrap();
        assert_eq!(r.protocol, 0);
    }

    #[test]
    fn ignores_unknown_fields() {
        let r = Request::from_slice(br#"{"protocol":1,"method":"m","surprise":42}"#).unwrap();
        assert_eq!(r.method, "m");
    }

    #[test]
    fn envelopes_shape() {
        let ok = ok_response(&json!("id1"), json!({"a":1}));
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["id"], "id1");
        assert_eq!(ok["result"]["a"], 1);

        let e = OrcrError::not_found("nope");
        let er = err_response(&json!(7), &e);
        assert_eq!(er["ok"], false);
        assert_eq!(er["id"], 7);
        assert_eq!(er["error"]["code"], "not_found");
    }

    #[test]
    fn frame_round_trip() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(&mut out, &json!({"hello":"world"})).unwrap();
        assert!(out.ends_with(b"\n"));
        let mut r = BufReader::new(&out[..]);
        let frame = read_frame(&mut r).unwrap().unwrap();
        let v: Value = serde_json::from_slice(&frame).unwrap();
        assert_eq!(v["hello"], "world");
        // Second read → clean EOF.
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn multiple_frames_in_stream() {
        let mut out: Vec<u8> = Vec::new();
        write_frame(&mut out, &json!({"n":1})).unwrap();
        write_frame(&mut out, &json!({"n":2})).unwrap();
        let mut r = BufReader::new(&out[..]);
        let f1: Value = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        let f2: Value = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(f1["n"], 1);
        assert_eq!(f2["n"], 2);
        assert!(read_frame(&mut r).unwrap().is_none());
    }

    #[test]
    fn blank_lines_skipped() {
        let data = b"\n\n{\"n\":5}\n";
        let mut r = BufReader::new(&data[..]);
        let f: Value = serde_json::from_slice(&read_frame(&mut r).unwrap().unwrap()).unwrap();
        assert_eq!(f["n"], 5);
    }
}
