//! Public-domain / freely-licensed test fixtures used by `dad-local dev` and
//! `dad-local test` — the local port of dad-sdk's `fixtures.ts`.
//!
//! These titles are legally safe to request from any addon, anywhere — no
//! copyrighted content is ever probed:
//! - The Blender Foundation open movies (Elephants Dream, Big Buck Bunny,
//!   Sintel, Tears of Steel) are released under CC-BY.
//! - Night of the Living Dead (1968) is in the US public domain.
//! - The Beverly Hillbillies (1962) episodes fell into the US public domain
//!   for lack of copyright renewal.
//!
//! TMDB IDs verified against the TMDB movie/TV pages.

use crate::responses::MediaType;

/// One probe target: a title the CLI requests from the addon under test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DadTestFixture {
    /// Display title for probe output.
    pub title: &'static str,
    /// Movie or TV.
    pub media_type: MediaType,
    /// TMDB id sent as `tmdb_id`.
    pub tmdb_id: u32,
    /// Season — TV only.
    pub season: Option<u32>,
    /// Episode — TV only.
    pub episode: Option<u32>,
}

/// The shared fixture set. Every run of `dad-local test` probes all of these
/// against every declared capability, exactly like `dad test` does for HTTP
/// addons — so a clean local run and a clean deployed run mean the same thing.
pub const DAD_TEST_FIXTURES: [DadTestFixture; 6] = [
    DadTestFixture {
        title: "Elephants Dream (2006)",
        media_type: MediaType::Movie,
        tmdb_id: 9761,
        season: None,
        episode: None,
    },
    DadTestFixture {
        title: "Big Buck Bunny (2008)",
        media_type: MediaType::Movie,
        tmdb_id: 10378,
        season: None,
        episode: None,
    },
    DadTestFixture {
        title: "Sintel (2010)",
        media_type: MediaType::Movie,
        tmdb_id: 45745,
        season: None,
        episode: None,
    },
    DadTestFixture {
        title: "Tears of Steel (2012)",
        media_type: MediaType::Movie,
        tmdb_id: 133701,
        season: None,
        episode: None,
    },
    DadTestFixture {
        title: "Night of the Living Dead (1968)",
        media_type: MediaType::Movie,
        tmdb_id: 10331,
        season: None,
        episode: None,
    },
    DadTestFixture {
        title: "The Beverly Hillbillies (1962)",
        media_type: MediaType::Tv,
        tmdb_id: 1930,
        season: Some(1),
        episode: Some(1),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_carry_valid_ids() {
        for fixture in &DAD_TEST_FIXTURES {
            assert!(fixture.tmdb_id > 0, "{} must have a real TMDB id", fixture.title);
            let tv = fixture.media_type == MediaType::Tv;
            assert_eq!(
                fixture.season.is_some(),
                tv,
                "{}: season presence must match media_type",
                fixture.title
            );
            assert_eq!(
                fixture.episode.is_some(),
                tv,
                "{}: episode presence must match media_type",
                fixture.title
            );
        }
    }
}
