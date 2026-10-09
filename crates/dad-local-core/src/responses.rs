//! Request / response contracts and their validators — the local port of
//! dad-sdk 3.0.1's `responses.ts`.
//!
//! Two layers live here:
//! 1. **Typed structs** (what addon handlers construct and the SDK serializes)
//!    — `DadRequest`, `StreamItem`, `MetaResponse`, `SubtitleItem`.
//! 2. **Raw-`Value` validators** (what both the SDK's RPC main and the host
//!    run against the wire JSON) — `validate_stream_item` and friends. The
//!    single code path guarantee: the SDK never emits a response that fails
//!    these validators, and the host never accepts one that does.
//!
//! The playback contract, unchanged from dad-sdk:
//! - `direct` stream with NO `headers` → the player hits the URL directly.
//! - `direct` with `needs_proxy: true` + NON-EMPTY `headers` → the host's
//!   proxy fetches it, injecting exactly those headers. Per-stream: Provider
//!   A's Referer never leaks onto Provider B's URL.
//! - `torrent` → identified by `info_hash` + REQUIRED `file_idx`; it has no
//!   URL — the peer swarm is the source. `file_idx` exists because season
//!   packs are the normal case; `0` means single-file.
//!
//! Strictness by PRESENCE: fields documented as inapplicable for one stream
//! type are errors when present AT ALL — `null`, `false`, and `{}` count as
//! present. (dad-sdk's doc comments promise exactly this; the local port
//! applies it uniformly, including `headers: null` on a plain direct stream,
//! which the TS validator let slip.)

use crate::errors::{DadError, DadErrorCode};
use crate::manifest::{is_reverse_dns_id, DadCapability};
use crate::validation::{is_https_url, json_type_name, Validation};
use crate::version::is_valid_version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

// ============================================================================
// Media type + request
// ============================================================================

/// The only two content domains Delulu carries. This closed set is exactly
/// why the whole contract can be strict: one canonical id space (`tmdb_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    /// A movie.
    Movie,
    /// A TV show.
    Tv,
}

/// The universal request — every handler call receives exactly this, inside
/// the RPC `params`. The host delivers the user's API key (when it holds one
/// for this addon) as `auth`; what it unlocks is the addon author's decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct DadRequest {
    /// TMDB id (e.g. 550).
    pub tmdb_id: u32,
    /// Movie or TV.
    pub media_type: MediaType,
    /// Season number — TV only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u32>,
    /// Episode number — TV only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e: Option<u32>,
    /// The raw API key from the host's vault, when the user holds one for
    /// this addon. Never persisted by the addon; used for this call only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
}

impl DadRequest {
    /// Semantic validation mirroring the HTTP contract's router rules
    /// (dad-sdk `createHttpAddonHandler`): a positive `tmdb_id`, and
    /// `s`/`e` are TV-only — a movie request carrying a season/episode is
    /// `bad_request`, never silently forwarded to the handler.
    pub fn validate(&self) -> Result<(), DadError> {
        if self.tmdb_id == 0 {
            return Err(DadError::new(
                DadErrorCode::BadRequest,
                "Invalid 'tmdb_id' - must be a positive integer (e.g. 550)",
            ));
        }
        if self.media_type == MediaType::Movie && (self.s.is_some() || self.e.is_some()) {
            return Err(DadError::new(
                DadErrorCode::BadRequest,
                "'s'/'e' (season/episode) are TV-only - a movie request carries neither. \
                 Use media_type 'tv' for per-episode calls.",
            ));
        }
        Ok(())
    }
}

// ============================================================================
// Stream items — typed authoring layer
// ============================================================================

/// The two stream kinds on the wire (`"type"` field values).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DadStreamType {
    /// Directly playable (or proxied by the host).
    #[default]
    Direct,
    /// A torrent for the host's torrent engine.
    Torrent,
}

impl DadStreamType {
    /// Wire spelling (same as the serde form).
    pub const fn as_str(self) -> &'static str {
        match self {
            DadStreamType::Direct => "direct",
            DadStreamType::Torrent => "torrent",
        }
    }
}

/// The stream item an addon handler returns. One flat type mirroring the wire
/// exactly — the SDK's constructors ([`StreamItem::direct`],
/// [`StreamItem::proxied`], [`StreamItem::torrent`]) fill the right fields and
/// leave the rest `None`, and the validator (run by the RPC main before
/// anything leaves the process) enforces the cross-type rules anyway.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct StreamItem {
    /// `direct` or `torrent`.
    #[serde(rename = "type")]
    pub item_type: DadStreamType,
    /// Display title / release name (e.g. "Server 1 - 1080p").
    pub title: String,

    // ── direct identity ────────────────────────────────────────────────────
    /// Playable video URL (MP4/HLS .m3u8), HTTPS only. Direct-only; FORBIDDEN
    /// on torrents (a torrent's identity is its `info_hash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_url: Option<String>,
    /// `true` marks a stream the HOST must proxy (with `headers`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_proxy: Option<bool>,
    /// The exact headers the host's proxy must inject. Required (non-empty)
    /// when `needs_proxy` is `true`; forbidden otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<std::collections::BTreeMap<String, String>>,

    // ── torrent identity ───────────────────────────────────────────────────
    /// BTIH info hash: 40 hex chars (v1) or 64 hex (v2). Torrent-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    /// Zero-based file index inside the torrent; REQUIRED for torrents,
    /// `0` for single-file. Direct-only presence is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_idx: Option<u64>,
    /// Reported seeders for ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u64>,
    /// Tracker announce URLs (`udp://`, `https://`, `wss://`, `ws://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trackers: Option<Vec<String>>,

    // ── shared, informational (all optional) ───────────────────────────────
    /// Container hint so the player picks an engine without sniffing the URL
    /// ("hls", "mp4", "mpd", "mkv", "webm", "other", ...). Free-form: the
    /// client drops what it cannot play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_format: Option<String>,
    /// e.g. "2160p", "1080p", "720p", "480p".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// e.g. "HEVC", "AVC", "AV1".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    /// e.g. "Dolby Vision", "HDR10+", "HDR", "SDR".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_format: Option<String>,
    /// e.g. "Dolby Atmos", "DTS-HD", "DD+", "AAC".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_format: Option<String>,
    /// REQUIRED. Languages in this file — display-only, so the shelf can say
    /// "2 audio tracks" before the user picks. A muxed English+Hindi file is
    /// ONE item listing both; never emit the same `stream_url` twice. Use
    /// `[]` when the languages are unknown — `null`/omitted is rejected, so
    /// client code maps over it directly.
    pub audio_languages: Vec<String>,
    /// File size in gigabytes (e.g. 2.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_gb: Option<f64>,
    /// Subtitles bundled with THIS stream — validated by the same rules as a
    /// standalone `/subtitles` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<Vec<SubtitleItem>>,
}

impl StreamItem {
    /// A directly-playable stream (no proxy, no headers).
    pub fn direct(title: impl Into<String>, stream_url: impl Into<String>) -> Self {
        StreamItem {
            item_type: DadStreamType::Direct,
            title: title.into(),
            stream_url: Some(stream_url.into()),
            audio_languages: Vec::new(),
            ..Default::default()
        }
    }

    /// A stream the HOST must proxy, injecting exactly these headers.
    pub fn proxied(
        title: impl Into<String>,
        stream_url: impl Into<String>,
        headers: std::collections::BTreeMap<String, String>,
    ) -> Self {
        StreamItem {
            item_type: DadStreamType::Direct,
            title: title.into(),
            stream_url: Some(stream_url.into()),
            needs_proxy: Some(true),
            headers: Some(headers),
            audio_languages: Vec::new(),
            ..Default::default()
        }
    }

    /// A torrent candidate. `file_idx` `0` means single-file.
    pub fn torrent(
        title: impl Into<String>,
        info_hash: impl Into<String>,
        file_idx: u64,
    ) -> Self {
        StreamItem {
            item_type: DadStreamType::Torrent,
            title: title.into(),
            info_hash: Some(info_hash.into()),
            file_idx: Some(file_idx),
            audio_languages: Vec::new(),
            ..Default::default()
        }
    }
}

/// Dedicated authoring types for callers that want the stricter shape at
/// construction time (the RPC main accepts `Vec<StreamItem>`; these convert).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct DirectStreamItem {
    /// Display title.
    pub title: String,
    /// Playable HTTPS URL.
    pub stream_url: String,
    /// Container hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_format: Option<String>,
    /// e.g. "1080p".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Required languages list (`[]` = unknown).
    pub audio_languages: Vec<String>,
    /// Subtitles riding on this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<Vec<SubtitleItem>>,
}

/// A proxied variant: `needs_proxy: true` + the exact headers to inject.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ProxiedStreamItem {
    /// Display title.
    pub title: String,
    /// HTTPS URL the proxy fetches on the player's behalf.
    pub stream_url: String,
    /// The exact headers to inject (Referer, User-Agent, Origin, Cookie...).
    pub headers: std::collections::BTreeMap<String, String>,
    /// Container hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_format: Option<String>,
    /// e.g. "1080p".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Required languages list (`[]` = unknown).
    pub audio_languages: Vec<String>,
    /// Subtitles riding on this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<Vec<SubtitleItem>>,
}

/// A torrent candidate: identity is `info_hash` + `file_idx`, nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct TorrentStreamItem {
    /// Display title / release name.
    pub title: String,
    /// 40-hex (v1) or 64-hex (v2) BTIH — the torrent's identity.
    pub info_hash: String,
    /// Zero-based file index; `0` for single-file.
    pub file_idx: u64,
    /// Reported seeders for ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<u64>,
    /// Optional tracker announce URLs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trackers: Option<Vec<String>>,
    /// Required languages list (`[]` = unknown).
    pub audio_languages: Vec<String>,
    /// Subtitles riding on this stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitles: Option<Vec<SubtitleItem>>,
}

impl From<DirectStreamItem> for StreamItem {
    fn from(d: DirectStreamItem) -> Self {
        StreamItem {
            item_type: DadStreamType::Direct,
            title: d.title,
            stream_url: Some(d.stream_url),
            media_format: d.media_format,
            resolution: d.resolution,
            audio_languages: d.audio_languages,
            subtitles: d.subtitles,
            ..Default::default()
        }
    }
}

impl From<ProxiedStreamItem> for StreamItem {
    fn from(p: ProxiedStreamItem) -> Self {
        StreamItem {
            item_type: DadStreamType::Direct,
            title: p.title,
            stream_url: Some(p.stream_url),
            needs_proxy: Some(true),
            headers: Some(p.headers),
            media_format: p.media_format,
            resolution: p.resolution,
            audio_languages: p.audio_languages,
            subtitles: p.subtitles,
            ..Default::default()
        }
    }
}

impl From<TorrentStreamItem> for StreamItem {
    fn from(t: TorrentStreamItem) -> Self {
        StreamItem {
            item_type: DadStreamType::Torrent,
            title: t.title,
            info_hash: Some(t.info_hash),
            file_idx: Some(t.file_idx),
            seeders: t.seeders,
            trackers: t.trackers,
            audio_languages: t.audio_languages,
            subtitles: t.subtitles,
            ..Default::default()
        }
    }
}

// ============================================================================
// Meta + subtitle typed layer
// ============================================================================

/// Meta enrichment — fills ONLY what TMDB doesn't already carry. Every field
/// optional; `None`/absent means "unknown / not found".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct MetaResponse {
    /// Canonical IMDb id (e.g. "tt0137523").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_id: Option<String>,
    /// Canonical IMDb community rating as a NUMBER (e.g. 8.8) — an addon MUST
    /// normalize (`parse`) any string rating before returning. `None` when
    /// unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_rating: Option<f64>,
    /// Official trailer URLs, ordered by preference — the FIRST entry is the
    /// default. HTTPS only. Empty/absent = no trailer. One adaptive URL per
    /// title; quality selection is the client player's job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailers: Option<Vec<String>>,
}

/// The subtitle container formats the contract recognizes (dad-sdk 3.0.1
/// carried only vtt/srt; 3.0.2 expanded the set). The SDK checks only that
/// the addon advertises a supported container — it never transcodes; the
/// player renders.
pub const DAD_SUBTITLE_FORMATS: [&str; 6] = ["vtt", "srt", "ass", "ssa", "ttml", "dfxp"];

/// Subtitle format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleFormat {
    /// WebVTT.
    Vtt,
    /// SubRip.
    Srt,
    /// Advanced SubStation Alpha.
    Ass,
    /// SubStation Alpha.
    Ssa,
    /// Timed Text Markup Language.
    Ttml,
    /// Distribution Format Exchange Profile.
    Dfxp,
}

/// One subtitle track, standalone or embedded on a stream item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SubtitleItem {
    /// Stable id for the client (e.g. "en-sdh").
    pub id: String,
    /// Track URL.
    pub url: String,
    /// ISO-ish code (e.g. "en", "es", "bn", "hi").
    pub lang_code: String,
    /// Display language name (e.g. "English").
    pub language: String,
    /// Display title (e.g. "English \[SDH\]").
    pub title: String,
    /// One of `DAD_SUBTITLE_FORMATS`.
    pub format: SubtitleFormat,
    /// Optional provider display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

// ============================================================================
// Raw-Value validators — the contract enforcement both sides run
// ============================================================================

/// Fields that only exist for the torrent engine. Forbidden on `direct`
/// items, checked by PRESENCE (`null` counts as present).
const TORRENT_ONLY_FIELDS: [&str; 4] = ["info_hash", "file_idx", "trackers", "seeders"];

fn is_info_hash(value: &str) -> bool {
    (value.len() == 40 || value.len() == 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_tracker_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "udp" | "tcp" | "wss" | "ws" | "https"
    ) && !rest.is_empty()
    && !value.chars().any(|c| c.is_whitespace())
}

/// Validates ONE stream item against the playback contract. See the module
/// docs for the presence-strictness rule.
pub fn validate_stream_item(item: &Value) -> Validation {
    let Some(obj) = item.as_object() else {
        return Validation::error(format!(
            "Stream item must be an object - got {}",
            json_type_name(item)
        ));
    };
    let mut errors: Vec<String> = Vec::new();

    let item_type = match obj.get("type").and_then(Value::as_str) {
        Some("direct") => DadStreamType::Direct,
        Some("torrent") => DadStreamType::Torrent,
        Some(other) => {
            return Validation::error(format!(
                "Invalid stream 'type' {other:?}. Must be 'direct' or 'torrent'"
            ));
        }
        None => {
            return Validation::error("Missing string 'type' field - must be 'direct' or 'torrent'");
        }
    };

    match obj.get("title") {
        Some(Value::String(title)) if !title.trim().is_empty() => {}
        _ => errors.push("Missing or invalid non-empty 'title' string".to_string()),
    }

    // Identity is per-type; each type forbids the other's fields BY PRESENCE.
    match item_type {
        DadStreamType::Direct => {
            match obj.get("stream_url") {
                Some(Value::String(url)) if !url.trim().is_empty() => {
                    if !is_https_url(url) {
                        errors.push(format!(
                            "Direct stream 'stream_url' must be an HTTPS URL - got '{url}'. \
                             (javascript:, file:, and similar schemes are never valid.)"
                        ));
                    }
                }
                _ => errors.push("Missing or invalid non-empty 'stream_url' string".to_string()),
            }
            for field in TORRENT_ONLY_FIELDS {
                if obj.contains_key(field) {
                    errors.push(format!(
                        "Direct stream items must NOT carry '{field}' - it is a torrent-engine \
                         field and a direct URL has no torrent to identify."
                    ));
                }
            }

            // Proxy semantics. Presence-strict: `headers: null` is a value
            // the addon chose to send for a documented-inapplicable field.
            let needs_proxy_true = obj.get("needs_proxy") == Some(&Value::Bool(true));
            if needs_proxy_true {
                match obj.get("headers") {
                    Some(Value::Object(headers)) if !headers.is_empty() => {
                        for (key, value) in headers {
                            if !value.is_string() {
                                errors.push(format!("Header '{key}' must be a string value"));
                            }
                        }
                    }
                    _ => errors.push(
                        "'needs_proxy: true' requires a non-empty 'headers' object \
                         (e.g. { Referer, User-Agent })"
                            .to_string(),
                    ),
                }
            } else if obj.contains_key("headers") {
                errors.push(
                    "Directly-playable stream cannot carry 'headers'. Either drop them \
                     entirely (plays direct) or set 'needs_proxy: true'."
                        .to_string(),
                );
            }
        }
        DadStreamType::Torrent => {
            if obj.contains_key("stream_url") {
                errors.push(
                    "Torrent items must NOT carry 'stream_url' - a torrent has no playable \
                     URL; the peer swarm is the source. Send 'info_hash' (+ 'file_idx') instead."
                        .to_string(),
                );
            }
            if obj.contains_key("headers") {
                errors.push(
                    "Torrent items must not carry 'headers' - torrents go to the torrent \
                     engine, not the proxy"
                        .to_string(),
                );
            }
            if obj.contains_key("needs_proxy") {
                errors.push(
                    "Torrent items must not set 'needs_proxy' (not even false) - torrents \
                     go to the torrent engine, not the proxy"
                        .to_string(),
                );
            }

            // The strictest check in the whole stream contract: a wrong hash
            // downloads a completely different torrent.
            match obj.get("info_hash") {
                Some(Value::String(hash)) if !hash.trim().is_empty() => {
                    if !is_info_hash(hash) {
                        errors.push(format!(
                            "Torrent 'info_hash' must be 40 hex chars (v1) or 64 hex chars \
                             (v2), nothing else - got '{hash}'"
                        ));
                    }
                }
                _ => errors.push(
                    "Torrent items require an 'info_hash' - a torrent has no URL, so the \
                     BTIH hash is its identity (40-hex v1 or 64-hex v2)"
                        .to_string(),
                ),
            }

            match obj.get("file_idx") {
                Some(Value::Number(n)) if n.is_u64() => {}
                Some(other) => errors.push(format!(
                    "Torrent 'file_idx' must be a non-negative integer (use 0 for a \
                     single-file torrent) - got {}",
                    json_type_name(other)
                )),
                None => errors.push(
                    "Torrent items require 'file_idx' - the zero-based index of the file \
                     you mean. Use 0 for a single-file torrent; for a season/episode pack \
                     it must be the exact episode requested."
                        .to_string(),
                ),
            }

            match obj.get("seeders") {
                None | Some(Value::Null) => {}
                Some(Value::Number(n)) if n.is_u64() => {}
                Some(_) => errors.push(
                    "Torrent 'seeders' must be a non-negative integer when present (e.g. 42)"
                        .to_string(),
                ),
            }

            match obj.get("trackers") {
                None | Some(Value::Null) => {}
                Some(Value::Array(trackers)) => {
                    for tracker in trackers {
                        let ok = tracker
                            .as_str()
                            .map(|t| !t.trim().is_empty() && is_tracker_url(t.trim()))
                            .unwrap_or(false);
                        if !ok {
                            errors.push(
                                "'trackers' must contain only usable announce URLs - expected \
                                 udp://, https://, wss://, or ws://"
                                    .to_string(),
                            );
                            break;
                        }
                    }
                }
                Some(_) => errors.push("'trackers' must be an array of tracker announce URLs".to_string()),
            }
        }
    }

    // audio_languages: REQUIRED everywhere (see StreamItem docs).
    match obj.get("audio_languages") {
        Some(Value::Array(langs)) => {
            for (i, lang) in langs.iter().enumerate() {
                let ok = lang.as_str().map(|l| !l.trim().is_empty()).unwrap_or(false);
                if !ok {
                    errors.push(format!(
                        "'audio_languages' must contain only non-empty language names - \
                         bad entry at index {i}"
                    ));
                }
            }
        }
        Some(other) => errors.push(format!(
            "'audio_languages' must be an array of language names (e.g. [\"English\", \
             \"Hindi\"]) - got {}",
            json_type_name(other)
        )),
        None => errors.push(
            "Missing 'audio_languages' - it is required because every stream has audio. \
             Use [] if you cannot tell."
                .to_string(),
        ),
    }

    // Informational hints: well-formed if present, never whitelisted.
    for field in ["media_format", "resolution", "codec", "hdr_format", "audio_format"] {
        if let Some(v) = obj.get(field) {
            if !v.is_null() && !v.is_string() {
                errors.push(format!("'{field}' must be a string if present (got {})", json_type_name(v)));
            }
        }
    }
    if let Some(v) = obj.get("size_gb") {
        if !v.is_null() && !v.is_number() {
            errors.push(format!("'size_gb' must be a number if present (got {})", json_type_name(v)));
        }
    }

    // Embedded per-stream subtitles: same contract as standalone.
    match obj.get("subtitles") {
        None | Some(Value::Null) => {}
        Some(subs) => {
            let check = validate_subtitle_items(subs);
            if !check.valid {
                errors.push(format!("Embedded 'subtitles' on stream: {}", check));
            }
        }
    }

    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

/// Derives the stream types an addon may emit from its declared capabilities.
/// Both the RPC main (production) and `dad-local test` call this — never
/// re-derive it per call site, so the two can never drift. An addon with no
/// stream capability maps to an EMPTY list, and an empty list passed to
/// [`validate_stream_items`] means "no stream type is permitted".
pub fn allowed_stream_types_for_capabilities(capabilities: &[DadCapability]) -> Vec<DadStreamType> {
    let mut allowed = Vec::new();
    if capabilities.contains(&DadCapability::DirectStream) {
        allowed.push(DadStreamType::Direct);
    }
    if capabilities.contains(&DadCapability::Torrent) {
        allowed.push(DadStreamType::Torrent);
    }
    allowed
}

/// Validates a whole stream list. `allowed` is derived from the addon's
/// DECLARED capabilities ([`allowed_stream_types_for_capabilities`]):
/// `None` skips the capability check entirely; `Some(&[])` enforces "no
/// stream type is permitted" — any item at all is an error.
pub fn validate_stream_items(items: &Value, allowed: Option<&[DadStreamType]>) -> Validation {
    let Some(list) = items.as_array() else {
        return Validation::error("Stream result must be an array of stream items".to_string());
    };
    let mut errors: Vec<String> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let check = validate_stream_item(item);
        if !check.valid {
            errors.extend(check.errors.into_iter().map(|e| format!("Stream item #{}: {e}", i + 1)));
            continue;
        }
        if let Some(allowed) = allowed {
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            let permitted = allowed.iter().any(|a| a.as_str() == item_type);
            if !permitted {
                let permitted_list = if allowed.is_empty() {
                    "no stream types".to_string()
                } else {
                    format!("allowed: {}", allowed.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", "))
                };
                errors.push(format!(
                    "Stream item #{}: type '{}' is not allowed by this addon's declared \
                     capabilities ({permitted_list})",
                    i + 1,
                    item_type
                ));
            }
        }
    }
    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

/// Validates a meta response against the meta contract. A `null` result
/// ("found nothing") is valid.
pub fn validate_meta_response(raw: &Value) -> Validation {
    if raw.is_null() {
        return Validation::ok();
    }
    let Some(obj) = raw.as_object() else {
        return Validation::error(format!(
            "Meta response must be an object or null - got {}",
            json_type_name(raw)
        ));
    };
    let mut errors: Vec<String> = Vec::new();

    match obj.get("imdb_id") {
        None | Some(Value::Null) => {}
        Some(Value::String(_)) => {}
        Some(other) => errors.push(format!("'imdb_id' must be a string - got {}", json_type_name(other))),
    }
    match obj.get("imdb_rating") {
        None | Some(Value::Null) => {}
        Some(Value::Number(n)) if n.as_f64().map(|f| f.is_finite()).unwrap_or(false) => {}
        Some(_) => errors.push(
            "'imdb_rating' must be a number - normalize string ratings (e.g. parse) before \
             returning"
                .to_string(),
        ),
    }
    match obj.get("trailers") {
        None | Some(Value::Null) => {}
        Some(Value::Array(trailers)) => {
            for (i, trailer) in trailers.iter().enumerate() {
                match trailer.as_str() {
                    Some(url) if is_https_url(url) => {}
                    Some(url) => errors.push(format!(
                        "trailers[{i}] must be an HTTPS URL - got '{url}'"
                    )),
                    None => errors.push(format!(
                        "trailers[{i}] must be a string - got {}",
                        json_type_name(trailer)
                    )),
                }
            }
        }
        Some(_) => errors.push("'trailers' must be an array of HTTPS URL strings".to_string()),
    }

    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

/// Validates a subtitle list (structural checks on each item, exactly
/// mirroring dad-sdk's `validateSubtitleItems`).
pub fn validate_subtitle_items(items: &Value) -> Validation {
    let Some(list) = items.as_array() else {
        return Validation::error("Subtitle result must be an array".to_string());
    };
    let mut errors: Vec<String> = Vec::new();
    for (i, item) in list.iter().enumerate() {
        let at = format!("Subtitle item #{}", i + 1);
        let Some(obj) = item.as_object() else {
            errors.push(format!("{at}: must be an object - got {}", json_type_name(item)));
            continue;
        };
        for field in ["id", "url", "lang_code", "language", "title"] {
            match obj.get(field) {
                Some(Value::String(s)) if !s.is_empty() => {}
                _ => errors.push(format!("{at}: missing or invalid '{field}' string")),
            }
        }
        match obj.get("format") {
            Some(Value::String(format)) if DAD_SUBTITLE_FORMATS.contains(&format.as_str()) => {}
            Some(other) => errors.push(format!(
                "{at}: 'format' must be one of {} - got {}",
                DAD_SUBTITLE_FORMATS.join(", "),
                json_type_name(other)
            )),
            None => errors.push(format!("{at}: 'format' must be one of {}", DAD_SUBTITLE_FORMATS.join(", "))),
        }
    }
    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

impl fmt::Display for MediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MediaType::Movie => "movie",
            MediaType::Tv => "tv",
        })
    }
}

/// Convenience: build a `Value` object from a typed request (what the host
/// writes into RPC params).
impl DadRequest {
    /// Serialize to the JSON `params` value.
    pub fn to_params(&self) -> serde_json::Result<Value> {
        serde_json::to_value(self)
    }
}

/// Convenience: the typed stream list as raw JSON (what the RPC main
/// validates before writing to stdout).
pub fn stream_items_to_value(items: &[StreamItem]) -> serde_json::Result<Value> {
    serde_json::to_value(items)
}

// ============================================================================
// Health check / Pong contract
// ============================================================================

/// The universal 4-field health pong payload returned by any addon responding
/// to a health ping (`healthCheck`, `health`, or `ping`).
///
/// Guaranteed to contain exactly these 4 fields across both local and HTTP SDKs.
/// Health endpoints are unconditionally accessible and never require an API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthPong {
    /// Always `true` when the addon is alive and responsive.
    pub ok: bool,
    /// Canonical reverse-DNS identifier (e.g. `"dev.fearless.rosseta"`).
    pub addon_id: String,
    /// Human-readable display name.
    pub name: String,
    /// Semver version string (e.g. `"0.1.0"`).
    pub version: String,
}

impl HealthPong {
    /// Creates a new HealthPong from the given addon metadata.
    pub fn new(
        addon_id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            ok: true,
            addon_id: addon_id.into(),
            name: name.into(),
            version: version.into(),
        }
    }
}

/// Validates a raw `Value` against the universal 4-field HealthPong contract.
pub fn validate_health_pong(raw: &Value) -> Validation {
    let Some(obj) = raw.as_object() else {
        return Validation::error(format!(
            "Health pong response must be an object - got {}",
            json_type_name(raw)
        ));
    };

    let mut errors = Vec::new();

    // Check unknown fields
    for key in obj.keys() {
        if !matches!(key.as_str(), "ok" | "addon_id" | "name" | "version") {
            errors.push(format!("Unknown field in health pong: '{key}'"));
        }
    }

    match obj.get("ok") {
        Some(Value::Bool(true)) => {}
        Some(Value::Bool(false)) => errors.push("'ok' must be true in a healthy pong response".to_string()),
        Some(other) => errors.push(format!("'ok' must be boolean true - got {}", json_type_name(other))),
        None => errors.push("Missing 'ok' field in health pong".to_string()),
    }

    match obj.get("addon_id") {
        Some(Value::String(id)) if !id.trim().is_empty() => {
            if !is_reverse_dns_id(id) {
                errors.push(format!(
                    "'addon_id' must be a reverse-DNS identifier (e.g. 'org.example.demo') - got '{id}'"
                ));
            }
        }
        Some(other) => errors.push(format!("'addon_id' must be a non-empty string - got {}", json_type_name(other))),
        None => errors.push("Missing 'addon_id' field in health pong".to_string()),
    }

    match obj.get("name") {
        Some(Value::String(name)) if !name.trim().is_empty() => {}
        Some(other) => errors.push(format!("'name' must be a non-empty string - got {}", json_type_name(other))),
        None => errors.push("Missing 'name' field in health pong".to_string()),
    }

    match obj.get("version") {
        Some(Value::String(v)) if !v.trim().is_empty() => {
            if !is_valid_version(v) {
                errors.push(format!(
                    "'version' must be a valid semver string (e.g. '1.0.0') - got '{v}'"
                ));
            }
        }
        Some(other) => errors.push(format!("'version' must be a non-empty string - got {}", json_type_name(other))),
        None => errors.push("Missing 'version' field in health pong".to_string()),
    }

    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation::fail(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn request_shape_is_strict_snake_case() {
        let req: DadRequest = serde_json::from_value(json!({
            "tmdb_id": 550, "media_type": "movie", "auth": "sk_live_x"
        }))
        .unwrap();
        assert_eq!(req.tmdb_id, 550);
        assert_eq!(req.media_type, MediaType::Movie);

        // camelCase is dead.
        assert!(serde_json::from_value::<DadRequest>(json!({
            "tmdbId": 550, "mediaType": "movie"
        }))
        .is_err());
        // Unknown params rejected.
        assert!(serde_json::from_value::<DadRequest>(json!({
            "tmdb_id": 550, "media_type": "movie", "extra": 1
        }))
        .is_err());
    }

    #[test]
    fn request_semantic_validation_mirrors_http_router() {
        let movie = DadRequest {
            tmdb_id: 550,
            media_type: MediaType::Movie,
            s: None,
            e: None,
            auth: None,
        };
        assert!(movie.validate().is_ok());

        // tmdb_id 0 is not a real TMDB id.
        let zero = DadRequest { tmdb_id: 0, ..movie.clone() };
        assert_eq!(zero.validate().unwrap_err().code, DadErrorCode::BadRequest);

        // Movies take no season/episode.
        let movie_with_season = DadRequest {
            tmdb_id: 550,
            media_type: MediaType::Movie,
            s: Some(1),
            e: None,
            auth: None,
        };
        assert_eq!(movie_with_season.validate().unwrap_err().code, DadErrorCode::BadRequest);

        // TV with s/e is the normal case.
        let episode = DadRequest {
            tmdb_id: 1930,
            media_type: MediaType::Tv,
            s: Some(1),
            e: Some(1),
            auth: None,
        };
        assert!(episode.validate().is_ok());
    }

    #[test]
    fn direct_stream_contract() {
        // Minimal valid direct stream.
        let ok = json!({
            "type": "direct",
            "title": "Server 1 1080p",
            "stream_url": "https://cdn.example.com/movie.m3u8",
            "audio_languages": []
        });
        assert!(validate_stream_item(&ok).valid, "{}", validate_stream_item(&ok));

        // stream_url must be HTTPS.
        let bad = json!({
            "type": "direct", "title": "x", "stream_url": "http://cdn.example.com/v.mp4",
            "audio_languages": []
        });
        assert!(!validate_stream_item(&bad).valid);

        // Torrent-only fields forbidden by PRESENCE - even null.
        for present in [json!(null), json!("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"), json!(0)] {
            let mut item = ok.clone();
            item["info_hash"] = present;
            assert!(!validate_stream_item(&item).valid);
        }

        // headers without needs_proxy: true - error (even null: presence rule).
        let mut item = ok.clone();
        item["headers"] = json!({ "Referer": "https://x.example.com/" });
        assert!(!validate_stream_item(&item).valid);
        let mut item = ok.clone();
        item["headers"] = json!(null);
        assert!(!validate_stream_item(&item).valid);

        // needs_proxy: true requires non-empty string headers.
        let mut proxied = ok.clone();
        proxied["needs_proxy"] = json!(true);
        assert!(!validate_stream_item(&proxied).valid); // missing
        proxied["headers"] = json!({});
        assert!(!validate_stream_item(&proxied).valid); // empty
        proxied["headers"] = json!({ "Referer": "https://provider-a.example.com/", "User-Agent": "Mozilla/5.0" });
        assert!(validate_stream_item(&proxied).valid, "{}", validate_stream_item(&proxied));

        // audio_languages is required.
        let mut missing_langs = ok.clone();
        missing_langs.as_object_mut().unwrap().remove("audio_languages");
        assert!(validate_stream_item(&missing_langs).errors.iter().any(|e| e.contains("audio_languages")));
        let mut null_langs = ok.clone();
        null_langs["audio_languages"] = json!(null);
        assert!(!validate_stream_item(&null_langs).valid);
        let mut empty_lang_name = ok.clone();
        empty_lang_name["audio_languages"] = json!(["English", ""]);
        assert!(!validate_stream_item(&empty_lang_name).valid);
    }

    #[test]
    fn torrent_stream_contract() {
        let base = json!({
            "type": "torrent",
            "title": "Movie.2160p.Remux",
            "info_hash": "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
            "file_idx": 0,
            "audio_languages": ["English"]
        });
        assert!(validate_stream_item(&base).valid, "{}", validate_stream_item(&base));

        // 64-hex v2 hash works.
        let mut v2 = base.clone();
        v2["info_hash"] = json!("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678a1b2c3d4e5f60718293a4b5c");
        assert!(validate_stream_item(&v2).valid);

        // 39 hex chars: rejected.
        let mut short = base.clone();
        short["info_hash"] = json!("a1b2c3d4e5f60718293a4b5c6d7e8f90123456");
        assert!(!validate_stream_item(&short).valid);

        // stream_url forbidden.
        let mut with_url = base.clone();
        with_url["stream_url"] = json!("https://x.example.com/a.mkv");
        assert!(!validate_stream_item(&with_url).valid);

        // file_idx required, integer, non-negative.
        let mut no_idx = base.clone();
        no_idx.as_object_mut().unwrap().remove("file_idx");
        assert!(!validate_stream_item(&no_idx).valid);
        let mut float_idx = base.clone();
        float_idx["file_idx"] = json!(1.5);
        assert!(!validate_stream_item(&float_idx).valid);
        let mut negative_idx = base.clone();
        negative_idx["file_idx"] = json!(-1);
        assert!(!validate_stream_item(&negative_idx).valid);

        // headers/needs_proxy forbidden - even null/false.
        let mut with_headers = base.clone();
        with_headers["headers"] = json!(null);
        assert!(!validate_stream_item(&with_headers).valid);
        let mut with_proxy = base.clone();
        with_proxy["needs_proxy"] = json!(false);
        assert!(!validate_stream_item(&with_proxy).valid);

        // trackers must be usable announce URLs.
        let mut good_trackers = base.clone();
        good_trackers["trackers"] = json!(["udp://tracker.example.org:1337/announce", "wss://tracker.example.com"]);
        assert!(validate_stream_item(&good_trackers).valid);
        let mut bad_trackers = base.clone();
        bad_trackers["trackers"] = json!(["not a url"]);
        assert!(!validate_stream_item(&bad_trackers).valid);
        let mut js_trackers = base.clone();
        js_trackers["trackers"] = json!(["javascript:alert(1)"]);
        assert!(!validate_stream_item(&js_trackers).valid);
    }

    #[test]
    fn allowed_types_capability_gate() {
        let direct_only = allowed_stream_types_for_capabilities(&[DadCapability::DirectStream]);
        let items = json!([
            { "type": "torrent", "title": "t", "info_hash": "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", "file_idx": 0, "audio_languages": [] }
        ]);
        let result = validate_stream_items(&items, Some(&direct_only));
        assert!(!result.valid);
        assert!(result.errors[0].contains("not allowed by this addon's declared capabilities"));

        // Empty allowed list = nothing permitted (subtitle-only addon).
        let direct_items = json!([
            { "type": "direct", "title": "s", "stream_url": "https://x.example.com/a.mp4", "audio_languages": [] }
        ]);
        assert!(!validate_stream_items(&direct_items, Some(&[])).valid);

        // None skips the check (raw validation use).
        assert!(validate_stream_items(&direct_items, None).valid);
    }

    #[test]
    fn meta_contract() {
        assert!(validate_meta_response(&json!(null)).valid);
        assert!(validate_meta_response(&json!({})).valid);
        let good = json!({
            "imdb_id": "tt0137523",
            "imdb_rating": 8.8,
            "trailers": ["https://example.com/t.mp4"]
        });
        assert!(validate_meta_response(&good).valid);

        let string_rating = json!({ "imdb_rating": "8.8" });
        assert!(!validate_meta_response(&string_rating).valid);

        let http_trailer = json!({ "trailers": ["http://example.com/t.mp4"] });
        assert!(!validate_meta_response(&http_trailer).valid);
    }

    #[test]
    fn subtitle_contract() {
        let good = json!([{
            "id": "en-sdh", "url": "https://cdn.example.com/en.vtt",
            "lang_code": "en", "language": "English", "title": "English [SDH]",
            "format": "vtt"
        }]);
        assert!(validate_subtitle_items(&good).valid);

        // 3.0.2 expanded the format set: all six containers are valid.
        for format in DAD_SUBTITLE_FORMATS {
            let item = json!([{
                "id": "x", "url": "https://c.example.com/a.track",
                "lang_code": "en", "language": "English", "title": "x",
                "format": format
            }]);
            assert!(validate_subtitle_items(&item).valid, "format '{format}' must be valid");
        }

        let bad_format = json!([{ "id": "x", "url": "https://c.example.com/a.sub", "lang_code": "en", "language": "English", "title": "x", "format": "sub" }]);
        assert!(!validate_subtitle_items(&bad_format).valid);

        let missing_field = json!([{ "id": "x", "lang_code": "en", "language": "English", "title": "x", "format": "srt" }]);
        assert!(!validate_subtitle_items(&missing_field).valid);
    }

    #[test]
    fn typed_constructors_roundtrip_through_validator() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Referer".to_string(), "https://provider-a.example.com/".to_string());
        let items = vec![
            StreamItem::direct("Direct 1080p", "https://cdn.example.com/movie.m3u8"),
            StreamItem::proxied("Proxied", "https://provider-a.example.com/stream.m3u8", headers),
            StreamItem::torrent("Movie.2160p", "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678", 0),
        ];
        let raw = stream_items_to_value(&items).unwrap();
        let allowed = allowed_stream_types_for_capabilities(&[
            DadCapability::DirectStream,
            DadCapability::Torrent,
        ]);
        let result = validate_stream_items(&raw, Some(&allowed));
        assert!(result.valid, "{}", result);

        // Wire spelling spot checks.
        assert_eq!(raw[0]["type"], json!("direct"));
        assert_eq!(raw[0]["audio_languages"], json!([]));
        assert!(raw[0].get("needs_proxy").is_none());
        assert_eq!(raw[1]["needs_proxy"], json!(true));
        assert_eq!(raw[2]["file_idx"], json!(0));
    }

    #[test]
    fn embedded_subtitles_are_validated() {
        let with_bad_subs = json!({
            "type": "direct", "title": "x", "stream_url": "https://x.example.com/a.mkv",
            "audio_languages": [],
            "subtitles": [{ "id": "en", "url": "https://x.example.com/en.vtt", "lang_code": "en", "language": "English", "title": "English", "format": "sub" }]
        });
        let result = validate_stream_item(&with_bad_subs);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("Embedded 'subtitles'")));
    }

    #[test]
    fn health_pong_contract() {
        let valid = json!({
            "ok": true,
            "addon_id": "dev.fearless.rosseta",
            "name": "Rosseta",
            "version": "0.1.0"
        });
        assert!(validate_health_pong(&valid).valid);

        let typed = HealthPong::new("dev.fearless.rosseta", "Rosseta", "0.1.0");
        let raw = serde_json::to_value(&typed).unwrap();
        assert_eq!(raw, valid);
        assert!(validate_health_pong(&raw).valid);

        // Extra / legacy fields (e.g. protocol_version) are rejected under strict contract
        let with_protocol = json!({
            "ok": true,
            "addon_id": "dev.fearless.rosseta",
            "name": "Rosseta",
            "version": "0.1.0",
            "protocol_version": "2.0"
        });
        assert!(!validate_health_pong(&with_protocol).valid);

        // ok: false is invalid
        let not_ok = json!({
            "ok": false,
            "addon_id": "dev.fearless.rosseta",
            "name": "Rosseta",
            "version": "0.1.0"
        });
        assert!(!validate_health_pong(&not_ok).valid);

        // Bad addon_id (not reverse-DNS)
        let bad_id = json!({
            "ok": true,
            "addon_id": "rosseta",
            "name": "Rosseta",
            "version": "0.1.0"
        });
        assert!(!validate_health_pong(&bad_id).valid);

        // Bad version (not semver)
        let bad_version = json!({
            "ok": true,
            "addon_id": "dev.fearless.rosseta",
            "name": "Rosseta",
            "version": "v0.1"
        });
        assert!(!validate_health_pong(&bad_version).valid);

        // Missing field
        let missing = json!({
            "ok": true,
            "addon_id": "dev.fearless.rosseta",
            "version": "0.1.0"
        });
        assert!(!validate_health_pong(&missing).valid);
    }
}

