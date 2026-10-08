//! The DAD error model — a closed set of 9 machine-readable codes in a single
//! response shape, ported 1:1 from dad-sdk 3.0.1.
//!
//! Every failure a DAD addon can emit is one of:
//!
//! ```json
//! { "error": "content_unavailable", "error_message": "No streams for this title" }
//! ```
//!
//! Both the SDK and addon authors use the SAME vocabulary, so the host (and
//! `dad-local test`) can always tell WHY a request failed without parsing
//! prose. In the local transport the error object rides inside the RPC
//! `result` — the HTTP status table is kept only as the canonical semantic
//! mapping the host may surface.
//!
//! Verdict split (mirrors `dad test` 3.0.1):
//! - **graceful** — `content_unavailable`, `rate_limited`, and `unauthorized`
//!   when a declared api_key gate rejected an anonymous call: the addon
//!   functioning, just with nothing to give. These PASS probes.
//! - **server/contract** — `bad_request`, `method_not_allowed`, `not_found`,
//!   `invalid_response`, `upstream_unreachable`, `internal_error`: the addon
//!   itself is broken. These FAIL probes.

use crate::validation::{json_type_name, Validation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Machine-readable error codes. The host treats these as a closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DadErrorCode {
    /// Malformed request (bad params, bad route shape).
    BadRequest,
    /// The transport disallows this method/verb.
    MethodNotAllowed,
    /// Missing or invalid API key.
    Unauthorized,
    /// Unknown method or undeclared capability.
    NotFound,
    /// Title known, but the addon has no content for it.
    ContentUnavailable,
    /// A response failed SDK validation.
    InvalidResponse,
    /// The addon's upstream source could not be reached.
    UpstreamUnreachable,
    /// The addon asks the caller to slow down.
    RateLimited,
    /// Unexpected addon crash / catch-all.
    InternalError,
}

impl DadErrorCode {
    /// The canonical HTTP status this code maps to (semantic twin of
    /// dad-sdk's `DAD_ERROR_STATUS`). The local transport carries no status
    /// line; the host may use this for user-facing mapping.
    pub const fn http_status(self) -> u16 {
        match self {
            DadErrorCode::BadRequest => 400,
            DadErrorCode::MethodNotAllowed => 405,
            DadErrorCode::Unauthorized => 401,
            DadErrorCode::NotFound => 404,
            DadErrorCode::ContentUnavailable => 404,
            DadErrorCode::InvalidResponse => 422,
            DadErrorCode::UpstreamUnreachable => 502,
            DadErrorCode::RateLimited => 429,
            DadErrorCode::InternalError => 500,
        }
    }

    /// True for the "graceful" codes — a functioning addon answering with
    /// nothing (see the module docs for the exact split).
    pub const fn is_graceful(self) -> bool {
        matches!(self, DadErrorCode::ContentUnavailable | DadErrorCode::RateLimited)
    }
}

/// Free-function twin of dad-sdk's `DAD_ERROR_STATUS` table, for callers that
/// work on raw codes coming off the wire.
pub const fn dad_error_status(code: DadErrorCode) -> u16 {
    code.http_status()
}

/// The error type addon handlers raise (the Rust `DadError`). The SDK's RPC
/// main converts a raised `DadError` into the exact wire shape above; any
/// other panic is caught and answered as `internal_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DadError {
    /// The machine code.
    pub code: DadErrorCode,
    /// Human-readable message (shown to the end user when appropriate).
    pub message: String,
}

impl DadError {
    /// Build an error from a code + message.
    pub fn new(code: DadErrorCode, message: impl Into<String>) -> Self {
        DadError { code, message: message.into() }
    }
}

impl fmt::Display for DadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code.code_json_name(), self.message)
    }
}

impl std::error::Error for DadError {}

impl DadErrorCode {
    /// The exact wire spelling of this code (same as its serde form).
    pub const fn code_json_name(self) -> &'static str {
        match self {
            DadErrorCode::BadRequest => "bad_request",
            DadErrorCode::MethodNotAllowed => "method_not_allowed",
            DadErrorCode::Unauthorized => "unauthorized",
            DadErrorCode::NotFound => "not_found",
            DadErrorCode::ContentUnavailable => "content_unavailable",
            DadErrorCode::InvalidResponse => "invalid_response",
            DadErrorCode::UpstreamUnreachable => "upstream_unreachable",
            DadErrorCode::RateLimited => "rate_limited",
            DadErrorCode::InternalError => "internal_error",
        }
    }
}

/// Parses a raw `error` string into a code, if it is in the closed vocabulary.
pub fn parse_error_code(raw: &str) -> Option<DadErrorCode> {
    Some(match raw {
        "bad_request" => DadErrorCode::BadRequest,
        "method_not_allowed" => DadErrorCode::MethodNotAllowed,
        "unauthorized" => DadErrorCode::Unauthorized,
        "not_found" => DadErrorCode::NotFound,
        "content_unavailable" => DadErrorCode::ContentUnavailable,
        "invalid_response" => DadErrorCode::InvalidResponse,
        "upstream_unreachable" => DadErrorCode::UpstreamUnreachable,
        "rate_limited" => DadErrorCode::RateLimited,
        "internal_error" => DadErrorCode::InternalError,
        _ => return None,
    })
}

/// Validates a value as a well-formed DAD error response: a known `error`
/// code and a non-empty string `error_message`.
pub fn validate_error_response(raw: &Value) -> Validation {
    let Some(obj) = raw.as_object() else {
        return Validation::error(format!(
            "Error response must be an object - got {}",
            json_type_name(raw)
        ));
    };
    let mut errors = Vec::new();
    match obj.get("error") {
        Some(Value::String(code)) => {
            if parse_error_code(code).is_none() {
                errors.push(format!(
                    "'error' must be a known DadErrorCode - got {code:?}, expected one of: \
                     bad_request, method_not_allowed, unauthorized, not_found, \
                     content_unavailable, invalid_response, upstream_unreachable, \
                     rate_limited, internal_error"
                ));
            }
        }
        Some(other) => errors.push(format!(
            "'error' must be a string code - got {}",
            json_type_name(other)
        )),
        None => errors.push("missing 'error' code".to_string()),
    }
    match obj.get("error_message") {
        Some(Value::String(msg)) if !msg.trim().is_empty() => {}
        Some(Value::Null) | None => errors.push("'error_message' must be a non-empty string".to_string()),
        Some(_) => errors.push("'error_message' must be a non-empty string".to_string()),
    }
    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

/// True when the value is a DAD error response with a KNOWN code
/// (`error_message` is not checked — use [`validate_error_response`] for that).
pub fn is_error_response(raw: &Value) -> bool {
    raw.as_object()
        .and_then(|o| o.get("error"))
        .and_then(Value::as_str)
        .and_then(parse_error_code)
        .is_some()
}

/// True when the value merely LOOKS like an error payload — an object with a
/// string `error` field — regardless of code/message validity.
///
/// This catches a handler that TRIED to return an error but got the shape
/// wrong (a typo'd code, a missing `error_message`, a serialized `Error`) so
/// it can be reported as exactly that, instead of being misread as a success
/// payload and answered with a 422 about its own error object.
pub fn looks_like_error_payload(raw: &Value) -> bool {
    raw.as_object()
        .and_then(|o| o.get("error"))
        .map(Value::is_string)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serde_uses_exact_wire_spelling() {
        assert_eq!(
            serde_json::to_value(DadErrorCode::ContentUnavailable).unwrap(),
            json!("content_unavailable")
        );
        assert_eq!(
            serde_json::from_value::<DadErrorCode>(json!("upstream_unreachable")).unwrap(),
            DadErrorCode::UpstreamUnreachable
        );
        assert!(serde_json::from_value::<DadErrorCode>(json!("server_unreachable")).is_err());
    }

    #[test]
    fn status_table() {
        assert_eq!(dad_error_status(DadErrorCode::BadRequest), 400);
        assert_eq!(dad_error_status(DadErrorCode::ContentUnavailable), 404);
        assert_eq!(dad_error_status(DadErrorCode::InvalidResponse), 422);
        assert_eq!(dad_error_status(DadErrorCode::UpstreamUnreachable), 502);
    }

    #[test]
    fn graceful_split() {
        assert!(DadErrorCode::ContentUnavailable.is_graceful());
        assert!(DadErrorCode::RateLimited.is_graceful());
        assert!(!DadErrorCode::InternalError.is_graceful());
        assert!(!DadErrorCode::Unauthorized.is_graceful()); // graceful only via the declared-gate rule
    }

    #[test]
    fn error_response_validation() {
        assert!(validate_error_response(&json!({
            "error": "content_unavailable", "error_message": "No streams"
        }))
        .valid);
        assert!(!validate_error_response(&json!({
            "error": "server_unreachable", "error_message": "typo'd code"
        }))
        .valid);
        assert!(!validate_error_response(&json!({
            "error": "internal_error", "error_message": "   "
        }))
        .valid);
        assert!(!validate_error_response(&json!({ "error": "internal_error" })).valid);
        assert!(!validate_error_response(&json!("boom")).valid);
    }

    #[test]
    fn lookalikes() {
        assert!(is_error_response(&json!({ "error": "not_found", "error_message": "x" })));
        assert!(!is_error_response(&json!({ "error": "not_a_code", "error_message": "x" })));
        assert!(looks_like_error_payload(&json!({ "error": "not_a_code" })));
        // TS semantics: only a STRING error field looks like an error payload.
        assert!(!looks_like_error_payload(&json!({ "error": 42 })));
        assert!(!looks_like_error_payload(&json!({ "err": "x" })));
        assert!(!looks_like_error_payload(&json!("plain string")));
    }
}
