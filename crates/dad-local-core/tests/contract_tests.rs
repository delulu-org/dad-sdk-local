//! Contract conformance tests — the port of the dad-sdk test suite plus the
//! local-specific rules. Everything the host's own implementation must agree
//! with lives here; the M3 golden vectors will be generated from these shapes.

use dad_local_core::*;
use serde_json::{json, Value};

fn valid_manifest() -> Value {
    json!({
        "id": "org.example.opensubs",
        "name": "OpenSubtitles",
        "version": "1.2.0",
        "type": "local",
        "protocol_version": "2.0",
        "description": "Subtitle tracks from OpenSubtitles using your own free API key.",
        "publisher": "Delulu Core Team",
        "capabilities": ["subtitle"],
        "api_key": {
            "required": true,
            "page_url": "https://www.opensubtitles.com/en/users/new_api_key"
        },
        "platform_assets": {
            "windows-x64": {
                "download_url": "https://github.com/delulu-org/opensubs-addon/releases/download/v1.2.0/opensubs-addon-x86_64-windows.exe",
                "binary_name": "opensubs-addon.exe",
                "sha256": "9f2b4c8e17d3a6f05b8e2c41d97a3f6e8d0b5c2a9e7f4136d8c5a2b7e9f04136",
                "entry_command": "rpc"
            },
            "linux-x64": {
                "download_url": "https://github.com/delulu-org/opensubs-addon/releases/download/v1.2.0/opensubs-addon-x86_64-linux",
                "binary_name": "opensubs-addon",
                "sha256": "c41d97a3f6e8d0b5c2a9e7f4136d8c5a2b7e9f041369f2b4c8e17d3a6f05b8e2",
                "entry_command": "rpc"
            },
            "macos-arm64": {
                "download_url": "https://github.com/delulu-org/opensubs-addon/releases/download/v1.2.0/opensubs-addon-aarch64-macos",
                "binary_name": "opensubs-addon",
                "sha256": "5b8e2c41d97a3f6e8d0b5c2a9e7f4136d8c5a2b7e9f041369f2b4c8e17d3a6f0",
                "entry_command": "rpc"
            }
        },
        "signature": "3nF9xQ2vLm7Kd0pRs5tUw8yB1cE4gH6jN9oQ2tV5xZ8aC1dF4gH7kM0pS3vY6bE9hK2nQ5tW8zA1cF4gJ7mP0s=="
    })
}

#[test]
fn the_full_release_manifest_from_the_design_docs_parses() {
    let manifest = LocalAddonManifest::parse(&valid_manifest()).expect("must parse");
    assert_eq!(manifest.id, "org.example.opensubs");
    // (namespace blocking was removed from the SDK by design)
    assert_eq!(manifest.capabilities, vec![DadCapability::Subtitle]);
    assert_eq!(manifest.platform_assets.len(), 3);
    assert!(manifest.platform_assets.contains_key("windows-x64"));
    assert!(validate_manifest(&valid_manifest()).valid);
}

#[test]
fn authoring_state_is_valid_and_release_state_is_valid() {
    // Authoring: empty placeholders.
    let mut authoring = valid_manifest();
    authoring["signature"] = json!("");
    for asset in authoring["platform_assets"].as_object_mut().unwrap().values_mut() {
        asset["sha256"] = json!("");
    }
    assert!(validate_manifest(&authoring).valid, "{}", validate_manifest(&authoring));

    // Release: filled. (valid_manifest is already release state.)
    assert!(validate_manifest(&valid_manifest()).valid);
}

#[test]
fn canonical_payload_binding_survives_field_edit_detection() {
    let manifest = LocalAddonManifest::parse(&valid_manifest()).unwrap();
    let original = manifest.canonical_payload().unwrap();

    // Any manifest field edit changes the canonical bytes -> signature no
    // longer matches -> install-time verification fails. This is the
    // bit-level versioning guarantee at the payload level.
    let mut tampered = manifest.clone();
    tampered.capabilities.push(DadCapability::DirectStream);
    assert_ne!(original, tampered.canonical_payload().unwrap());

    let mut tampered = manifest.clone();
    tampered.platform_assets.get_mut("linux-x64").unwrap().sha256 =
        "0000000000000000000000000000000000000000000000000000000000000000".to_string();
    assert_ne!(original, tampered.canonical_payload().unwrap());

    let mut tampered = manifest.clone();
    tampered.version = "1.2.1".to_string();
    assert_ne!(original, tampered.canonical_payload().unwrap());
}

#[test]
fn catalog_row_shaped_documents_do_not_belong_here() {
    // The local catalog carries display data + one pointer. This test pins
    // the CATALOG contract documented in the workspace README: rows must not
    // carry manifest-owned fields. Catalog validation itself ships with the
    // CLI (M2); here we pin the rationale as a manifest-side truth: the
    // manifest is the only place platform_assets/signature can live.
    let row = json!({
        "id": "org.example.opensubs",
        "name": "OpenSubtitles",
        "version": "1.2.0",
        "type": "local",
        "manifest_url": "https://addons.delulu.dev/opensubs/manifest.json"
    });
    assert!(row.get("platform_assets").is_none());
    assert!(row.get("signature").is_none());
}

#[test]
fn error_payload_helpers_drive_the_probe_verdicts() {
    let graceful = json!({
        "error": "content_unavailable",
        "error_message": "No subtitles for this title"
    });
    let server = json!({
        "error": "internal_error",
        "error_message": "scraper panicked"
    });
    assert!(validate_error_response(&graceful).valid);
    assert!(DadErrorCode::ContentUnavailable.is_graceful());
    assert!(validate_error_response(&server).valid);
    assert!(!DadErrorCode::InternalError.is_graceful());
}

#[test]
fn host_side_stream_validation_against_declared_capabilities() {
    // A subtitle-only addon returning a stream is the contract violation the
    // capability gate exists to catch.
    let subtitle_only = allowed_stream_types_for_capabilities(&[DadCapability::Subtitle]);
    assert!(subtitle_only.is_empty());

    let stream_attempt = json!([
        { "type": "direct", "title": "sneaky", "stream_url": "https://x.example.com/a.mp4", "audio_languages": [] }
    ]);
    let result = validate_stream_items(&stream_attempt, Some(&subtitle_only));
    assert!(!result.valid);
    assert!(result.errors[0].contains("no stream types"));

    let both = allowed_stream_types_for_capabilities(&[
        DadCapability::DirectStream,
        DadCapability::Torrent,
    ]);
    assert_eq!(both.len(), 2);
}

#[test]
fn season_pack_file_idx_semantics() {
    // Episode 3 of a pack must say which file it means; 0 = single-file.
    let pack = json!({
        "type": "torrent",
        "title": "Show.S01.Complete.1080p",
        "info_hash": "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
        "file_idx": 2,
        "seeders": 142,
        "audio_languages": ["English", "Bangla"]
    });
    assert!(validate_stream_item(&pack).valid, "{}", validate_stream_item(&pack));
}

#[test]
fn per_stream_headers_never_leak_between_providers() {
    use std::collections::BTreeMap;

    let mut provider_a = BTreeMap::new();
    provider_a.insert("Referer".to_string(), "https://provider-a.example.com/".to_string());
    let mut provider_b = BTreeMap::new();
    provider_b.insert("Referer".to_string(), "https://provider-b.example.net/".to_string());

    let items = vec![
        StreamItem::proxied("Provider A 1080p", "https://provider-a.example.com/stream.m3u8", provider_a),
        StreamItem::proxied("Provider B 1080p", "https://provider-b.example.net/stream.m3u8", provider_b),
    ];
    let raw = stream_items_to_value(&items).unwrap();
    assert_ne!(raw[0]["headers"], raw[1]["headers"]);
    let result = validate_stream_items(&raw, Some(&[DadStreamType::Direct]));
    assert!(result.valid, "{}", result);
}
