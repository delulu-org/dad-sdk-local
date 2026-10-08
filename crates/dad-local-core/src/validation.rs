//! Shared validation primitives + the result shape every contract validator
//! returns.
//!
//! Everything here is hand-rolled on purpose: the core crate's only
//! dependencies are `serde` + `serde_json`, so "is this an HTTPS URL" and
//! friends are implemented without the `url` or `regex` crates. The trade-off
//! is documented per function; the host (which may use whatever crates it
//! likes) should treat these as the CONTRACT definitions and may implement
//! them more liberally as long as the golden conformance vectors (M3) pass.

use serde_json::Value;
use std::fmt;

/// The universal validator result: dad-sdk's `{ valid, errors }` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// True when zero errors were recorded.
    pub valid: bool,
    /// Every violation found, each a specific human-readable message.
    pub errors: Vec<String>,
}

impl Validation {
    /// A passing result.
    pub fn ok() -> Self {
        Validation { valid: true, errors: Vec::new() }
    }

    /// A failing result from one or more messages.
    pub fn fail<I, S>(messages: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let errors: Vec<String> = messages.into_iter().map(Into::into).collect();
        Validation { valid: false, errors }
    }

    /// A failing result from a single message.
    pub fn error(message: impl Into<String>) -> Self {
        Validation::fail([message])
    }

    /// Append another validator's outcome (flattening its messages).
    pub fn merge(&mut self, other: Validation) {
        if !other.valid {
            self.valid = false;
            self.errors.extend(other.errors);
        }
    }

    /// Prefix every message (used to locate which array item failed).
    pub fn prefix(mut self, prefix: &str) -> Self {
        self.errors = self.errors.into_iter().map(|e| format!("{prefix}{e}")).collect();
        self
    }
}

impl fmt::Display for Validation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.valid {
            write!(f, "valid")
        } else {
            write!(f, "{}", self.errors.join("; "))
        }
    }
}

/// Human-readable JSON type name for error messages.
pub(crate) fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// True only for a well-formed HTTPS URL.
///
/// Hand-rolled (the TS SDK uses `new URL()`): requires the `https://` scheme,
/// a non-empty authority containing at least one alphanumeric character, and
/// no whitespace anywhere. Anything that would need a real URL parser to
/// decide is deliberately left to the conformance vectors — the contract only
/// needs "obviously not HTTPS" to fail here.
pub fn is_https_url(value: &str) -> bool {
    // Scheme is case-insensitive per RFC 3986 (`HTTPS://` is legal); the
    // authority/host case is left untouched — hosts are case-insensitive too
    // and normalizing them here would only create a second source of truth.
    if value.len() < 8 || !value[..8].eq_ignore_ascii_case("https://") {
        return false;
    }
    let rest = &value[8..];
    if value.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    // "https://", "https://.", "https://.." etc. carry no real host.
    !authority.is_empty() && authority.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Same as [`is_https_url`], but the URL must be a bare ORIGIN: no path
/// (a single trailing `/` is tolerated), no query string, no fragment.
///
/// Used for `download_url`-style pointers where the value must be an origin
/// plus nothing else. (In the local contract this guards fields like the
/// manifest's per-asset `download_url`, which points at a file — those use
/// plain [`is_https_url`]; origin-only is kept for future host-side fields
/// and mirrors dad-sdk's `isBareHttpsUrl` for `baseUrl`.)
pub fn is_bare_https_origin(value: &str) -> bool {
    if !is_https_url(value) {
        return false;
    }
    let rest = &value["https://".len()..];
    if rest.contains('?') || rest.contains('#') {
        return false;
    }
    match rest.find('/') {
        None => true,
        Some(i) => {
            let path = &rest[i..];
            path == "/" || path.chars().all(|c| c == '/')
        }
    }
}

/// Standard-alphabet base64 decode, implemented in-crate so the signature
/// shape check stays dependency-free. Strict: canonical padding only.
pub fn decode_standard_base64(input: &str) -> Result<Vec<u8>, String> {
    fn value_of(byte: u8) -> Result<u32, String> {
        match byte {
            b'A'..=b'Z' => Ok((byte - b'A') as u32),
            b'a'..=b'z' => Ok((byte - b'a' + 26) as u32),
            b'0'..=b'9' => Ok((byte - b'0' + 52) as u32),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 character '{}'", byte as char)),
        }
    }

    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return Err("empty base64 string".to_string());
    }
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".to_string());
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 {
        return Err("too much base64 padding".to_string());
    }
    let body_len = bytes.len() - padding;
    if bytes[..body_len].contains(&b'=') {
        return Err("base64 padding inside the body".to_string());
    }

    let mut out = Vec::with_capacity(body_len * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut seen: usize = 0;
    for &byte in &bytes[..body_len] {
        acc = (acc << 6) | value_of(byte)?;
        seen += 1;
        if seen == 4 {
            out.push((acc >> 16) as u8);
            out.push((acc >> 8) as u8);
            out.push(acc as u8);
            acc = 0;
            seen = 0;
        }
    }
    match seen {
        0 => {}
        2 => out.push((acc >> 4) as u8),
        3 => {
            out.push((acc >> 10) as u8);
            out.push((acc >> 2) as u8);
        }
        _ => return Err("invalid base64 length".to_string()),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls() {
        for good in [
            "https://example.com",
            "https://example.com/file.exe",
            "https://sub.domain.example.com/a/b?x=1#frag",
            "https://delulu-addons.pages.dev/default_addon_logo.png",
            "HTTPS://example.com",
        ] {
            assert!(is_https_url(good), "expected '{good}' valid");
        }
        for bad in [
            "", "http://example.com", "https://", "https://.", "https://..",
            "https:///path", "javascript:alert(1)",
            "https://ex ample.com", "ftp://example.com", "//example.com",
            "https://example.com\n", "file:///C:/evil.exe",
        ] {
            assert!(!is_https_url(bad), "expected '{bad}' invalid");
        }
    }

    #[test]
    fn bare_origins() {
        for good in ["https://example.com", "https://example.com/"] {
            assert!(is_bare_https_origin(good), "expected '{good}' valid");
        }
        for bad in [
            "https://example.com/sub", "https://example.com?a=b",
            "https://example.com#frag", "http://example.com",
        ] {
            assert!(!is_bare_https_origin(bad), "expected '{bad}' invalid");
        }
    }

    #[test]
    fn base64_roundtrip_and_rejects() {
        // 64 zero bytes -> 88 canonical chars ending "==".
        let encoded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";
        let decoded = decode_standard_base64(encoded).unwrap();
        assert_eq!(decoded, vec![0u8; 64]);

        assert_eq!(decode_standard_base64("aGVsbG8=").unwrap(), b"hello");
        assert!(decode_standard_base64("!!!!").is_err());
        assert!(decode_standard_base64("abc").is_err()); // len % 4 != 0
        assert!(decode_standard_base64("=abc").is_err()); // padding inside body
        assert!(decode_standard_base64("ab==cd==").is_err());
    }
}
