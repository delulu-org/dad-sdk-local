//! DAD addon versioning.
//!
//! A DAD manifest version is STRICT semantic `major.minor.patch` (e.g. "1.2.0"),
//! nothing else — no prerelease suffixes, no single "2", no "v1.2", and no
//! leading zeros on any segment ("01.2.0" is invalid).
//!
//! There is exactly ONE version per addon: the `version` inside the addon's own
//! `manifest.json`, which is what the host acts on after fetching, validating,
//! and signature-verifying it at install time. A catalog's `version` field is a
//! DISCOVERY copy only.

/// True when `version` is a strict `major.minor.patch` semantic version string.
///
/// Each segment is either the single digit `0` or a non-zero digit followed by
/// any digits — so "1.2.3" and "0.0.0" pass, while "01.2.3", "1.2", "1.2.3-rc"
/// and "v1.2.3" all fail.
pub fn is_valid_version(version: &str) -> bool {
    let segments: Vec<&str> = version.split('.').collect();
    segments.len() == 3 && segments.iter().all(|s| is_version_segment(s))
}

fn is_version_segment(segment: &str) -> bool {
    if segment.is_empty() {
        return false;
    }
    if segment == "0" {
        return true;
    }
    // No leading zeros: a segment longer than one digit must not start with '0'.
    !segment.starts_with('0') && segment.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::is_valid_version;

    #[test]
    fn accepts_strict_semver() {
        for good in ["0.0.0", "1.0.0", "1.2.3", "2.1.0", "10.20.30"] {
            assert!(is_valid_version(good), "expected '{good}' to be valid");
        }
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "", "1", "1.2", "1.2.3.4", "01.2.3", "1.02.3", "1.2.03", "v1.2.3", "1.2.3-rc",
            "1.2.x", "1..3", " 1.2.3", "1.2.3 ",
        ] {
            assert!(!is_valid_version(bad), "expected '{bad}' to be invalid");
        }
    }
}
