//! Demo Gated — M1 conformance addon for the enforced api_key gate.
//!
//! The runtime rejects keyless requests with `unauthorized` BEFORE the
//! handler runs (this file's handler is never invoked without `auth`).
//! A PRESENT key's validity is the author's business: here `auth != "good-key"`
//! is answered as `unauthorized` by the handler itself, mirroring how a
//! real addon would call its upstream.

#![forbid(unsafe_code)]

use dad_local_runtime::prelude::*;

#[derive(Default)]
struct DemoGated;

impl GetSubtitlesHandler for DemoGated {
    async fn get_subtitles(&self, request: DadRequest) -> Result<Vec<SubtitleItem>, DadError> {
        match request.auth.as_deref() {
            Some("good-key") => Ok(vec![SubtitleItem {
                id: "en-sdh".to_string(),
                url: "https://cdn.example.com/en-sdh.vtt".to_string(),
                lang_code: "en".to_string(),
                language: "English".to_string(),
                title: "English [SDH]".to_string(),
                format: SubtitleFormat::Vtt,
                provider: Some("OpenSubtitles".to_string()),
            }]),
            _ => Err(DadError::new(
                DadErrorCode::Unauthorized,
                "Missing or invalid API key",
            )),
        }
    }
}

dad_local_runtime::define_local_addon! {
    manifest = "reference/demo_gated/manifest.json";
    addon = DemoGated;
}
