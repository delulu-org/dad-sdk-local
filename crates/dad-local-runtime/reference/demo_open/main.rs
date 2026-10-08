//! Demo Open — M1 conformance addon (no key gate).
//!
//! Deliberate test hooks, keyed by `tmdb_id` so the spawn tests can drive
//! every protocol path:
//! - `13`  → handler PANICS → runtime answers `internal_error`
//! - `42`  → handler returns an http:// stream → runtime answers
//!   `invalid_response` (the malformed payload never reaches stdout)
//! - `7`   → handler returns a graceful `content_unavailable`
//! - other → one direct + one torrent item, plus `getMeta` enrichment

#![forbid(unsafe_code)]

use dad_local_runtime::prelude::*;

#[derive(Default)]
struct DemoOpen;

impl GetStreamsHandler for DemoOpen {
    async fn get_streams(&self, request: DadRequest) -> Result<Vec<StreamItem>, DadError> {
        match request.tmdb_id {
            13 => panic!("demo panic - the runtime must contain this"),
            42 => Ok(vec![StreamItem {
                item_type: DadStreamType::Direct,
                title: "Bad http stream (contract violation on purpose)".to_string(),
                stream_url: Some("http://not-https.example.com/video.mp4".to_string()),
                audio_languages: vec![],
                ..Default::default()
            }]),
            7 => Err(DadError::new(
                DadErrorCode::ContentUnavailable,
                "No streams for this title",
            )),
            _ => Ok(vec![
                StreamItem::direct("Demo 1080p", "https://cdn.example.com/demo.m3u8"),
                StreamItem::torrent(
                    "Demo.2160p.Remux",
                    "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678",
                    0,
                ),
            ]),
        }
    }
}

impl GetMetaHandler for DemoOpen {
    async fn get_meta(&self, _request: DadRequest) -> Result<Option<MetaResponse>, DadError> {
        Ok(Some(MetaResponse {
            imdb_id: Some("tt0111113".to_string()),
            imdb_rating: Some(8.8),
            trailers: Some(vec!["https://cdn.example.com/trailer.mp4".to_string()]),
        }))
    }
}

// NOTE: no GetSubtitlesHandler — and the manifest does not declare `subtitle`,
// so both directions of the compile-time capability check stay quiet. Try
// adding `impl GetSubtitlesHandler for DemoOpen` and watch it fail to compile.
// (Or add "subtitle" to manifest.json capabilities without the impl — also a
// compile error.)

dad_local_runtime::define_local_addon! {
    manifest = "reference/demo_open/manifest.json";
    addon = DemoOpen;
}
