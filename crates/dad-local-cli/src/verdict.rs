//! Probe verdict classification — the local port of `dad test`'s asymmetric
//! bias: a probe only passes when the response is a valid success payload or
//! a graceful DAD error the manifest makes sense of. Server/contract errors
//! are failures, because the probe only sends valid input to declared
//! methods — anything broken is the addon's own bug, never "an empty answer".

use crate::spawn::SpawnedResponse;
use dad_local_core::{
    allowed_stream_types_for_capabilities, validate_meta_response, validate_stream_items,
    validate_subtitle_items, DadCapability, DadErrorCode, DadStreamType,
};
use serde_json::Value;

/// One probe's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVerdict {
    pub ok: bool,
    pub detail: String,
    /// The probe returned real content (not just a graceful nothing).
    pub produced_data: bool,
    /// The probe was answered by a DECLARED api_key gate rejecting an
    /// anonymous request — healthy but nothing else was exercised.
    pub gate_only: bool,
}

impl ProbeVerdict {
    fn pass(detail: String, produced_data: bool, gate_only: bool) -> Self {
        ProbeVerdict { ok: true, detail, produced_data, gate_only }
    }
    fn fail(detail: String) -> Self {
        ProbeVerdict { ok: false, detail, produced_data: false, gate_only: false }
    }
}

/// Maps a declared capability to the RPC method the probe should call.
/// `direct_stream` and `torrent` share `getStreams` — probed once, never
/// twice with identical requests.
pub fn probe_methods(capabilities: &[DadCapability]) -> Vec<&'static str> {
    let mut methods = Vec::new();
    if capabilities
        .iter()
        .any(|c| matches!(c, DadCapability::DirectStream | DadCapability::Torrent))
    {
        methods.push("getStreams");
    }
    if capabilities.iter().any(|c| matches!(c, DadCapability::Meta)) {
        methods.push("getMeta");
    }
    if capabilities.iter().any(|c| matches!(c, DadCapability::Subtitle)) {
        methods.push("getSubtitles");
    }
    methods
}

fn response_has_data(method: &str, result: &Value) -> bool {
    match method {
        "getMeta" => result.as_object().map(|o| !o.is_empty()).unwrap_or(false),
        _ => result.as_array().map(|a| !a.is_empty()).unwrap_or(false),
    }
}

fn validate_success(method: &str, capabilities: &[DadCapability], result: &Value) -> Vec<String> {
    match method {
        "getStreams" => {
            let allowed: Vec<DadStreamType> =
                allowed_stream_types_for_capabilities(capabilities);
            validate_stream_items(result, Some(&allowed)).errors
        }
        "getMeta" => validate_meta_response(result).errors,
        "getSubtitles" => validate_subtitle_items(result).errors,
        _ => Vec::new(),
    }
}

/// Classifies one probe. `manifest_declares_gate` mirrors the manifest's
/// `api_key` field; `sent_key` says whether this probe carried `auth`.
pub fn classify(
    method: &str,
    capabilities: &[DadCapability],
    manifest_declares_gate: bool,
    sent_key: bool,
    spawned: &SpawnedResponse,
) -> ProbeVerdict {
    let Some(response) = &spawned.response else {
        return ProbeVerdict::fail(format!(
            "transport failure: {}",
            spawned
                .transport_error
                .as_deref()
                .unwrap_or("no response for an unknown reason")
        ));
    };

    if let Some(error) = response.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or_default();
        let message = error
            .get("error_message")
            .and_then(Value::as_str)
            .unwrap_or("(no error_message)");
        let Some(code) = dad_local_core::parse_error_code(code) else {
            return ProbeVerdict::fail(format!(
                "unmodeled error code '{code}' - a DAD addon answers only with the closed 9-code vocabulary"
            ));
        };
        return match code {
            DadErrorCode::Unauthorized => {
                if sent_key {
                    ProbeVerdict::fail(format!(
                        "key rejected: unauthorized - {message} (a --key was supplied but the addon rejected it)"
                    ))
                } else if manifest_declares_gate {
                    ProbeVerdict::pass(
                        format!("graceful error: unauthorized - {message} (declared api_key gate is working)"),
                        false,
                        true,
                    )
                } else {
                    ProbeVerdict::fail(format!(
                        "returned unauthorized but the manifest declares no 'api_key' gate - {message}. \
                         Declare the gate in manifest.json, or stop rejecting anonymous requests."
                    ))
                }
            }
            code if code.is_graceful() => {
                ProbeVerdict::pass(format!("graceful error: {} - {message}", code.code_json_name()), false, false)
            }
            code => ProbeVerdict::fail(format!(
                "server/contract error: {} - {message} (the probe only sends valid input to \
                 declared methods, so this is an addon bug)",
                code.code_json_name()
            )),
        };
    }

    let Some(result) = response.get("result") else {
        return ProbeVerdict::fail(
            "response carries neither 'result' nor 'error' - not a valid envelope".to_string(),
        );
    };

    let violations = validate_success(method, capabilities, result);
    if !violations.is_empty() {
        return ProbeVerdict::fail(format!("invalid response: {}", violations.join("; ")));
    }
    let produced = response_has_data(method, result);
    ProbeVerdict::pass(
        if produced {
            "valid response".to_string()
        } else {
            "valid empty result".to_string()
        },
        produced,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps() -> Vec<DadCapability> {
        vec![DadCapability::DirectStream, DadCapability::Torrent, DadCapability::Meta, DadCapability::Subtitle]
    }

    fn spawned(value: Value) -> SpawnedResponse {
        SpawnedResponse { response: Some(value), transport_error: None }
    }

    fn transport(message: &str) -> SpawnedResponse {
        SpawnedResponse { response: None, transport_error: Some(message.to_string()) }
    }

    #[test]
    fn probe_methods_dedupe_streams() {
        let methods = probe_methods(&caps());
        assert_eq!(methods, vec!["getStreams", "getMeta", "getSubtitles"]);
        assert!(probe_methods(&[]).is_empty());
    }

    #[test]
    fn valid_data_passes() {
        let verdict = classify(
            "getStreams",
            &caps(),
            false,
            false,
            &spawned(json!({"jsonrpc": "2.0", "id": 1, "result": [
                {"type": "direct", "title": "x", "stream_url": "https://c.example.com/a.mp4", "audio_languages": []}
            ]})),
        );
        assert!(verdict.ok && verdict.produced_data && !verdict.gate_only, "{verdict:?}");
    }

    #[test]
    fn graceful_errors_pass_without_data() {
        for code in ["content_unavailable", "rate_limited"] {
            let verdict = classify(
                "getStreams",
                &caps(),
                false,
                false,
                &spawned(json!({"error": {"code": code, "error_message": "nothing"}})),
            );
            assert!(verdict.ok && !verdict.produced_data, "{code}: {verdict:?}");
        }
    }

    #[test]
    fn server_contract_errors_fail() {
        for code in [
            "bad_request", "method_not_allowed", "not_found", "invalid_response",
            "upstream_unreachable", "internal_error",
        ] {
            let verdict = classify(
                "getStreams",
                &caps(),
                false,
                false,
                &spawned(json!({"error": {"code": code, "error_message": "broken"}})),
            );
            assert!(!verdict.ok, "{code} must fail: {verdict:?}");
        }
    }

    #[test]
    fn unauthorized_depends_on_gate_and_key() {
        let gated_no_key = classify(
            "getStreams", &caps(), true, false,
            &spawned(json!({"error": {"code": "unauthorized", "error_message": "need key"}})),
        );
        assert!(gated_no_key.ok && gated_no_key.gate_only);

        let gated_with_key = classify(
            "getStreams", &caps(), true, true,
            &spawned(json!({"error": {"code": "unauthorized", "error_message": "need key"}})),
        );
        assert!(!gated_with_key.ok);

        let ungated = classify(
            "getStreams", &caps(), false, false,
            &spawned(json!({"error": {"code": "unauthorized", "error_message": "need key"}})),
        );
        assert!(!ungated.ok);
        assert!(ungated.detail.contains("declares no 'api_key' gate"));
    }

    #[test]
    fn unmodeled_codes_and_transport_failures_fail() {
        let verdict = classify(
            "getStreams", &caps(), false, false,
            &spawned(json!({"error": {"code": "server_unreachable", "error_message": "typo"}})),
        );
        assert!(!verdict.ok && verdict.detail.contains("unmodeled"));

        let verdict = classify("getStreams", &caps(), false, false, &transport("timed out"));
        assert!(!verdict.ok && verdict.detail.contains("transport failure"));
    }

    #[test]
    fn invalid_success_payloads_fail_even_from_the_sdk() {
        let verdict = classify(
            "getStreams",
            &caps(),
            false,
            false,
            &spawned(json!({"jsonrpc": "2.0", "id": 1, "result": [
                {"type": "direct", "title": "x", "stream_url": "http://not-https.example.com/a.mp4", "audio_languages": []}
            ]})),
        );
        assert!(!verdict.ok && verdict.detail.contains("invalid response"));
    }
}
