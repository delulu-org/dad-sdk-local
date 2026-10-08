//! The local addon manifest — the contract document.
//!
//! Served ONCE per addon and signature-bound to the exact per-platform binary
//! bits: the ed25519 `signature` (added at the publish stage, by the publish
//! CLI, over the canonical payload defined by [`LocalAddonManifest::canonical_payload`])
//! covers every field including each `platform_assets[*].sha256`. A version
//! therefore IS a specific set of bits: patch the binary → hash mismatch;
//! edit any manifest field → signature invalid; re-sign → needs the private
//! key that only exists at publish time.
//!
//! Two legal states, nothing in between:
//! - **authoring** — `sha256`/`signature` are `""` (or the fields are absent /
//!   null): what `dad-local init` scaffolds and what the author edits.
//! - **release** — every `sha256` filled (64-hex) + `signature` filled
//!   (standard base64 decoding to exactly 64 bytes). Only this state may be
//!   installed; the HOST hard-requires both.
//!
//! Serialization rules:
//! - `deny_unknown_fields` everywhere — an unknown field makes the manifest
//!   invalid, because canonically-dropped fields would escape the signature.
//!   Adding fields requires a `protocol_version` bump.
//! - snake_case field names, uniform with the rest of the local contract.
//! - empty means unset (`""` / `null` / absent are the same "not provided"
//!   state), matching the dad-sdk logo convention.

use crate::validation::{decode_standard_base64, is_https_url, Validation};
use crate::version::is_valid_version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// The only addon type this SDK builds.
pub const SUPPORTED_PROTOCOL_VERSION: &str = "2.0";

/// The four DAD capabilities, in the order `dad-local init` prints them.
pub const DAD_CAPABILITIES: [DadCapability; 4] = [
    DadCapability::Meta,
    DadCapability::DirectStream,
    DadCapability::Torrent,
    DadCapability::Subtitle,
];

/// A declared capability. Exactly one handler per capability is enforced at
/// COMPILE time by the runtime crate's `define_local_addon!` macro (M1);
/// this enum is the contract-side spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DadCapability {
    /// Metadata enrichment (IMDb id/rating, trailers).
    Meta,
    /// Directly-playable or proxied stream URLs.
    DirectStream,
    /// Torrent candidates for the host's torrent engine.
    Torrent,
    /// Subtitle tracks.
    Subtitle,
}

impl DadCapability {
    /// Wire spelling (same as the serde form).
    pub const fn as_str(self) -> &'static str {
        match self {
            DadCapability::Meta => "meta",
            DadCapability::DirectStream => "direct_stream",
            DadCapability::Torrent => "torrent",
            DadCapability::Subtitle => "subtitle",
        }
    }
}

/// The addon type. Only `local` exists here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddonType {
    /// A native binary the host spawns and speaks stdio JSON-RPC with.
    Local,
}

/// The OpenAI-style key gate: the host prompts the user at install time,
/// opens `page_url` in the OS browser, stores the key in its own vault, and
/// delivers it per-request inside the RPC params as `auth`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ApiKey {
    /// Hard gate: if `true`, the addon cannot be used until a key is provided
    /// — the SDK's RPC main rejects keyless requests with `unauthorized`
    /// before the handler runs.
    pub required: bool,
    /// HTTPS URL where the user signs up / generates a key.
    pub page_url: String,
}

/// One per-platform binary inside `platform_assets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct PlatformAsset {
    /// HTTPS URL of the binary (every redirect hop must also be HTTPS —
    /// enforced by the host's downloader).
    pub download_url: String,
    /// Filename inside the per-version install dir; no path separators, no `..`.
    pub binary_name: String,
    /// SHA-256 of THIS platform's binary: `""` while authoring, 64-hex once
    /// published. Per-platform because different targets produce different
    /// bytes — this is why there is no single top-level `sha256`.
    #[serde(default)]
    pub sha256: String,
    /// Arguments the host appends when spawning. Optional, default `"rpc"`.
    #[serde(default = "default_entry_command")]
    pub entry_command: String,
}

fn default_entry_command() -> String {
    "rpc".to_string()
}

/// The local addon manifest. See the module docs for the two legal states
/// and the signing story.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct LocalAddonManifest {
    /// Reverse-DNS id (e.g. `org.delulu.opensubs`). Never rename after
    /// publishing.
    pub id: String,
    /// Display name in the client.
    pub name: String,
    /// Strict `major.minor.patch` — the client's source of truth.
    pub version: String,
    /// Always `local`.
    #[serde(rename = "type")]
    pub addon_type: AddonType,
    /// The RPC contract this binary speaks. Must be `"2.0"`.
    pub protocol_version: String,
    /// One line for the listing shelf.
    #[serde(default)]
    pub description: Option<String>,
    /// Author or org display name.
    #[serde(default)]
    pub publisher: Option<String>,
    /// HTTPS logo URL. Omitted/empty → the host DISPLAYS the shared default
    /// logo; the manifest itself is never mutated post-signing.
    #[serde(default)]
    pub logo: Option<String>,
    /// Declared capabilities; every one must have its handler (compile-time
    /// in the runtime crate) and no capability may be declared twice.
    pub capabilities: Vec<DadCapability>,
    /// Optional API key gate.
    #[serde(default)]
    pub api_key: Option<ApiKey>,
    /// Per-platform binaries, keyed `"{os}-{arch}"` (e.g. `windows-x64`).
    /// At least one entry required.
    pub platform_assets: BTreeMap<String, PlatformAsset>,
    /// ed25519 signature over [`LocalAddonManifest::canonical_payload`]:
    /// `""` while authoring, standard base64 (64 bytes) once published.
    #[serde(default)]
    pub signature: String,
}

impl LocalAddonManifest {
    /// Parse + fully validate a raw JSON document.
    ///
    /// This is the ONE validator, used by the author (`dad-local validate`),
    /// the publish CLI, and (as the contract definition the host must mirror)
    /// at install time. Returns the typed manifest or every violation found.
    pub fn parse(raw: &Value) -> Result<LocalAddonManifest, Vec<String>> {
        let manifest: LocalAddonManifest = serde_json::from_value(raw.clone())
            .map_err(|e| vec![format!("Manifest is not a valid local addon manifest: {e}")])?;
        match validate_typed_manifest(&manifest) {
            Validation { valid: true, .. } => Ok(manifest),
            Validation { errors, .. } => Err(errors),
        }
    }

    /// The exact bytes the ed25519 `signature` covers.
    ///
    /// Defined as: **the typed manifest serialized to compact JSON, with the
    /// `signature` KEY removed entirely, keys sorted** (`serde_json`'s default
    /// map is a BTreeMap, so serialization is already key-sorted; absent
    /// optional fields serialize as explicit `null`s).
    ///
    /// Both the signer (publish CLI) and every verifier (host) MUST derive
    /// the payload through the typed struct — never from the raw JSON
    /// document — so absent-vs-null spelling differences on disk cannot
    /// produce two different payloads for the same manifest. The `signature`
    /// key is excluded even when empty, which kills the "signed with an empty
    /// string, then the field filled, bytes changed, signature invalid" bug
    /// class.
    ///
    /// NOTE: `serde_json`'s `preserve_order` feature must stay OFF for this
    /// crate (it is off by default and not enabled in our dependency tree);
    /// enabling it would break key sorting and therefore determinism.
    pub fn canonical_payload(&self) -> serde_json::Result<Vec<u8>> {
        let mut value = serde_json::to_value(self)?;
        if let Some(obj) = value.as_object_mut() {
            obj.remove("signature");
        }
        serde_json::to_vec(&value)
    }
}

fn is_reverse_dns_id(id: &str) -> bool {
    let segments: Vec<&str> = id.split('.').collect();
    if segments.len() < 2 {
        return false;
    }
    let valid_label = |label: &str, first: bool| {
        !label.is_empty()
            && label.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || (!first && c == '-')
            })
    };
    valid_label(segments[0], true) && segments[1..].iter().all(|s| valid_label(s, false))
}

fn is_platform_key(key: &str) -> bool {
    let Some((os, arch)) = key.split_once('-') else {
        return false;
    };
    !os.is_empty()
        && os.chars().all(|c| c.is_ascii_lowercase())
        && matches!(arch, "x64" | "x86" | "arm64")
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_valid_binary_name(name: &str) -> bool {
    !name.trim().is_empty() && !name.contains("..") && !name.contains('/') && !name.contains('\\')
}

fn optional_https(url: &Option<String>, field: &str, errors: &mut Vec<String>) {
    if let Some(url) = url {
        if url.trim().is_empty() || !is_https_url(url) {
            errors.push(format!("'{field}' must be an HTTPS URL when set"));
        }
    }
}

/// Validates an already-parsed typed manifest.
pub fn validate_typed_manifest(manifest: &LocalAddonManifest) -> Validation {
    let mut errors: Vec<String> = Vec::new();
    let m = manifest;

    // Identity.
    if m.id.trim().is_empty() {
        errors.push("Missing or invalid 'id' string".to_string());
    } else if !is_reverse_dns_id(&m.id) {
        errors.push(format!(
            "'id' must be reverse-DNS (e.g. 'com.yourname.addon-name') - got '{}'. \
             Same format 'dad-local init' enforces at scaffold time.",
            m.id
        ));
    }
    if m.name.trim().is_empty() {
        errors.push("Missing or invalid 'name' string".to_string());
    }
    if !is_valid_version(&m.version) {
        errors.push(
            "Invalid or missing 'version' - must be semantic 'major.minor.patch' (e.g. '1.2.0')"
                .to_string(),
        );
    }
    if m.protocol_version != SUPPORTED_PROTOCOL_VERSION {
        errors.push(format!(
            "'protocol_version' must be \"{}\" - got {:?}. This SDK speaks exactly one wire protocol.",
            SUPPORTED_PROTOCOL_VERSION, m.protocol_version
        ));
    }

    // Optional display fields: set means real.
    if let Some(description) = &m.description {
        if description.trim().is_empty() {
            errors.push("'description' must be a non-empty string when set".to_string());
        }
    }
    if let Some(publisher) = &m.publisher {
        if publisher.trim().is_empty() {
            errors.push("'publisher' must be a non-empty string when set".to_string());
        }
    }
    optional_https(&m.logo, "logo", &mut errors);

    // Capabilities: non-empty, no duplicates (validity is type-enforced).
    if m.capabilities.is_empty() {
        errors.push("Missing or empty 'capabilities' array".to_string());
    } else {
        let mut seen = HashSet::new();
        for cap in &m.capabilities {
            if !seen.insert(*cap) {
                errors.push(format!(
                    "Duplicate capability '{}' - declare each capability once",
                    cap.as_str()
                ));
            }
        }
    }

    // API key gate.
    if let Some(api_key) = &m.api_key {
        if api_key.page_url.trim().is_empty() || !is_https_url(&api_key.page_url) {
            errors.push("'api_key.page_url' must be an HTTPS URL".to_string());
        }
    }

    // Platform assets: at least one, valid keys, valid assets.
    if m.platform_assets.is_empty() {
        errors.push(
            "'platform_assets' must contain at least one entry (e.g. \"windows-x64\")".to_string(),
        );
    } else {
        for (key, asset) in &m.platform_assets {
            let at = format!("platform_assets.{key}");
            if !is_platform_key(key) {
                errors.push(format!(
                    "{at} is not a valid platform key - expected '{{os}}-{{arch}}' with \
                     arch one of x64, x86, arm64 (e.g. \"windows-x64\")"
                ));
            }
            if asset.download_url.trim().is_empty() || !is_https_url(&asset.download_url) {
                errors.push(format!("{at}.download_url must be an HTTPS URL"));
            }
            if !is_valid_binary_name(&asset.binary_name) {
                errors.push(format!(
                    "{at}.binary_name must be a plain filename - no path separators, no '..', \
                     not empty"
                ));
            }
            if !asset.sha256.is_empty() && !is_sha256_hex(&asset.sha256) {
                errors.push(format!(
                    "{at}.sha256 must be empty (authoring state) or 64 hex chars once published"
                ));
            }
            if asset.entry_command.trim().is_empty() {
                errors.push(format!(
                    "{at}.entry_command must be a non-empty string when set (default \"rpc\")"
                ));
            }
        }
    }

    // Signature: empty = authoring; otherwise standard base64 of exactly the
    // 64 bytes an ed25519 signature occupies. Content verification is crypto
    // work and belongs to the publish CLI / host, not this crate.
    if !m.signature.is_empty() {
        match decode_standard_base64(&m.signature) {
            Ok(bytes) if bytes.len() == 64 => {}
            Ok(bytes) => errors.push(format!(
                "'signature' must be standard base64 of a 64-byte ed25519 signature - \
                 decoded to {} bytes",
                bytes.len()
            )),
            Err(reason) => {
                errors.push(format!("'signature' is not valid base64: {reason}"));
            }
        }
    }

    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

/// Validates a raw JSON document against the local addon manifest contract.
///
/// Convenience wrapper: parses through the typed struct (so `deny_unknown_fields`
/// and type errors surface as violations) and returns the `{ valid, errors }`
/// shape. Prefer [`LocalAddonManifest::parse`] when you also want the value.
pub fn validate_manifest(raw: &Value) -> Validation {
    match LocalAddonManifest::parse(raw) {
        Ok(_) => Validation::ok(),
        Err(errors) => Validation::fail(errors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn minimal_manifest() -> Value {
        json!({
            "id": "org.example.my-addon",
            "name": "My Addon",
            "version": "1.0.0",
            "type": "local",
            "protocol_version": "2.0",
            "capabilities": ["direct_stream"],
            "platform_assets": {
                "windows-x64": {
                    "download_url": "https://example.com/my-addon.exe",
                    "binary_name": "my-addon.exe",
                    "sha256": "",
                    "entry_command": "rpc"
                }
            },
            "signature": ""
        })
    }

    #[test]
    fn minimal_authoring_manifest_is_valid() {
        let result = validate_manifest(&minimal_manifest());
        assert!(result.valid, "unexpected errors: {}", result);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let mut m = minimal_manifest();
        m["platform_assets"]["windows-x64"]["debug_build"] = json!(true);
        let result = validate_manifest(&m);
        assert!(!result.valid);
        assert!(
            result.errors.iter().any(|e| e.contains("unknown field")),
            "expected an unknown-field error, got: {}",
            result
        );
    }

    #[test]
    fn structural_violations() {
        let mut m = minimal_manifest();
        m["type"] = json!("http");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["protocol_version"] = json!("1.0");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["capabilities"] = json!([]);
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["capabilities"] = json!(["direct_stream", "direct_stream"]);
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["id"] = json!("My Addon");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["platform_assets"] = json!({});
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["platform_assets"] = json!({
            "win64": { "download_url": "https://example.com/a.exe", "binary_name": "a.exe" }
        });
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["platform_assets"]["windows-x64"]["binary_name"] = json!("../evil.exe");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["platform_assets"]["windows-x64"]["sha256"] = json!("abc123");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["platform_assets"]["windows-x64"]["download_url"] = json!("http://example.com/a.exe");
        assert!(!validate_manifest(&m).valid);

        let mut m = minimal_manifest();
        m["api_key"] = json!({ "required": true, "page_url": "http://nope.example.com" });
        assert!(!validate_manifest(&m).valid);
    }

    #[test]
    fn release_state_signature_shapes() {
        let good_sig = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
        let mut m = minimal_manifest();
        m["platform_assets"]["windows-x64"]["sha256"] =
            json!("9f2b4c8e17d3a6f05b8e2c41d97a3f6e8d0b5c2a9e7f4136d8c5a2b7e9f04136");
        m["signature"] = json!(good_sig);
        assert!(validate_manifest(&m).valid, "{}", validate_manifest(&m));

        // Not base64.
        let mut m = minimal_manifest();
        m["signature"] = json!("not base64!!");
        assert!(!validate_manifest(&m).valid);

        // Base64 but wrong length (decodes to 1 byte).
        let mut m = minimal_manifest();
        m["signature"] = json!("aGVsbG8=");
        assert!(!validate_manifest(&m).valid);
    }

    #[test]
    fn canonical_payload_is_deterministic_and_signature_free() {
        let manifest: LocalAddonManifest =
            serde_json::from_value(minimal_manifest()).expect("parse");
        let a = manifest.canonical_payload().unwrap();
        let b = manifest.canonical_payload().unwrap();
        assert_eq!(a, b);

        // Signature contents never affect the payload.
        let mut signed = manifest.clone();
        signed.signature = "3nF9xQ2vLm7Kd0pRs5tUw8yB1cE4gH6jN9oQ2tV5xZ8aC1dF4gH7kM0pS3vY6bE9hK2nQ5tW8zA1cF4gJ7mP0s==".to_string();
        assert_eq!(a, signed.canonical_payload().unwrap());

        // Keys are sorted; the payload starts with the alphabetically-first key.
        let text = String::from_utf8(a).unwrap();
        let keys: Vec<&str> = [
            "api_key", "capabilities", "description", "id", "logo",
            "name", "platform_assets", "protocol_version", "publisher",
            "type", "version",
        ]
        .into_iter()
        .filter(|k| text.contains(format!("\"{k}\"").as_str()))
        .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "keys must appear in sorted order in {text}");

        // Absent optionals serialize as explicit nulls (contract for signer
        // and verifier going through the typed struct).
        assert!(text.contains("\"description\":null"));
        assert!(!text.contains("\"signature\""));
    }
}
