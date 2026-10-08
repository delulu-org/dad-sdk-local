//! # dad-local-core
//!
//! The **contract** of the DAD local addon ecosystem — the Rust port of the
//! dad-sdk (TS, 3.0.2) contract discipline, re-targeted at local native-binary
//! addons speaking newline JSON-RPC over stdio (`protocol_version: "2.0"`).
//!
//! This crate is the single source of truth for BOTH sides of the wire:
//! - the **SDK runtime** (M1) validates every response an addon produces
//!   before it ever reaches stdout, and
//! - the **host** (the Delulu client, which intentionally does NOT depend on
//!   this crate) must implement the identical validation. Nothing enforces
//!   that today, so the two validators are kept aligned by hand.
//!
//! Design rules baked in here, non-negotiable:
//! - **snake_case, uniform**: manifest fields, RPC params, and response items
//!   all use the dad-sdk response-style naming (`stream_url`, `info_hash`,
//!   `audio_languages`, ...). No camelCase anywhere in the contract.
//! - **`deny_unknown_fields`** on manifest structs: an unknown field makes the
//!   whole manifest invalid, because canonically-dropped fields would escape
//!   the signature (post-sign field smuggling). New fields require a
//!   `protocol_version` bump.
//! - **Strictness by presence**: fields documented as inapplicable for one
//!   stream type (e.g. `info_hash` on a direct stream) are errors when
//!   *present at all* — `null`, `false`, and `{}` count as present.
//! - **Pure contract**: dependencies are `serde` + `serde_json` only. No I/O,
//!   no crypto, no platform code. Hashing, signing, and publishing live
//!   outside the SDK (Delulu's internal publisher tool); verification lives
//!   in the host.
//!
//! Contract baseline: dad-sdk **3.0.2** (`audio_languages` required; torrent
//! identity = `info_hash` + required `file_idx`, never `stream_url`; six
//! subtitle containers; graceful vs server/contract error split in tests).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod errors;
pub mod fixtures;
pub mod manifest;
pub mod responses;
pub mod validation;
pub mod version;

pub use fixtures::{DadTestFixture, DAD_TEST_FIXTURES};
pub use errors::{
    dad_error_status, is_error_response, looks_like_error_payload, parse_error_code,
    validate_error_response, DadError, DadErrorCode,
};
pub use manifest::{
    validate_manifest, AddonType, ApiKey, DadCapability, LocalAddonManifest,
    PlatformAsset, DAD_CAPABILITIES,
    SUPPORTED_PROTOCOL_VERSION,
};
pub use responses::{
    allowed_stream_types_for_capabilities, stream_items_to_value, validate_meta_response,
    validate_stream_item, validate_stream_items, validate_subtitle_items, DadRequest,
    DadStreamType, DirectStreamItem, MediaType, MetaResponse, ProxiedStreamItem, StreamItem,
    DAD_SUBTITLE_FORMATS, SubtitleFormat, SubtitleItem, TorrentStreamItem,
};
pub use validation::{is_bare_https_origin, is_https_url, Validation};
pub use version::is_valid_version;
