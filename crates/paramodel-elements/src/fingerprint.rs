// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! BLAKE3 content fingerprints.
//!
//! Per SRD-0003 D7, content-addressed identifiers use BLAKE3 with a
//! lowercase-hex full representation and a `b3:` short form for UI. The
//! `Fingerprint` type wraps the 32-byte digest; `FingerprintBuilder` wraps
//! a `blake3::Hasher` so callers can stream canonical bytes into a hash
//! without materialising them.

use std::fmt;

use serde::{Deserialize, Serialize, de};

/// A 32-byte BLAKE3 content fingerprint.
///
/// Display renders lowercase hex (64 characters). `Debug` and
/// [`Self::display_short`] render the UI-friendly `b3:<12 hex>` form.
/// Serialisation uses the lowercase hex string (so JSON fingerprints are
/// human-inspectable).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Construct from an existing 32-byte digest.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Hash the given bytes as a one-shot.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Borrow the raw digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Render the short `b3:<12 hex>` form for UI and logs.
    #[must_use]
    pub fn display_short(&self) -> String {
        let mut out = String::with_capacity(3 + 12);
        out.push_str("b3:");
        for byte in &self.0[..6] {
            let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02x}"));
        }
        out
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short form keeps log lines readable.
        write!(f, "Fingerprint({})", self.display_short())
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for Fingerprint {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Fingerprint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = <&str>::deserialize(deserializer)?;
        parse_hex(s).map(Self).map_err(de::Error::custom)
    }
}

fn parse_hex(s: &str) -> Result<[u8; 32], &'static str> {
    if s.len() != 64 {
        return Err("expected a 64-character hex string");
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for i in 0..32 {
        let hi = hex_digit(bytes[i * 2])?;
        let lo = hex_digit(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

const fn hex_digit(b: u8) -> Result<u8, &'static str> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err("non-hex character in fingerprint"),
    }
}

/// Streaming fingerprint builder.
///
/// Wraps a `blake3::Hasher` so callers can write canonical bytes into a
/// hash incrementally without materialising them first.
#[derive(Debug, Default)]
pub struct FingerprintBuilder {
    hasher: blake3::Hasher,
}

impl FingerprintBuilder {
    /// Start a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes into the hash.
    #[must_use]
    pub fn update(mut self, data: &[u8]) -> Self {
        self.hasher.update(data);
        self
    }

    /// Feed a single byte into the hash. Convenient for tag bytes and
    /// discriminators.
    #[must_use]
    pub fn byte(self, byte: u8) -> Self {
        self.update(&[byte])
    }

    /// Feed a UTF-8 string preceded by a 4-byte little-endian length prefix.
    #[must_use]
    pub fn length_prefixed_str(self, s: &str) -> Self {
        let len = u32::try_from(s.len()).expect("string length fits in u32");
        self.u32_le(len).update(s.as_bytes())
    }

    /// Feed a `u32` as four little-endian bytes.
    #[must_use]
    pub fn u32_le(self, v: u32) -> Self {
        self.update(&v.to_le_bytes())
    }

    /// Feed a `u64` as eight little-endian bytes.
    #[must_use]
    pub fn u64_le(self, v: u64) -> Self {
        self.update(&v.to_le_bytes())
    }

    /// Feed an `i64` as eight little-endian bytes.
    #[must_use]
    pub fn i64_le(self, v: i64) -> Self {
        self.update(&v.to_le_bytes())
    }

    /// Finalise the hash.
    #[must_use]
    pub fn finish(self) -> Fingerprint {
        Fingerprint(*self.hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn of_is_deterministic() {
        let a = Fingerprint::of(b"hello");
        let b = Fingerprint::of(b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn of_distinguishes_inputs() {
        let a = Fingerprint::of(b"hello");
        let b = Fingerprint::of(b"hellp");
        assert_ne!(a, b);
    }

    #[test]
    fn display_is_64_lowercase_hex() {
        let fp = Fingerprint::of(b"");
        let s = format!("{fp}");
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn display_short_has_b3_prefix_and_12_hex_chars() {
        let fp = Fingerprint::of(b"hello");
        let s = fp.display_short();
        assert!(s.starts_with("b3:"));
        assert_eq!(s.len(), 3 + 12);
        assert!(s[3..].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn builder_matches_oneshot() {
        let one_shot = Fingerprint::of(b"hello world");
        let streamed = FingerprintBuilder::new()
            .update(b"hello")
            .update(b" ")
            .update(b"world")
            .finish();
        assert_eq!(one_shot, streamed);
    }

    #[test]
    fn serde_roundtrip_via_hex() {
        let fp = Fingerprint::of(b"serde-me");
        let json = serde_json::to_string(&fp).unwrap();
        let back: Fingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(fp, back);
        // Serialised form is the full 64-char lowercase hex string.
        let expected = format!("\"{fp}\"");
        assert_eq!(json, expected);
    }

    #[test]
    fn deserialising_non_hex_fails() {
        let err: Result<Fingerprint, _> =
            serde_json::from_str("\"not-a-fingerprint-at-all-only-hex-please-64-chars-x\"");
        assert!(err.is_err());
    }

    #[test]
    fn builder_tag_and_length_prefixed_str() {
        let direct = FingerprintBuilder::new()
            .byte(0x42)
            .length_prefixed_str("abc")
            .finish();
        let manual = FingerprintBuilder::new()
            .update(&[0x42])
            .update(&3u32.to_le_bytes())
            .update(b"abc")
            .finish();
        assert_eq!(direct, manual);
    }
}
