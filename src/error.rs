//! The orcr error model (spec §13).
//!
//! A deliberately small, stable enum of nine error codes; everything finer lives in
//! `details`. Every code maps to a fixed process exit code (spec §6) and serializes to
//! the JSON error envelope `{"ok":false,"error":{code,message,details}}`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;

/// The nine stable error codes (spec §13). `details.cause` / `details.reason` carry the
/// finer classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    InvalidRequest,
    StateConflict,
    Blocked,
    Timeout,
    IntegrationMissing,
    TranscriptUnavailable,
    EnvironmentError,
    ServerError,
}

impl ErrorCode {
    /// The stable wire string for this code.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::NotFound => "not_found",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::StateConflict => "state_conflict",
            ErrorCode::Blocked => "blocked",
            ErrorCode::Timeout => "timeout",
            ErrorCode::IntegrationMissing => "integration_missing",
            ErrorCode::TranscriptUnavailable => "transcript_unavailable",
            ErrorCode::EnvironmentError => "environment_error",
            ErrorCode::ServerError => "server_error",
        }
    }

    /// The process exit code for this error (spec §6):
    /// `2` environment · `3` timeout · `4` blocked · `5` killed/ended · `6` not found ·
    /// `7` state conflict · `1` other.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::NotFound => 6,
            ErrorCode::StateConflict => 7,
            ErrorCode::Blocked => 4,
            ErrorCode::Timeout => 3,
            ErrorCode::IntegrationMissing => 2,
            ErrorCode::EnvironmentError => 2,
            // invalid_request, transcript_unavailable, server_error → 1
            ErrorCode::InvalidRequest
            | ErrorCode::TranscriptUnavailable
            | ErrorCode::ServerError => 1,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A structured orcr error: a stable code, a human message, and free-form `details`.
#[derive(Debug, Clone)]
pub struct OrcrError {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
}

impl OrcrError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        OrcrError {
            code,
            message: message.into(),
            details: Value::Null,
        }
    }

    /// Attach a `details` object.
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    /// The `{"ok":false,"error":{code,message,details}}` envelope for `--json` output.
    pub fn to_envelope(&self) -> Value {
        let mut err = json!({
            "code": self.code.as_str(),
            "message": self.message,
        });
        if !self.details.is_null() {
            err["details"] = self.details.clone();
        }
        json!({ "ok": false, "error": err })
    }

    // --- Constructors for the common cases (keep call sites terse) ---

    pub fn not_found(message: impl Into<String>) -> Self {
        OrcrError::new(ErrorCode::NotFound, message)
    }

    pub fn invalid_request(message: impl Into<String>, reason: &str) -> Self {
        OrcrError::new(ErrorCode::InvalidRequest, message).with_details(json!({ "reason": reason }))
    }

    pub fn state_conflict(message: impl Into<String>) -> Self {
        OrcrError::new(ErrorCode::StateConflict, message)
    }

    /// An environment problem (server/store/herdr/home/platform/version).
    /// `cause` is one of the documented values (spec §13):
    /// `herdr_unreachable`, `server_start_failed`, `store_locked`, `config_invalid`,
    /// `unsafe_home`, `unsupported_platform`, `unsupported_version`, ...
    pub fn environment(cause: &str, message: impl Into<String>) -> Self {
        OrcrError::new(ErrorCode::EnvironmentError, message).with_details(json!({ "cause": cause }))
    }

    pub fn server_error(cause: &str, message: impl Into<String>) -> Self {
        OrcrError::new(ErrorCode::ServerError, message).with_details(json!({ "cause": cause }))
    }
}

impl fmt::Display for OrcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if !self.details.is_null() {
            write!(f, " ({})", self.details)?;
        }
        Ok(())
    }
}

impl std::error::Error for OrcrError {}

/// Crate-wide result type.
pub type Result<T> = std::result::Result<T, OrcrError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_spec() {
        assert_eq!(ErrorCode::NotFound.exit_code(), 6);
        assert_eq!(ErrorCode::StateConflict.exit_code(), 7);
        assert_eq!(ErrorCode::Blocked.exit_code(), 4);
        assert_eq!(ErrorCode::Timeout.exit_code(), 3);
        assert_eq!(ErrorCode::IntegrationMissing.exit_code(), 2);
        assert_eq!(ErrorCode::EnvironmentError.exit_code(), 2);
        assert_eq!(ErrorCode::InvalidRequest.exit_code(), 1);
        assert_eq!(ErrorCode::TranscriptUnavailable.exit_code(), 1);
        assert_eq!(ErrorCode::ServerError.exit_code(), 1);
    }

    #[test]
    fn wire_strings_are_stable() {
        assert_eq!(ErrorCode::EnvironmentError.as_str(), "environment_error");
        assert_eq!(
            ErrorCode::IntegrationMissing.as_str(),
            "integration_missing"
        );
        assert_eq!(
            ErrorCode::TranscriptUnavailable.as_str(),
            "transcript_unavailable"
        );
    }

    #[test]
    fn envelope_shape() {
        let e = OrcrError::environment("unsafe_home", "home is world-writable");
        let env = e.to_envelope();
        assert_eq!(env["ok"], false);
        assert_eq!(env["error"]["code"], "environment_error");
        assert_eq!(env["error"]["message"], "home is world-writable");
        assert_eq!(env["error"]["details"]["cause"], "unsafe_home");
    }

    #[test]
    fn envelope_omits_null_details() {
        let e = OrcrError::not_found("nope");
        let env = e.to_envelope();
        assert!(env["error"].get("details").is_none());
    }
}
