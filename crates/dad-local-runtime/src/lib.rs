//! # dad-local-runtime
//!
//! The engine that lives INSIDE every DAD local addon binary. The addon
//! author writes handler traits; this crate owns everything else:
//!
//! - **one-shot execution model**: spawn → read ONE request line → dispatch →
//!   write ONE validated response line → exit. No daemon, no event loop —
//!   cancellation is a process kill, for free.
//! - **sole stdout ownership**: the runtime is the only writer to stdout;
//!   handler logging goes through the [`log!`] macro (stderr). Stray `println!`
//!   output degrades to ignored noise (the host skips non-matching lines) but
//!   is a contract violation.
//! - **envelope hardening**: max line length, strict `jsonrpc` +
//!   `protocol_version == "2.0"` checks, malformed input answered with a
//!   well-formed `bad_request` error (id `null` when unknowable).
//! - **enforced api_key gate**: `api_key.required` manifests reject keyless
//!   requests with `unauthorized` BEFORE the handler runs — the local twin of
//!   dad-sdk's enforced 401.
//! - **panic containment**: a handler panic never kills the protocol — it is
//!   caught and answered as `internal_error` (this is why `panic = "abort"`
//!   is a forbidden build profile).
//! - **response validation before emission**: handler output that fails the
//!   dad-local-core validators never leaves the process; the host receives a
//!   well-formed `invalid_response` error instead. An addon physically cannot
//!   speak a malformed stream/meta/subtitle payload.
//!
//! Wire envelope (protocol_version "2.0", snake_case):
//!
//! ```json
//! {"jsonrpc":"2.0","id":1,"protocol_version":"2.0","method":"getStreams",
//!  "params":{"tmdb_id":550,"media_type":"movie"}}
//! ```
//!
//! Responses: `{"jsonrpc":"2.0","id":1,"result":…}` or
//! `{"jsonrpc":"2.0","id":1,"error":{"code":"unauthorized","error_message":"…"}}`
//! — note the DAD error model (string code), not JSON-RPC's integer codes.

#![forbid(unsafe_code)]
#![allow(async_fn_in_trait)] // static dispatch only; the returned future is
                             // awaited on a current-thread runtime (no Send bound needed)

pub use dad_local_core::{
    DadError, DadErrorCode, DadRequest, HealthPong, LocalAddonManifest, MetaResponse, StreamItem,
    SubtitleItem, validate_health_pong,
};
use serde::Serialize;
use serde_json::Value;
use std::future::Future;
use std::io::{Read, Write};

pub use dad_local_macros::define_local_addon;

/// The protocol version this runtime speaks. Refused anything else.
pub const PROTOCOL_VERSION: &str = "2.0";

/// Hard cap on the request line. A legitimate request is well under 1 KiB;
/// anything larger is a protocol violation, not a request.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// Handler logging — stderr ONLY. The runtime owns stdout; a handler using
/// `println!` would corrupt the protocol channel (the host tolerates it by
/// skipping non-matching lines, but it is a contract violation).
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("[{}] {}", ::core::module_path!(), ::core::format_args!($($arg)*))
    };
}

/// Capability marker types — implemented by `define_local_addon!` ONLY for
/// capabilities the manifest actually declares. The handler traits require
/// them as supertraits, which is what turns "handler implemented but
/// capability undeclared" into a compile error.
pub mod caps {
    /// Marker for `direct_stream`/`torrent` (both map to `getStreams`).
    pub struct Streams;
    /// Marker for `meta`.
    pub struct Meta;
    /// Marker for `subtitle`.
    pub struct Subtitles;
}

/// Sealed-ish capability witness. Do not implement manually — the macro does.
pub trait HasCapability<C> {}

/// Streams handler. Required when the manifest declares `direct_stream` and
////or `torrent`; the response validator rejects items outside the declared
/// capabilities.
#[diagnostic::on_unimplemented(
    message = "`{Self}` must implement `GetStreamsHandler` - its manifest declares `direct_stream` and/or `torrent`",
    label = "implement `GetStreamsHandler` for this type, or remove the capability from manifest.json"
)]
pub trait GetStreamsHandler: HasCapability<caps::Streams> {
    /// Resolve playable streams for the request.
    async fn get_streams(&self, request: DadRequest) -> Result<Vec<StreamItem>, DadError>;
}

/// Meta handler. Required when the manifest declares `meta`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` must implement `GetMetaHandler` - its manifest declares `meta`",
    label = "implement `GetMetaHandler` for this type, or remove the capability from manifest.json"
)]
pub trait GetMetaHandler: HasCapability<caps::Meta> {
    /// Enrich the title with what TMDB does not carry. `Ok(None)` = nothing found.
    async fn get_meta(&self, request: DadRequest) -> Result<Option<MetaResponse>, DadError>;
}

/// Subtitles handler. Required when the manifest declares `subtitle`.
#[diagnostic::on_unimplemented(
    message = "`{Self}` must implement `GetSubtitlesHandler` - its manifest declares `subtitle`",
    label = "implement `GetSubtitlesHandler` for this type, or remove the capability from manifest.json"
)]
pub trait GetSubtitlesHandler: HasCapability<caps::Subtitles> {
    /// Resolve subtitle tracks for the request. Empty vec = none available.
    async fn get_subtitles(&self, request: DadRequest) -> Result<Vec<SubtitleItem>, DadError>;
}

/// Everything an addon author normally needs, one glob away.
pub mod prelude {
    pub use crate::{
        caps, define_local_addon, log, GetMetaHandler, GetStreamsHandler, GetSubtitlesHandler,
    };
    pub use dad_local_core::{
        DadError, DadErrorCode, DadRequest, DadStreamType, HealthPong, MediaType, MetaResponse,
        StreamItem, SubtitleFormat, SubtitleItem,
    };
}

// ============================================================================
// Envelope
// ============================================================================

/// A parsed, envelope-valid request.
#[derive(Debug, Clone)]
pub struct Incoming {
    /// Echoed back in the response. `None` when the request carried no id.
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// Why a request line never became an [`Incoming`]. These are protocol-level
/// failures — answered with a well-formed error response carrying `id: null`
/// (except [`ProtocolFailure::EmptyInput`], which means nobody is talking to
/// us at all).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolFailure {
    /// stdin closed with no input: the host sent nothing. No response is
    /// written; the generated main exits 1 with an explanation on stderr.
    EmptyInput,
    /// The request line exceeded [`MAX_REQUEST_BYTES`].
    LineTooLong,
    /// The line was not valid UTF-8/JSON.
    NotJson(String),
    /// Valid JSON, but not a valid protocol envelope.
    BadEnvelope(String),
}

impl ProtocolFailure {
    /// The DAD error this failure maps to (always `bad_request`).
    pub fn to_error(&self) -> DadError {
        let message = match self {
            ProtocolFailure::EmptyInput => "empty request".to_string(),
            ProtocolFailure::LineTooLong => format!(
                "request line exceeds the {}-byte protocol limit",
                MAX_REQUEST_BYTES
            ),
            ProtocolFailure::NotJson(detail) => format!("request is not valid JSON: {detail}"),
            ProtocolFailure::BadEnvelope(detail) => format!("invalid request envelope: {detail}"),
        };
        DadError::new(DadErrorCode::BadRequest, message)
    }
}

/// Reads one newline-terminated request line from stdin with the byte cap
/// enforced WHILE reading (never buffers unbounded input).
pub fn read_incoming() -> Result<Incoming, ProtocolFailure> {
    let stdin = std::io::stdin();
    let mut lock = stdin.lock();
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        let n = lock.read(&mut byte).map_err(|e| ProtocolFailure::NotJson(format!("stdin read failed: {e}")))?;
        if n == 0 {
            break;
        }
        if byte[0] == b'\n' {
            return parse_envelope(&buf);
        }
        buf.push(byte[0]);
        if buf.len() > MAX_REQUEST_BYTES {
            return Err(ProtocolFailure::LineTooLong);
        }
    }
    if buf.is_empty() {
        Err(ProtocolFailure::EmptyInput)
    } else {
        parse_envelope(&buf)
    }
}

/// Pure envelope parser — no I/O, fully unit-testable.
pub fn parse_envelope(bytes: &[u8]) -> Result<Incoming, ProtocolFailure> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| ProtocolFailure::NotJson(format!("not UTF-8: {e}")))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|e| ProtocolFailure::NotJson(format!("invalid JSON: {e}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| ProtocolFailure::BadEnvelope("request must be a JSON object".to_string()))?;

    match obj.get("jsonrpc") {
        Some(Value::String(v)) if v == "2.0" => {}
        Some(other) => {
            return Err(ProtocolFailure::BadEnvelope(format!(
                "'jsonrpc' must be \"2.0\" - got {other}"
            )))
        }
        None => return Err(ProtocolFailure::BadEnvelope("missing 'jsonrpc' field".to_string())),
    }
    match obj.get("protocol_version") {
        Some(Value::String(v)) if v == PROTOCOL_VERSION => {}
        Some(other) => {
            return Err(ProtocolFailure::BadEnvelope(format!(
                "unsupported 'protocol_version': {other} - this binary speaks {}",
                PROTOCOL_VERSION
            )))
        }
        None => {
            return Err(ProtocolFailure::BadEnvelope(
                "missing 'protocol_version' field".to_string(),
            ))
        }
    }
    let method = match obj.get("method") {
        Some(Value::String(m)) if !m.is_empty() => m.clone(),
        Some(other) => {
            return Err(ProtocolFailure::BadEnvelope(format!(
                "'method' must be a non-empty string - got {other}"
            )))
        }
        None => return Err(ProtocolFailure::BadEnvelope("missing 'method' field".to_string())),
    };
    let id = match obj.get("id") {
        None | Some(Value::Null) => None,
        Some(id @ (Value::Number(_) | Value::String(_))) => Some(id.clone()),
        Some(other) => {
            return Err(ProtocolFailure::BadEnvelope(format!(
                "'id' must be a number or string - got {other}"
            )))
        }
    };
    let params = match obj.get("params") {
        None | Some(Value::Null) => None,
        Some(params) => Some(params.clone()),
    };
    Ok(Incoming { id, method, params })
}

// ============================================================================
// Handler plumbing (called from the macro-generated dispatcher)
// ============================================================================

/// Parses + semantically validates `params` into a [`DadRequest`].
pub fn parse_request_params(params: Option<Value>) -> Result<DadRequest, DadError> {
    let params = params
        .ok_or_else(|| DadError::new(DadErrorCode::BadRequest, "missing 'params' object"))?;
    let request: DadRequest = serde_json::from_value(params).map_err(|e| {
        DadError::new(
            DadErrorCode::BadRequest,
            format!("invalid request params: {e}"),
        )
    })?;
    request.validate()?;
    Ok(request)
}

/// The enforced api_key gate — runs BEFORE the handler, exactly like
/// dad-sdk's enforced 401. `auth` presence is all the SDK checks; whether a
/// PRESENT key is valid is the addon author's business.
pub fn enforce_api_key_gate(manifest: &LocalAddonManifest, request: &DadRequest) -> Result<(), DadError> {
    if let Some(api_key) = &manifest.api_key {
        if api_key.required && request.auth.is_none() {
            return Err(DadError::new(
                DadErrorCode::Unauthorized,
                format!(
                    "This addon requires an API key - get one at {}",
                    api_key.page_url
                ),
            ));
        }
    }
    Ok(())
}

/// Serializes + validates the stream list against the manifest's DECLARED
/// capabilities. Invalid output NEVER leaves the process — it becomes an
/// `invalid_response` error the host understands.
pub fn validate_streams_or_invalid(
    items: &[StreamItem],
    manifest: &LocalAddonManifest,
) -> Result<Value, DadError> {
    let raw = to_json_or_internal(items)?;
    let allowed = dad_local_core::allowed_stream_types_for_capabilities(&manifest.capabilities);
    let check = dad_local_core::validate_stream_items(&raw, Some(&allowed));
    if !check.valid {
        return Err(DadError::new(
            DadErrorCode::InvalidResponse,
            format!("Invalid stream response: {check}"),
        ));
    }
    Ok(raw)
}

/// Same enforcement for meta responses.
pub fn validate_meta_or_invalid(meta: Option<MetaResponse>) -> Result<Value, DadError> {
    let raw = to_json_or_internal(&meta)?;
    let check = dad_local_core::validate_meta_response(&raw);
    if !check.valid {
        return Err(DadError::new(
            DadErrorCode::InvalidResponse,
            format!("Invalid meta response: {check}"),
        ));
    }
    Ok(raw)
}

/// Same enforcement for subtitle lists.
pub fn validate_subtitles_or_invalid(subs: &[SubtitleItem]) -> Result<Value, DadError> {
    let raw = to_json_or_internal(subs)?;
    let check = dad_local_core::validate_subtitle_items(&raw);
    if !check.valid {
        return Err(DadError::new(
            DadErrorCode::InvalidResponse,
            format!("Invalid subtitle response: {check}"),
        ));
    }
    Ok(raw)
}

fn to_json_or_internal<T: Serialize + ?Sized>(value: &T) -> Result<Value, DadError> {
    serde_json::to_value(value).map_err(|e| {
        DadError::new(
            DadErrorCode::InternalError,
            format!("response contained a value JSON cannot represent (e.g. NaN): {e}"),
        )
    })
}

/// Parses the embedded (compile-time-validated) manifest. Infallible in
/// practice — kept as a function so the failure mode is at least loud.
pub fn load_embedded_manifest(manifest_json: &str) -> LocalAddonManifest {
    let value: Value = serde_json::from_str(manifest_json)
        .expect("embedded manifest was validated at compile time and is valid JSON");
    LocalAddonManifest::parse(&value)
        .expect("embedded manifest was validated at compile time")
}

// ============================================================================
// The one-shot flow
// ============================================================================

/// A finished response, ready for [`write_response`].
#[derive(Debug, Clone)]
pub struct RpcResponse {
    /// Echoed request id; `None` for protocol failures.
    pub id: Option<Value>,
    /// The result payload, or the DAD error.
    pub outcome: Result<Value, DadError>,
}

impl RpcResponse {
    /// A protocol-failure response (`id: null`, `bad_request`).
    pub fn from_failure(failure: &ProtocolFailure) -> RpcResponse {
        RpcResponse { id: None, outcome: Err(failure.to_error()) }
    }
}

/// The heart of the one-shot model: takes a (possibly failed) incoming
/// request, runs the generated dispatcher on a current-thread tokio runtime
/// with panic containment, and returns the response.
pub fn handle<Fut>(
    incoming: Result<Incoming, ProtocolFailure>,
    _manifest: &LocalAddonManifest,
    dispatch: impl Fn(String, Option<Value>) -> Fut,
) -> RpcResponse
where
    Fut: Future<Output = Result<Value, DadError>>,
{
    let incoming = match incoming {
        Ok(incoming) => incoming,
        Err(failure) => return RpcResponse::from_failure(&failure),
    };

    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            return RpcResponse {
                id: incoming.id,
                outcome: Err(DadError::new(
                    DadErrorCode::InternalError,
                    format!("failed to start the async runtime: {e}"),
                )),
            };
        }
    };

    let future = dispatch(incoming.method.clone(), incoming.params.clone());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(future)
    }));

    match outcome {
        Ok(Ok(value)) => RpcResponse { id: incoming.id, outcome: Ok(value) },
        Ok(Err(err)) => RpcResponse { id: incoming.id, outcome: Err(err) },
        // The panic message is NOT echoed into the response: panic payloads
        // regularly carry internal details. It already went to stderr via
        // the default hook; the host gets a clean contract error.
        Err(_) => RpcResponse {
            id: incoming.id,
            outcome: Err(DadError::new(
                DadErrorCode::InternalError,
                "handler panicked - see the addon's stderr for details",
            )),
        },
    }
}

/// Writes the single response line to stdout. The ONLY stdout write in the
/// whole process.
pub fn write_response(response: &RpcResponse) {
    let wire = match &response.outcome {
        Ok(result) => WireResponse {
            jsonrpc: "2.0",
            id: response.id.as_ref(),
            result: Some(result),
            error: None,
        },
        Err(err) => WireResponse {
            jsonrpc: "2.0",
            id: response.id.as_ref(),
            result: None,
            error: Some(WireError {
                code: err.code,
                error_message: &err.message,
            }),
        },
    };
    let mut line = serde_json::to_string(&wire).expect("response is always serializable");
    line.push('\n');
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    let _ = lock.write_all(line.as_bytes());
    let _ = lock.flush();
}

#[derive(Serialize)]
struct WireResponse<'a> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<WireError<'a>>,
}

#[derive(Serialize)]
struct WireError<'a> {
    code: DadErrorCode,
    error_message: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_manifest() -> LocalAddonManifest {
        let raw = json!({
            "id": "org.example.demo-open",
            "name": "Demo Open",
            "version": "0.1.0",
            "type": "local",
            "protocol_version": "2.0",
            "capabilities": ["direct_stream", "torrent", "meta"],
            "platform_assets": {
                "windows-x64": {
                    "download_url": "https://example.com/demo.exe",
                    "binary_name": "demo.exe"
                }
            }
        });
        LocalAddonManifest::parse(&raw).expect("valid manifest")
    }

    fn envelope(method: &str, params: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 7, "protocol_version": "2.0",
            "method": method, "params": params
        }))
        .unwrap()
    }

    #[test]
    fn envelope_parsing() {
        let incoming = parse_envelope(&envelope("getStreams", json!({"tmdb_id": 1, "media_type": "movie"}))).unwrap();
        assert_eq!(incoming.method, "getStreams");
        assert_eq!(incoming.id, Some(json!(7)));

        // Missing jsonrpc / wrong protocol_version / non-object / bad id.
        assert!(matches!(parse_envelope(b"{}"), Err(ProtocolFailure::BadEnvelope(_))));
        assert!(parse_envelope(b"not json at all").is_err());
        let wrong_version = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": 1, "protocol_version": "1.0", "method": "x"
        })).unwrap();
        assert!(matches!(parse_envelope(&wrong_version), Err(ProtocolFailure::BadEnvelope(d)) if d.contains("protocol_version")));
        let bad_id = serde_json::to_vec(&json!({
            "jsonrpc": "2.0", "id": [1], "protocol_version": "2.0", "method": "x"
        })).unwrap();
        assert!(parse_envelope(&bad_id).is_err());
    }

    #[test]
    fn handle_panics_become_internal_error() {
        let manifest = valid_manifest();
        let response = handle(
            Ok(parse_envelope(&envelope("getStreams", json!({"tmdb_id": 1, "media_type": "movie"}))).unwrap()),
            &manifest,
            |_, _| async { panic!("boom - must never reach stdout raw") },
        );
        match response.outcome {
            Err(err) => {
                assert_eq!(err.code, DadErrorCode::InternalError);
                assert!(!err.message.contains("boom"), "panic payload must be redacted");
            }
            Ok(_) => panic!("expected an error"),
        }
        assert_eq!(response.id, Some(json!(7)));
    }

    #[test]
    fn handle_maps_dispatch_errors_verbatim() {
        let manifest = valid_manifest();
        let response = handle(
            Ok(parse_envelope(&envelope("getStreams", json!({"tmdb_id": 1, "media_type": "movie"}))).unwrap()),
            &manifest,
            |_, _| async {
                Err(DadError::new(DadErrorCode::ContentUnavailable, "Nothing here"))
            },
        );
        assert_eq!(
            response.outcome.err().unwrap().code,
            DadErrorCode::ContentUnavailable
        );
    }

    #[test]
    fn api_key_gate_runs_before_handlers() {
        let raw = json!({
            "id": "org.example.demo-gated",
            "name": "Demo Gated",
            "version": "0.1.0",
            "type": "local",
            "protocol_version": "2.0",
            "capabilities": ["subtitle"],
            "api_key": { "required": true, "page_url": "https://example.com/get-key" },
            "platform_assets": {
                "windows-x64": { "download_url": "https://example.com/demo.exe", "binary_name": "demo.exe" }
            }
        });
        let manifest = LocalAddonManifest::parse(&raw).unwrap();

        let no_auth = DadRequest {
            tmdb_id: 1, media_type: dad_local_core::MediaType::Tv, s: None, e: None, auth: None,
        };
        let err = enforce_api_key_gate(&manifest, &no_auth).unwrap_err();
        assert_eq!(err.code, DadErrorCode::Unauthorized);
        assert!(err.message.contains("get one at"));

        let with_auth = DadRequest {
            tmdb_id: 1, media_type: dad_local_core::MediaType::Tv, s: None, e: None,
            auth: Some("k".to_string()),
        };
        assert!(enforce_api_key_gate(&manifest, &with_auth).is_ok());
    }

    #[test]
    fn response_wire_shape() {
        let response = RpcResponse {
            id: Some(json!(5)),
            outcome: Ok(json!({"ok": true})),
        };
        // Round-trip through the exact serialization write_response uses.
        let wire = serde_json::to_string(&WireResponse {
            jsonrpc: "2.0",
            id: response.id.as_ref(),
            result: Some(&json!({"ok": true})),
            error: None,
        })
        .unwrap();
        let parsed: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed["jsonrpc"], json!("2.0"));
        assert_eq!(parsed["id"], json!(5));
        assert_eq!(parsed["result"]["ok"], json!(true));
        assert!(parsed.get("error").is_none());

        let _err_response = RpcResponse {
            id: None,
            outcome: Err(DadError::new(DadErrorCode::Unauthorized, "no key")),
        };
        let wire = serde_json::to_string(&WireResponse {
            jsonrpc: "2.0",
            id: None,
            result: None,
            error: Some(WireError { code: DadErrorCode::Unauthorized, error_message: "no key" }),
        })
        .unwrap();
        let parsed: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed["error"]["code"], json!("unauthorized"));
        assert_eq!(parsed["error"]["error_message"], json!("no key"));
    }

    #[test]
    fn params_validation_errors() {
        // Missing params entirely.
        assert_eq!(
            parse_request_params(None).unwrap_err().code,
            DadErrorCode::BadRequest
        );
        // Movie with a season - bad_request, never forwarded.
        let err = parse_request_params(Some(json!({"tmdb_id": 550, "media_type": "movie", "s": 1}))).unwrap_err();
        assert_eq!(err.code, DadErrorCode::BadRequest);
        assert!(err.message.contains("TV-only"));
        // tmdb_id 0.
        assert!(parse_request_params(Some(json!({"tmdb_id": 0, "media_type": "movie"}))).is_err());
        // camelCase params rejected (deny_unknown_fields / missing required).
        assert!(parse_request_params(Some(json!({"tmdbId": 550, "mediaType": "movie"}))).is_err());
    }
}
