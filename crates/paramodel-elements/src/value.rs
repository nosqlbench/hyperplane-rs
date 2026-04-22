// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Parameter values, provenance, and canonical fingerprints.
//!
//! A `Value` is the observation of a `Parameter` at a particular point in
//! a trial or binding. Each concrete variant owns the native Rust value
//! plus a shared [`Provenance`] that records the owning parameter name,
//! the instant the value was generated, an optional generator tag, and
//! a BLAKE3 fingerprint over the canonical byte form.
//!
//! Canonical form (per SRD-0004 §Fingerprinting) is a tag byte, then the
//! parameter name as UTF-8 with a `0x00` terminator, then the per-kind
//! payload. The full byte layout is shipped with each variant's
//! `fingerprint_of` helper so downstream tools can reproduce the hash
//! without depending on this crate.
//!
//! Constructors always compute the fingerprint. `Value::verify_fingerprint`
//! re-derives it and reports mismatches; callers run this at trust
//! boundaries where tampering matters.

use indexmap::IndexSet;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::fingerprint::{Fingerprint, FingerprintBuilder};
use crate::names::{NameError, ParameterName};

// ---------------------------------------------------------------------------
// Canonical tag bytes.
// ---------------------------------------------------------------------------

const TAG_INTEGER:   u8 = 0x01;
const TAG_DOUBLE:    u8 = 0x02;
const TAG_BOOLEAN:   u8 = 0x03;
const TAG_STRING:    u8 = 0x04;
const TAG_SELECTION: u8 = 0x05;

/// Canonical quiet-NaN bit pattern.
///
/// Floats are hashed by their `to_le_bytes()`, which diverges for
/// different NaN payloads. We fold every NaN input to this one pattern
/// before hashing so `NaN` values fingerprint identically regardless of
/// the arithmetic that produced them.
const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

const fn canonicalise_f64(v: f64) -> f64 {
    if v.is_nan() {
        f64::from_bits(CANONICAL_NAN_BITS)
    } else {
        v
    }
}

// ---------------------------------------------------------------------------
// ValueKind discriminator.
// ---------------------------------------------------------------------------

/// Unit-only discriminator for a [`Value`] variant.
///
/// Useful when talking about "this is a Selection value" without
/// borrowing the whole enum. Serialises as a lowercase tag
/// (`"integer"`, `"double"`, `"boolean"`, `"string"`, `"selection"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// 64-bit signed integer.
    Integer,
    /// IEEE-754 `f64`.
    Double,
    /// Boolean.
    Boolean,
    /// UTF-8 string.
    String,
    /// Ordered multi-item selection from a registered domain.
    Selection,
}

// ---------------------------------------------------------------------------
// SelectionItem newtype.
// ---------------------------------------------------------------------------

/// A validated member of a selection domain.
///
/// Items are non-empty UTF-8 strings with no ASCII control characters.
/// Constructor-side validation means a `SelectionItem` is always safe to
/// hash into a canonical form.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SelectionItem(String);

impl SelectionItem {
    /// Construct a new item, validating the candidate string.
    pub fn new(candidate: impl Into<String>) -> Result<Self, NameError> {
        let s = candidate.into();
        if s.is_empty() {
            return Err(NameError::Empty);
        }
        for (offset, ch) in s.char_indices() {
            if ch.is_control() {
                return Err(NameError::InvalidChar { ch, offset });
            }
        }
        Ok(Self(s))
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consume and return the inner string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Display for SelectionItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for SelectionItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SelectionItem({:?})", self.0)
    }
}

impl AsRef<str> for SelectionItem {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Serialize for SelectionItem {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SelectionItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Generator provenance tags.
// ---------------------------------------------------------------------------

/// Which boundary a [`GeneratorInfo::Boundary`] value picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryKind {
    /// Numeric minimum.
    Min,
    /// Numeric maximum.
    Max,
    /// First item of an ordered set.
    First,
    /// Last item of an ordered set.
    Last,
}

/// How a value was produced.
///
/// Parallels upstream's generator metadata. Stored inside [`Provenance`]
/// as an option: a value constructed by a caller that doesn't care about
/// the source leaves it `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorInfo {
    /// Supplied verbatim by the caller.
    Explicit,
    /// Taken from the owning parameter's default.
    Default,
    /// Picked from the domain's boundary set.
    Boundary {
        /// Which boundary was selected.
        which: BoundaryKind,
    },
    /// Drawn pseudo-randomly; seed is recorded when known.
    Random {
        /// RNG seed, if the caller tracked it.
        seed: Option<u64>,
    },
    /// Computed from a derived parameter's expression.
    Derived {
        /// Source form of the derivation expression.
        expression: String,
    },
}

// ---------------------------------------------------------------------------
// Provenance.
// ---------------------------------------------------------------------------

/// Shared metadata attached to every [`Value`].
///
/// Values of any kind carry the same provenance: the owning parameter,
/// an observation timestamp, the optional generator tag, and the
/// canonical fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// Name of the parameter this value observes.
    pub parameter:    ParameterName,
    /// When the value was constructed.
    pub generated_at: Timestamp,
    /// How the value was produced, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator:    Option<GeneratorInfo>,
    /// Canonical fingerprint of (kind, parameter, value bytes).
    pub fingerprint:  Fingerprint,
}

// ---------------------------------------------------------------------------
// Per-kind value structs.
// ---------------------------------------------------------------------------

/// An observed `i64` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegerValue {
    /// The observed number.
    pub value:      i64,
    /// Shared provenance.
    pub provenance: Provenance,
}

impl IntegerValue {
    /// Construct, computing the fingerprint and stamping "now".
    #[must_use]
    pub fn new(name: ParameterName, value: i64, generator: Option<GeneratorInfo>) -> Self {
        Self::new_at(name, value, generator, Timestamp::now())
    }

    /// Construct with an explicit timestamp (deterministic in tests).
    #[must_use]
    pub fn new_at(
        name:      ParameterName,
        value:     i64,
        generator: Option<GeneratorInfo>,
        now:       Timestamp,
    ) -> Self {
        let fingerprint = Self::fingerprint_of(&name, value);
        Self {
            value,
            provenance: Provenance {
                parameter: name,
                generated_at: now,
                generator,
                fingerprint,
            },
        }
    }

    /// Canonical fingerprint for an integer value.
    #[must_use]
    pub fn fingerprint_of(name: &ParameterName, value: i64) -> Fingerprint {
        FingerprintBuilder::new()
            .byte(TAG_INTEGER)
            .update(name.as_str().as_bytes())
            .byte(0x00)
            .i64_le(value)
            .finish()
    }
}

/// An observed `f64` value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubleValue {
    /// The observed number. `NaN` is normalised before fingerprinting.
    pub value:      f64,
    /// Shared provenance.
    pub provenance: Provenance,
}

impl DoubleValue {
    /// Construct, computing the fingerprint and stamping "now".
    #[must_use]
    pub fn new(name: ParameterName, value: f64, generator: Option<GeneratorInfo>) -> Self {
        Self::new_at(name, value, generator, Timestamp::now())
    }

    /// Construct with an explicit timestamp (deterministic in tests).
    #[must_use]
    pub fn new_at(
        name:      ParameterName,
        value:     f64,
        generator: Option<GeneratorInfo>,
        now:       Timestamp,
    ) -> Self {
        let fingerprint = Self::fingerprint_of(&name, value);
        Self {
            value: canonicalise_f64(value),
            provenance: Provenance {
                parameter: name,
                generated_at: now,
                generator,
                fingerprint,
            },
        }
    }

    /// Canonical fingerprint for a double value. `NaN` collapses to the
    /// canonical quiet-NaN pattern first.
    #[must_use]
    pub fn fingerprint_of(name: &ParameterName, value: f64) -> Fingerprint {
        let canonical = canonicalise_f64(value);
        FingerprintBuilder::new()
            .byte(TAG_DOUBLE)
            .update(name.as_str().as_bytes())
            .byte(0x00)
            .update(&canonical.to_le_bytes())
            .finish()
    }
}

/// An observed `bool` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooleanValue {
    /// The observed flag.
    pub value:      bool,
    /// Shared provenance.
    pub provenance: Provenance,
}

impl BooleanValue {
    /// Construct, computing the fingerprint and stamping "now".
    #[must_use]
    pub fn new(name: ParameterName, value: bool, generator: Option<GeneratorInfo>) -> Self {
        Self::new_at(name, value, generator, Timestamp::now())
    }

    /// Construct with an explicit timestamp.
    #[must_use]
    pub fn new_at(
        name:      ParameterName,
        value:     bool,
        generator: Option<GeneratorInfo>,
        now:       Timestamp,
    ) -> Self {
        let fingerprint = Self::fingerprint_of(&name, value);
        Self {
            value,
            provenance: Provenance {
                parameter: name,
                generated_at: now,
                generator,
                fingerprint,
            },
        }
    }

    /// Canonical fingerprint for a boolean value.
    #[must_use]
    pub fn fingerprint_of(name: &ParameterName, value: bool) -> Fingerprint {
        FingerprintBuilder::new()
            .byte(TAG_BOOLEAN)
            .update(name.as_str().as_bytes())
            .byte(0x00)
            .byte(u8::from(value))
            .finish()
    }
}

/// An observed `String` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringValue {
    /// The observed text.
    pub value:      String,
    /// Shared provenance.
    pub provenance: Provenance,
}

impl StringValue {
    /// Construct, computing the fingerprint and stamping "now".
    #[must_use]
    pub fn new(name: ParameterName, value: impl Into<String>, generator: Option<GeneratorInfo>) -> Self {
        Self::new_at(name, value, generator, Timestamp::now())
    }

    /// Construct with an explicit timestamp.
    #[must_use]
    pub fn new_at(
        name:      ParameterName,
        value:     impl Into<String>,
        generator: Option<GeneratorInfo>,
        now:       Timestamp,
    ) -> Self {
        let value = value.into();
        let fingerprint = Self::fingerprint_of(&name, &value);
        Self {
            value,
            provenance: Provenance {
                parameter: name,
                generated_at: now,
                generator,
                fingerprint,
            },
        }
    }

    /// Canonical fingerprint for a string value. Payload is a
    /// `u32` LE length followed by the UTF-8 bytes.
    #[must_use]
    pub fn fingerprint_of(name: &ParameterName, value: &str) -> Fingerprint {
        FingerprintBuilder::new()
            .byte(TAG_STRING)
            .update(name.as_str().as_bytes())
            .byte(0x00)
            .length_prefixed_str(value)
            .finish()
    }
}

/// An observed selection value.
///
/// Stored as an `IndexSet` so duplicates are impossible and authored
/// order is preserved. Canonical form sorts items lexicographically
/// before hashing so two selections that differ only in authored order
/// fingerprint the same.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionValue {
    /// The selected items, in authored order.
    pub items:      IndexSet<SelectionItem>,
    /// Shared provenance.
    pub provenance: Provenance,
}

impl SelectionValue {
    /// Construct, computing the fingerprint and stamping "now".
    #[must_use]
    pub fn new(
        name:      ParameterName,
        items:     IndexSet<SelectionItem>,
        generator: Option<GeneratorInfo>,
    ) -> Self {
        Self::new_at(name, items, generator, Timestamp::now())
    }

    /// Construct with an explicit timestamp.
    #[must_use]
    pub fn new_at(
        name:      ParameterName,
        items:     IndexSet<SelectionItem>,
        generator: Option<GeneratorInfo>,
        now:       Timestamp,
    ) -> Self {
        let fingerprint = Self::fingerprint_of(&name, &items);
        Self {
            items,
            provenance: Provenance {
                parameter: name,
                generated_at: now,
                generator,
                fingerprint,
            },
        }
    }

    /// Canonical fingerprint for a selection value. Items sort
    /// lexicographically by UTF-8 bytes before hashing.
    #[must_use]
    pub fn fingerprint_of(name: &ParameterName, items: &IndexSet<SelectionItem>) -> Fingerprint {
        let mut sorted: Vec<&str> = items.iter().map(SelectionItem::as_str).collect();
        sorted.sort_unstable();
        let len = u32::try_from(sorted.len()).expect("selection size fits in u32");
        let mut builder = FingerprintBuilder::new()
            .byte(TAG_SELECTION)
            .update(name.as_str().as_bytes())
            .byte(0x00)
            .u32_le(len);
        for item in sorted {
            builder = builder.length_prefixed_str(item);
        }
        builder.finish()
    }
}

// ---------------------------------------------------------------------------
// The outer Value enum.
// ---------------------------------------------------------------------------

/// An observed parameter value, tagged by kind.
///
/// Serialises with a `kind` discriminator so wire formats are
/// self-describing:
///
/// ```json
/// { "kind": "integer", "value": 42, "provenance": { ... } }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Value {
    /// An `i64` observation.
    Integer(IntegerValue),
    /// An `f64` observation.
    Double(DoubleValue),
    /// A `bool` observation.
    Boolean(BooleanValue),
    /// A `String` observation.
    String(StringValue),
    /// A multi-item selection.
    Selection(SelectionValue),
}

impl Value {
    /// Discriminator for this value.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Integer(_)   => ValueKind::Integer,
            Self::Double(_)    => ValueKind::Double,
            Self::Boolean(_)   => ValueKind::Boolean,
            Self::String(_)    => ValueKind::String,
            Self::Selection(_) => ValueKind::Selection,
        }
    }

    /// Shared provenance for this value.
    #[must_use]
    pub const fn provenance(&self) -> &Provenance {
        match self {
            Self::Integer(v)   => &v.provenance,
            Self::Double(v)    => &v.provenance,
            Self::Boolean(v)   => &v.provenance,
            Self::String(v)    => &v.provenance,
            Self::Selection(v) => &v.provenance,
        }
    }

    /// Owning parameter.
    #[must_use]
    pub const fn parameter(&self) -> &ParameterName {
        &self.provenance().parameter
    }

    /// Canonical fingerprint computed at construction.
    #[must_use]
    pub const fn fingerprint(&self) -> &Fingerprint {
        &self.provenance().fingerprint
    }

    /// Borrow the `i64` payload, if this is an integer value.
    #[must_use]
    pub const fn as_integer(&self) -> Option<i64> {
        if let Self::Integer(v) = self {
            Some(v.value)
        } else {
            None
        }
    }

    /// Borrow the `f64` payload, if this is a double value.
    #[must_use]
    pub const fn as_double(&self) -> Option<f64> {
        if let Self::Double(v) = self {
            Some(v.value)
        } else {
            None
        }
    }

    /// Borrow the `bool` payload, if this is a boolean value.
    #[must_use]
    pub const fn as_boolean(&self) -> Option<bool> {
        if let Self::Boolean(v) = self {
            Some(v.value)
        } else {
            None
        }
    }

    /// Borrow the `str` payload, if this is a string value.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        if let Self::String(v) = self {
            Some(&v.value)
        } else {
            None
        }
    }

    /// Borrow the selection payload, if this is a selection value.
    #[must_use]
    pub const fn as_selection(&self) -> Option<&IndexSet<SelectionItem>> {
        if let Self::Selection(v) = self {
            Some(&v.items)
        } else {
            None
        }
    }

    /// Convenience constructor for an integer value.
    #[must_use]
    pub fn integer(name: ParameterName, value: i64, generator: Option<GeneratorInfo>) -> Self {
        Self::Integer(IntegerValue::new(name, value, generator))
    }

    /// Convenience constructor for a double value.
    #[must_use]
    pub fn double(name: ParameterName, value: f64, generator: Option<GeneratorInfo>) -> Self {
        Self::Double(DoubleValue::new(name, value, generator))
    }

    /// Convenience constructor for a boolean value.
    #[must_use]
    pub fn boolean(name: ParameterName, value: bool, generator: Option<GeneratorInfo>) -> Self {
        Self::Boolean(BooleanValue::new(name, value, generator))
    }

    /// Convenience constructor for a string value.
    #[must_use]
    pub fn string(name: ParameterName, value: impl Into<String>, generator: Option<GeneratorInfo>) -> Self {
        Self::String(StringValue::new(name, value, generator))
    }

    /// Convenience constructor for a selection value.
    #[must_use]
    pub fn selection(
        name:      ParameterName,
        items:     IndexSet<SelectionItem>,
        generator: Option<GeneratorInfo>,
    ) -> Self {
        Self::Selection(SelectionValue::new(name, items, generator))
    }

    /// Recompute the canonical fingerprint from the payload and compare
    /// with the stored provenance fingerprint.
    ///
    /// Returns `true` when they match. Intended for tamper-detection
    /// checkpoints; not run automatically during deserialisation.
    #[must_use]
    pub fn verify_fingerprint(&self) -> bool {
        let expected = match self {
            Self::Integer(v)   => IntegerValue::fingerprint_of(&v.provenance.parameter, v.value),
            Self::Double(v)    => DoubleValue::fingerprint_of(&v.provenance.parameter, v.value),
            Self::Boolean(v)   => BooleanValue::fingerprint_of(&v.provenance.parameter, v.value),
            Self::String(v)    => StringValue::fingerprint_of(&v.provenance.parameter, &v.value),
            Self::Selection(v) => SelectionValue::fingerprint_of(&v.provenance.parameter, &v.items),
        };
        &expected == self.fingerprint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn epoch() -> Timestamp {
        Timestamp::from_second(0).unwrap()
    }

    // ---------- basic construction & accessors ----------

    #[test]
    fn integer_value_roundtrips_accessors() {
        let v = Value::integer(pname("threads"), 42, None);
        assert_eq!(v.kind(), ValueKind::Integer);
        assert_eq!(v.as_integer(), Some(42));
        assert_eq!(v.as_double(), None);
        assert_eq!(v.parameter().as_str(), "threads");
    }

    #[test]
    fn boolean_double_string_accessors() {
        let b = Value::boolean(pname("on"), true, None);
        let d = Value::double(pname("ratio"), 1.5, None);
        let s = Value::string(pname("label"), "hi", None);
        assert_eq!(b.as_boolean(), Some(true));
        assert_eq!(d.as_double(), Some(1.5));
        assert_eq!(s.as_string(), Some("hi"));
    }

    #[test]
    fn selection_value_preserves_authored_order() {
        let mut items = IndexSet::new();
        items.insert(SelectionItem::new("gamma").unwrap());
        items.insert(SelectionItem::new("alpha").unwrap());
        items.insert(SelectionItem::new("beta").unwrap());
        let v = Value::selection(pname("picks"), items, None);
        let got: Vec<&str> = v.as_selection().unwrap().iter().map(SelectionItem::as_str).collect();
        assert_eq!(got, vec!["gamma", "alpha", "beta"]);
    }

    // ---------- fingerprint canonical form ----------

    #[test]
    fn integer_fingerprint_is_deterministic() {
        let a = IntegerValue::fingerprint_of(&pname("x"), 42);
        let b = IntegerValue::fingerprint_of(&pname("x"), 42);
        assert_eq!(a, b);
    }

    #[test]
    fn integer_fingerprint_distinguishes_name_and_value() {
        let base = IntegerValue::fingerprint_of(&pname("x"), 42);
        assert_ne!(base, IntegerValue::fingerprint_of(&pname("y"), 42));
        assert_ne!(base, IntegerValue::fingerprint_of(&pname("x"), 43));
    }

    #[test]
    fn integer_fingerprint_matches_hand_built_bytes() {
        // Reproduce the canonical form independently so a future
        // refactor can't silently change the bytes we hash.
        let name = pname("threads");
        let got = IntegerValue::fingerprint_of(&name, 42);
        let mut bytes = vec![TAG_INTEGER];
        bytes.extend_from_slice(name.as_str().as_bytes());
        bytes.push(0x00);
        bytes.extend_from_slice(&42i64.to_le_bytes());
        let expected = Fingerprint::of(&bytes);
        assert_eq!(got, expected);
    }

    #[test]
    fn double_nan_normalises() {
        let nan_a = f64::NAN;
        // Flip a payload bit to produce a different NaN bit pattern.
        let nan_b = f64::from_bits(f64::NAN.to_bits() ^ 1);
        assert!(nan_a.is_nan() && nan_b.is_nan());
        assert_ne!(nan_a.to_bits(), nan_b.to_bits());

        let fa = DoubleValue::fingerprint_of(&pname("r"), nan_a);
        let fb = DoubleValue::fingerprint_of(&pname("r"), nan_b);
        assert_eq!(fa, fb, "canonical NaN must collapse all payloads");
    }

    #[test]
    fn double_value_stores_canonical_nan() {
        let v = DoubleValue::new_at(
            pname("r"),
            f64::from_bits(f64::NAN.to_bits() ^ 1),
            None,
            epoch(),
        );
        assert_eq!(v.value.to_bits(), CANONICAL_NAN_BITS);
    }

    #[test]
    fn boolean_fingerprint_distinguishes_true_and_false() {
        let t = BooleanValue::fingerprint_of(&pname("b"), true);
        let f = BooleanValue::fingerprint_of(&pname("b"), false);
        assert_ne!(t, f);
    }

    #[test]
    fn string_fingerprint_distinguishes_content() {
        let a = StringValue::fingerprint_of(&pname("s"), "hello");
        let b = StringValue::fingerprint_of(&pname("s"), "hellp");
        assert_ne!(a, b);
    }

    #[test]
    fn selection_fingerprint_is_order_independent() {
        let mut one = IndexSet::new();
        one.insert(SelectionItem::new("alpha").unwrap());
        one.insert(SelectionItem::new("beta").unwrap());

        let mut two = IndexSet::new();
        two.insert(SelectionItem::new("beta").unwrap());
        two.insert(SelectionItem::new("alpha").unwrap());

        let fa = SelectionValue::fingerprint_of(&pname("s"), &one);
        let fb = SelectionValue::fingerprint_of(&pname("s"), &two);
        assert_eq!(fa, fb);
    }

    #[test]
    fn selection_fingerprint_distinguishes_contents() {
        let mut one = IndexSet::new();
        one.insert(SelectionItem::new("alpha").unwrap());
        let mut two = IndexSet::new();
        two.insert(SelectionItem::new("beta").unwrap());
        assert_ne!(
            SelectionValue::fingerprint_of(&pname("s"), &one),
            SelectionValue::fingerprint_of(&pname("s"), &two),
        );
    }

    #[test]
    fn kind_tags_are_disjoint() {
        let name = pname("x");
        let i = IntegerValue::fingerprint_of(&name, 0);
        let d = DoubleValue::fingerprint_of(&name, 0.0);
        let b = BooleanValue::fingerprint_of(&name, false);
        let s = StringValue::fingerprint_of(&name, "");
        let sel = SelectionValue::fingerprint_of(&name, &IndexSet::new());
        let all = [i, d, b, s, sel];
        for (ai, a) in all.iter().enumerate() {
            for (bi, b) in all.iter().enumerate() {
                if ai != bi {
                    assert_ne!(a, b, "kinds {ai} and {bi} collided");
                }
            }
        }
    }

    // ---------- verify_fingerprint ----------

    #[test]
    fn verify_fingerprint_passes_for_constructed_value() {
        let v = Value::integer(pname("x"), 7, None);
        assert!(v.verify_fingerprint());
    }

    #[test]
    fn verify_fingerprint_detects_mutation() {
        let mut iv = IntegerValue::new(pname("x"), 7, None);
        iv.value = 8; // Tamper with the payload.
        let v = Value::Integer(iv);
        assert!(!v.verify_fingerprint());
    }

    // ---------- provenance / generator ----------

    #[test]
    fn generator_is_preserved() {
        let v = Value::integer(
            pname("x"),
            7,
            Some(GeneratorInfo::Random { seed: Some(42) }),
        );
        match v.provenance().generator.as_ref().unwrap() {
            GeneratorInfo::Random { seed } => assert_eq!(*seed, Some(42)),
            other => panic!("wrong generator: {other:?}"),
        }
    }

    #[test]
    fn new_at_uses_supplied_timestamp() {
        let ts = Timestamp::from_second(1_700_000_000).unwrap();
        let v = IntegerValue::new_at(pname("x"), 7, None, ts);
        assert_eq!(v.provenance.generated_at, ts);
    }

    // ---------- serde ----------

    #[test]
    fn serde_roundtrip_integer_value() {
        let ts = Timestamp::from_second(1_700_000_000).unwrap();
        let v = Value::Integer(IntegerValue::new_at(pname("threads"), 42, None, ts));
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
        assert!(back.verify_fingerprint());
    }

    #[test]
    fn serde_roundtrip_selection_value() {
        let ts = Timestamp::from_second(0).unwrap();
        let mut items = IndexSet::new();
        items.insert(SelectionItem::new("alpha").unwrap());
        items.insert(SelectionItem::new("beta").unwrap());
        let v = Value::Selection(SelectionValue::new_at(pname("picks"), items, None, ts));
        let json = serde_json::to_string(&v).unwrap();
        let back: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
        assert!(back.verify_fingerprint());
    }

    #[test]
    fn selection_item_rejects_empty_and_control_chars() {
        assert!(SelectionItem::new("").is_err());
        assert!(SelectionItem::new("hello\nworld").is_err());
        assert!(SelectionItem::new("hello").is_ok());
    }

    #[test]
    fn validation_kind_serialises_as_snake_case() {
        let s = serde_json::to_string(&ValueKind::Selection).unwrap();
        assert_eq!(s, "\"selection\"");
    }
}
