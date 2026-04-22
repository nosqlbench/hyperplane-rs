// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Declarative constraint algebra.
//!
//! Each parameter kind has its own constraint enum with kind-specific
//! leaves plus the shared Boolean combinators (`And`, `Or`, `Not`,
//! `Always`, `Never`; `BoolConstraint` drops `And`/`Or` since the
//! underlying value only takes two states). The outer [`Constraint`]
//! enum discriminates by kind and dispatches `test` by native payload.
//!
//! Canonicalisation (per SRD-0004 D5) collapses identity elements,
//! flattens nested same-kind combinators, peels double negation, and
//! deduplicates children. The final sort step that gives us
//! fingerprint-stable canonical forms requires a total order over every
//! leaf (including `f64` leaves); that lands alongside constraint
//! fingerprinting in a follow-up slice. Until then [`canonicalize`]
//! does everything except sort.
//!
//! [`canonicalize`]: IntConstraint::canonicalize

use std::collections::BTreeSet;

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::domain::RegexPattern;
use crate::fingerprint::Fingerprint;
use crate::value::{SelectionItem, Value};

// ---------------------------------------------------------------------------
// Shared helpers for Boolean-algebra canonicalisation.
// ---------------------------------------------------------------------------

/// Remove adjacent duplicates using `PartialEq`.
///
/// A replacement for `Vec::dedup` that doesn't require `Ord`. After the
/// canonical sort runs, equal children are adjacent, so this collapses
/// every duplicate.
fn dedup_partial_eq<T: PartialEq>(xs: &mut Vec<T>) {
    let mut i = 1;
    while i < xs.len() {
        if xs[i] == xs[i - 1] {
            xs.remove(i);
        } else {
            i += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical byte encoding — shared primitive helpers.
// ---------------------------------------------------------------------------
//
// Every constraint serialises to a stable byte form via write_canonical.
// Per-kind variant tags live in disjoint byte ranges (Int = 0x10..=0x1A,
// Double = 0x20..=0x27, Bool = 0x30..=0x33, String = 0x40..=0x49,
// Selection = 0x50..=0x59), so the outer Constraint's canonical form
// needs no additional discriminator: the first byte already identifies
// the kind. The byte layout is the source of truth for both sort order
// (canonicalisation) and fingerprints.

const CANONICAL_NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

fn write_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_i64_le(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_f64_le_canonical(out: &mut Vec<u8>, v: f64) {
    // Fold every NaN payload to the canonical quiet NaN so equivalent
    // floats serialise identically.
    let v = if v.is_nan() { f64::from_bits(CANONICAL_NAN_BITS) } else { v };
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_str_len_prefixed(out: &mut Vec<u8>, s: &str) {
    let len = u32::try_from(s.len()).expect("string length fits in u32");
    write_u32_le(out, len);
    out.extend_from_slice(s.as_bytes());
}

fn write_i64_set(out: &mut Vec<u8>, values: &BTreeSet<i64>) {
    write_u32_le(out, u32::try_from(values.len()).expect("set size fits in u32"));
    for v in values {
        write_i64_le(out, *v);
    }
}

fn write_string_set(out: &mut Vec<u8>, values: &BTreeSet<String>) {
    write_u32_le(out, u32::try_from(values.len()).expect("set size fits in u32"));
    for v in values {
        write_str_len_prefixed(out, v);
    }
}

fn write_selection_item_set(out: &mut Vec<u8>, values: &BTreeSet<SelectionItem>) {
    write_u32_le(out, u32::try_from(values.len()).expect("set size fits in u32"));
    for v in values {
        write_str_len_prefixed(out, v.as_str());
    }
}

// ---------------------------------------------------------------------------
// IntConstraint.
// ---------------------------------------------------------------------------

/// Integer-value constraint.
///
/// Variants track SRD-0004 §Constraint algebra. `Multiple(0)` is a legal
/// representation but always tests `false`; the constructor [`Self::multiple`]
/// rejects zero so callers that go through the helper never produce it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum IntConstraint {
    /// Always satisfied.
    Always,
    /// Never satisfied.
    Never,
    /// `value >= n`.
    Min {
        /// The inclusive lower bound.
        n: i64,
    },
    /// `value <= n`.
    Max {
        /// The inclusive upper bound.
        n: i64,
    },
    /// `min <= value <= max`.
    Range {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// `value` is one of these.
    InSet {
        /// The permitted values.
        values: BTreeSet<i64>,
    },
    /// `value` is none of these.
    NotInSet {
        /// The forbidden values.
        values: BTreeSet<i64>,
    },
    /// `value % n == 0`. Always `false` for `n == 0`.
    Multiple {
        /// The divisor.
        n: i64,
    },
    /// All children must hold.
    And {
        /// Conjuncts.
        children: Vec<Self>,
    },
    /// At least one child must hold.
    Or {
        /// Disjuncts.
        children: Vec<Self>,
    },
    /// Child must not hold.
    Not {
        /// The negated child.
        child: Box<Self>,
    },
}

impl IntConstraint {
    /// Construct a `Multiple` leaf. Rejects `n == 0`.
    #[must_use]
    pub const fn multiple(n: i64) -> Option<Self> {
        if n == 0 {
            None
        } else {
            Some(Self::Multiple { n })
        }
    }

    /// Test a candidate value.
    #[must_use]
    pub fn test(&self, value: i64) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Min { n } => value >= *n,
            Self::Max { n } => value <= *n,
            Self::Range { min, max } => value >= *min && value <= *max,
            Self::InSet { values } => values.contains(&value),
            Self::NotInSet { values } => !values.contains(&value),
            Self::Multiple { n } => *n != 0 && value % *n == 0,
            Self::And { children } => children.iter().all(|c| c.test(value)),
            Self::Or { children } => children.iter().any(|c| c.test(value)),
            Self::Not { child } => !child.test(value),
        }
    }

    /// Conjunction, flattening nested `And`.
    #[must_use]
    pub fn and(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_and_int(&mut children, self);
        absorb_and_int(&mut children, rhs);
        Self::And { children }
    }

    /// Disjunction, flattening nested `Or`.
    #[must_use]
    pub fn or(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_or_int(&mut children, self);
        absorb_or_int(&mut children, rhs);
        Self::Or { children }
    }

    /// Collapse identities, peel double negation, flatten same-kind
    /// combinators, sort `And`/`Or` children by canonical bytes, and
    /// dedup.
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Not { child } => match child.canonicalize() {
                Self::Not { child: inner } => *inner,
                Self::Always => Self::Never,
                Self::Never => Self::Always,
                other => Self::Not {
                    child: Box::new(other),
                },
            },
            Self::And { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Always => {}
                        Self::Never => return Self::Never,
                        Self::And { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Always,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::And { children: flat },
                }
            }
            Self::Or { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Never => {}
                        Self::Always => return Self::Always,
                        Self::Or { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Never,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::Or { children: flat },
                }
            }
            leaf => leaf,
        }
    }

    /// Canonical byte form of this constraint *as written*.
    ///
    /// Two constraints with the same canonical bytes are structurally
    /// identical. [`Self::fingerprint`] canonicalises first and is
    /// what you want for semantic identity.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Canonical fingerprint — semantically identical constraints hash
    /// to the same value. Auto-canonicalises the tree before hashing.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let canonical = self.clone().canonicalize();
        Fingerprint::of(&canonical.canonical_bytes())
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        const T_ALWAYS:   u8 = 0x10;
        const T_NEVER:    u8 = 0x11;
        const T_MIN:      u8 = 0x12;
        const T_MAX:      u8 = 0x13;
        const T_RANGE:    u8 = 0x14;
        const T_INSET:    u8 = 0x15;
        const T_NOTINSET: u8 = 0x16;
        const T_MULTIPLE: u8 = 0x17;
        const T_AND:      u8 = 0x18;
        const T_OR:       u8 = 0x19;
        const T_NOT:      u8 = 0x1A;

        match self {
            Self::Always => out.push(T_ALWAYS),
            Self::Never => out.push(T_NEVER),
            Self::Min { n } => {
                out.push(T_MIN);
                write_i64_le(out, *n);
            }
            Self::Max { n } => {
                out.push(T_MAX);
                write_i64_le(out, *n);
            }
            Self::Range { min, max } => {
                out.push(T_RANGE);
                write_i64_le(out, *min);
                write_i64_le(out, *max);
            }
            Self::InSet { values } => {
                out.push(T_INSET);
                write_i64_set(out, values);
            }
            Self::NotInSet { values } => {
                out.push(T_NOTINSET);
                write_i64_set(out, values);
            }
            Self::Multiple { n } => {
                out.push(T_MULTIPLE);
                write_i64_le(out, *n);
            }
            Self::And { children } => {
                out.push(T_AND);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Or { children } => {
                out.push(T_OR);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Not { child } => {
                out.push(T_NOT);
                child.write_canonical(out);
            }
        }
    }
}

impl std::ops::Not for IntConstraint {
    type Output = Self;
    fn not(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

fn absorb_and_int(into: &mut Vec<IntConstraint>, c: IntConstraint) {
    match c {
        IntConstraint::And { children } => into.extend(children),
        other => into.push(other),
    }
}

fn absorb_or_int(into: &mut Vec<IntConstraint>, c: IntConstraint) {
    match c {
        IntConstraint::Or { children } => into.extend(children),
        other => into.push(other),
    }
}

// ---------------------------------------------------------------------------
// DoubleConstraint.
// ---------------------------------------------------------------------------

/// Double-precision float constraint.
///
/// No `InSet` or `Multiple` — those are ill-defined on floats (equality
/// is brittle, modulo is non-sensical).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DoubleConstraint {
    /// Always satisfied.
    Always,
    /// Never satisfied.
    Never,
    /// `value >= n`.
    Min {
        /// Inclusive lower bound.
        n: f64,
    },
    /// `value <= n`.
    Max {
        /// Inclusive upper bound.
        n: f64,
    },
    /// `min <= value <= max`.
    Range {
        /// Inclusive lower bound.
        min: f64,
        /// Inclusive upper bound.
        max: f64,
    },
    /// All children must hold.
    And {
        /// Conjuncts.
        children: Vec<Self>,
    },
    /// At least one child must hold.
    Or {
        /// Disjuncts.
        children: Vec<Self>,
    },
    /// Child must not hold.
    Not {
        /// The negated child.
        child: Box<Self>,
    },
}

impl DoubleConstraint {
    /// Test a candidate value. `NaN` inputs always fail.
    #[must_use]
    pub fn test(&self, value: f64) -> bool {
        if value.is_nan() {
            return false;
        }
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Min { n } => value >= *n,
            Self::Max { n } => value <= *n,
            Self::Range { min, max } => value >= *min && value <= *max,
            Self::And { children } => children.iter().all(|c| c.test(value)),
            Self::Or { children } => children.iter().any(|c| c.test(value)),
            Self::Not { child } => !child.test(value),
        }
    }

    /// Conjunction, flattening nested `And`.
    #[must_use]
    pub fn and(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_and_double(&mut children, self);
        absorb_and_double(&mut children, rhs);
        Self::And { children }
    }

    /// Disjunction, flattening nested `Or`.
    #[must_use]
    pub fn or(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_or_double(&mut children, self);
        absorb_or_double(&mut children, rhs);
        Self::Or { children }
    }

    /// Same rule set as [`IntConstraint::canonicalize`].
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Not { child } => match child.canonicalize() {
                Self::Not { child: inner } => *inner,
                Self::Always => Self::Never,
                Self::Never => Self::Always,
                other => Self::Not {
                    child: Box::new(other),
                },
            },
            Self::And { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Always => {}
                        Self::Never => return Self::Never,
                        Self::And { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Always,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::And { children: flat },
                }
            }
            Self::Or { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Never => {}
                        Self::Always => return Self::Always,
                        Self::Or { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Never,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::Or { children: flat },
                }
            }
            leaf => leaf,
        }
    }

    /// Canonical byte form of this constraint *as written*.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Canonical fingerprint — auto-canonicalises first. `NaN` leaves
    /// are folded to the canonical quiet-NaN pattern before hashing.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let canonical = self.clone().canonicalize();
        Fingerprint::of(&canonical.canonical_bytes())
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        const T_ALWAYS: u8 = 0x20;
        const T_NEVER:  u8 = 0x21;
        const T_MIN:    u8 = 0x22;
        const T_MAX:    u8 = 0x23;
        const T_RANGE:  u8 = 0x24;
        const T_AND:    u8 = 0x25;
        const T_OR:     u8 = 0x26;
        const T_NOT:    u8 = 0x27;

        match self {
            Self::Always => out.push(T_ALWAYS),
            Self::Never => out.push(T_NEVER),
            Self::Min { n } => {
                out.push(T_MIN);
                write_f64_le_canonical(out, *n);
            }
            Self::Max { n } => {
                out.push(T_MAX);
                write_f64_le_canonical(out, *n);
            }
            Self::Range { min, max } => {
                out.push(T_RANGE);
                write_f64_le_canonical(out, *min);
                write_f64_le_canonical(out, *max);
            }
            Self::And { children } => {
                out.push(T_AND);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Or { children } => {
                out.push(T_OR);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Not { child } => {
                out.push(T_NOT);
                child.write_canonical(out);
            }
        }
    }
}

impl std::ops::Not for DoubleConstraint {
    type Output = Self;
    fn not(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

fn absorb_and_double(into: &mut Vec<DoubleConstraint>, c: DoubleConstraint) {
    match c {
        DoubleConstraint::And { children } => into.extend(children),
        other => into.push(other),
    }
}

fn absorb_or_double(into: &mut Vec<DoubleConstraint>, c: DoubleConstraint) {
    match c {
        DoubleConstraint::Or { children } => into.extend(children),
        other => into.push(other),
    }
}

// ---------------------------------------------------------------------------
// BoolConstraint.
// ---------------------------------------------------------------------------

/// Boolean-value constraint.
///
/// No `And`/`Or` — with only two possible values every conjunction /
/// disjunction is either a tautology, a contradiction, or equal to one
/// of its leaves. `EqTo(false)` and `EqTo(true)` span the space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BoolConstraint {
    /// Always satisfied.
    Always,
    /// Never satisfied.
    Never,
    /// `value == b`.
    EqTo {
        /// Required value.
        b: bool,
    },
    /// Child must not hold.
    Not {
        /// The negated child.
        child: Box<Self>,
    },
}

impl BoolConstraint {
    /// Test a candidate value.
    #[must_use]
    pub fn test(&self, value: bool) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::EqTo { b } => value == *b,
            Self::Not { child } => !child.test(value),
        }
    }

    /// Peel double negation and fold `Not` over the identity variants.
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Not { child } => match child.canonicalize() {
                Self::Not { child: inner } => *inner,
                Self::Always => Self::Never,
                Self::Never => Self::Always,
                Self::EqTo { b } => Self::EqTo { b: !b },
            },
            other => other,
        }
    }

    /// Canonical byte form of this constraint *as written*.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Canonical fingerprint — auto-canonicalises first.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let canonical = self.clone().canonicalize();
        Fingerprint::of(&canonical.canonical_bytes())
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        const T_ALWAYS: u8 = 0x30;
        const T_NEVER:  u8 = 0x31;
        const T_EQTO:   u8 = 0x32;
        const T_NOT:    u8 = 0x33;

        match self {
            Self::Always => out.push(T_ALWAYS),
            Self::Never => out.push(T_NEVER),
            Self::EqTo { b } => {
                out.push(T_EQTO);
                out.push(u8::from(*b));
            }
            Self::Not { child } => {
                out.push(T_NOT);
                child.write_canonical(out);
            }
        }
    }
}

impl std::ops::Not for BoolConstraint {
    type Output = Self;
    fn not(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

// ---------------------------------------------------------------------------
// StringConstraint.
// ---------------------------------------------------------------------------

/// String-value constraint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StringConstraint {
    /// Always satisfied.
    Always,
    /// Never satisfied.
    Never,
    /// Value must match this regex.
    Regex {
        /// The compiled pattern.
        pattern: RegexPattern,
    },
    /// `min <= value.len() <= max` (byte length).
    LengthRange {
        /// Inclusive lower bound on byte length.
        min: u32,
        /// Inclusive upper bound on byte length.
        max: u32,
    },
    /// `!value.is_empty()`.
    NonEmpty,
    /// Value is in this set.
    InSet {
        /// The permitted values.
        values: BTreeSet<String>,
    },
    /// Value is not in this set.
    NotInSet {
        /// The forbidden values.
        values: BTreeSet<String>,
    },
    /// All children must hold.
    And {
        /// Conjuncts.
        children: Vec<Self>,
    },
    /// At least one child must hold.
    Or {
        /// Disjuncts.
        children: Vec<Self>,
    },
    /// Child must not hold.
    Not {
        /// The negated child.
        child: Box<Self>,
    },
}

impl StringConstraint {
    /// Test a candidate value.
    #[must_use]
    pub fn test(&self, value: &str) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Regex { pattern } => pattern.is_match(value),
            Self::LengthRange { min, max } => {
                let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
                len >= *min && len <= *max
            }
            Self::NonEmpty => !value.is_empty(),
            Self::InSet { values } => values.contains(value),
            Self::NotInSet { values } => !values.contains(value),
            Self::And { children } => children.iter().all(|c| c.test(value)),
            Self::Or { children } => children.iter().any(|c| c.test(value)),
            Self::Not { child } => !child.test(value),
        }
    }

    /// Conjunction, flattening nested `And`.
    #[must_use]
    pub fn and(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_and_string(&mut children, self);
        absorb_and_string(&mut children, rhs);
        Self::And { children }
    }

    /// Disjunction, flattening nested `Or`.
    #[must_use]
    pub fn or(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_or_string(&mut children, self);
        absorb_or_string(&mut children, rhs);
        Self::Or { children }
    }

    /// Same rule set as [`IntConstraint::canonicalize`].
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Not { child } => match child.canonicalize() {
                Self::Not { child: inner } => *inner,
                Self::Always => Self::Never,
                Self::Never => Self::Always,
                other => Self::Not {
                    child: Box::new(other),
                },
            },
            Self::And { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Always => {}
                        Self::Never => return Self::Never,
                        Self::And { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Always,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::And { children: flat },
                }
            }
            Self::Or { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Never => {}
                        Self::Always => return Self::Always,
                        Self::Or { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Never,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::Or { children: flat },
                }
            }
            leaf => leaf,
        }
    }

    /// Canonical byte form of this constraint *as written*.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Canonical fingerprint — auto-canonicalises first. Regex leaves
    /// hash their source pattern (not the compiled automaton).
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let canonical = self.clone().canonicalize();
        Fingerprint::of(&canonical.canonical_bytes())
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        const T_ALWAYS:      u8 = 0x40;
        const T_NEVER:       u8 = 0x41;
        const T_REGEX:       u8 = 0x42;
        const T_LENGTHRANGE: u8 = 0x43;
        const T_NONEMPTY:    u8 = 0x44;
        const T_INSET:       u8 = 0x45;
        const T_NOTINSET:    u8 = 0x46;
        const T_AND:         u8 = 0x47;
        const T_OR:          u8 = 0x48;
        const T_NOT:         u8 = 0x49;

        match self {
            Self::Always => out.push(T_ALWAYS),
            Self::Never => out.push(T_NEVER),
            Self::Regex { pattern } => {
                out.push(T_REGEX);
                write_str_len_prefixed(out, pattern.as_str());
            }
            Self::LengthRange { min, max } => {
                out.push(T_LENGTHRANGE);
                write_u32_le(out, *min);
                write_u32_le(out, *max);
            }
            Self::NonEmpty => out.push(T_NONEMPTY),
            Self::InSet { values } => {
                out.push(T_INSET);
                write_string_set(out, values);
            }
            Self::NotInSet { values } => {
                out.push(T_NOTINSET);
                write_string_set(out, values);
            }
            Self::And { children } => {
                out.push(T_AND);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Or { children } => {
                out.push(T_OR);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Not { child } => {
                out.push(T_NOT);
                child.write_canonical(out);
            }
        }
    }
}

impl std::ops::Not for StringConstraint {
    type Output = Self;
    fn not(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

fn absorb_and_string(into: &mut Vec<StringConstraint>, c: StringConstraint) {
    match c {
        StringConstraint::And { children } => into.extend(children),
        other => into.push(other),
    }
}

fn absorb_or_string(into: &mut Vec<StringConstraint>, c: StringConstraint) {
    match c {
        StringConstraint::Or { children } => into.extend(children),
        other => into.push(other),
    }
}

// ---------------------------------------------------------------------------
// SelectionConstraint.
// ---------------------------------------------------------------------------

/// Constraint over a selection value (an ordered set of items).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SelectionConstraint {
    /// Always satisfied.
    Always,
    /// Never satisfied.
    Never,
    /// Every required item must be selected.
    RequireAll {
        /// The items that must all be present.
        items: BTreeSet<SelectionItem>,
    },
    /// At least one required item must be selected.
    RequireAny {
        /// The items — at least one must be present.
        items: BTreeSet<SelectionItem>,
    },
    /// None of these items may be selected.
    ForbidAll {
        /// The forbidden items.
        items: BTreeSet<SelectionItem>,
    },
    /// `selection.len() >= n`.
    MinSize {
        /// The inclusive minimum size.
        n: u32,
    },
    /// `selection.len() <= n`.
    MaxSize {
        /// The inclusive maximum size.
        n: u32,
    },
    /// All children must hold.
    And {
        /// Conjuncts.
        children: Vec<Self>,
    },
    /// At least one child must hold.
    Or {
        /// Disjuncts.
        children: Vec<Self>,
    },
    /// Child must not hold.
    Not {
        /// The negated child.
        child: Box<Self>,
    },
}

impl SelectionConstraint {
    /// Test a candidate selection.
    #[must_use]
    pub fn test(&self, selection: &IndexSet<SelectionItem>) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::RequireAll { items } => items.iter().all(|i| selection.contains(i)),
            Self::RequireAny { items } => items.iter().any(|i| selection.contains(i)),
            Self::ForbidAll { items } => !items.iter().any(|i| selection.contains(i)),
            Self::MinSize { n } => selection.len() >= *n as usize,
            Self::MaxSize { n } => selection.len() <= *n as usize,
            Self::And { children } => children.iter().all(|c| c.test(selection)),
            Self::Or { children } => children.iter().any(|c| c.test(selection)),
            Self::Not { child } => !child.test(selection),
        }
    }

    /// Conjunction, flattening nested `And`.
    #[must_use]
    pub fn and(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_and_selection(&mut children, self);
        absorb_and_selection(&mut children, rhs);
        Self::And { children }
    }

    /// Disjunction, flattening nested `Or`.
    #[must_use]
    pub fn or(self, rhs: Self) -> Self {
        let mut children = Vec::new();
        absorb_or_selection(&mut children, self);
        absorb_or_selection(&mut children, rhs);
        Self::Or { children }
    }

    /// Same rule set as [`IntConstraint::canonicalize`].
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Not { child } => match child.canonicalize() {
                Self::Not { child: inner } => *inner,
                Self::Always => Self::Never,
                Self::Never => Self::Always,
                other => Self::Not {
                    child: Box::new(other),
                },
            },
            Self::And { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Always => {}
                        Self::Never => return Self::Never,
                        Self::And { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Always,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::And { children: flat },
                }
            }
            Self::Or { children } => {
                let mut flat: Vec<Self> = Vec::with_capacity(children.len());
                for c in children {
                    match c.canonicalize() {
                        Self::Never => {}
                        Self::Always => return Self::Always,
                        Self::Or { children: sub } => flat.extend(sub),
                        other => flat.push(other),
                    }
                }
                flat.sort_by_cached_key(Self::canonical_bytes);
                dedup_partial_eq(&mut flat);
                match flat.len() {
                    0 => Self::Never,
                    1 => flat.pop().expect("len == 1"),
                    _ => Self::Or { children: flat },
                }
            }
            leaf => leaf,
        }
    }

    /// Canonical byte form of this constraint *as written*.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    /// Canonical fingerprint — auto-canonicalises first.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let canonical = self.clone().canonicalize();
        Fingerprint::of(&canonical.canonical_bytes())
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        const T_ALWAYS:     u8 = 0x50;
        const T_NEVER:      u8 = 0x51;
        const T_REQUIREALL: u8 = 0x52;
        const T_REQUIREANY: u8 = 0x53;
        const T_FORBIDALL:  u8 = 0x54;
        const T_MINSIZE:    u8 = 0x55;
        const T_MAXSIZE:    u8 = 0x56;
        const T_AND:        u8 = 0x57;
        const T_OR:         u8 = 0x58;
        const T_NOT:        u8 = 0x59;

        match self {
            Self::Always => out.push(T_ALWAYS),
            Self::Never => out.push(T_NEVER),
            Self::RequireAll { items } => {
                out.push(T_REQUIREALL);
                write_selection_item_set(out, items);
            }
            Self::RequireAny { items } => {
                out.push(T_REQUIREANY);
                write_selection_item_set(out, items);
            }
            Self::ForbidAll { items } => {
                out.push(T_FORBIDALL);
                write_selection_item_set(out, items);
            }
            Self::MinSize { n } => {
                out.push(T_MINSIZE);
                write_u32_le(out, *n);
            }
            Self::MaxSize { n } => {
                out.push(T_MAXSIZE);
                write_u32_le(out, *n);
            }
            Self::And { children } => {
                out.push(T_AND);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Or { children } => {
                out.push(T_OR);
                write_u32_le(out, u32::try_from(children.len()).expect("fits in u32"));
                for c in children {
                    c.write_canonical(out);
                }
            }
            Self::Not { child } => {
                out.push(T_NOT);
                child.write_canonical(out);
            }
        }
    }
}

impl std::ops::Not for SelectionConstraint {
    type Output = Self;
    fn not(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

fn absorb_and_selection(into: &mut Vec<SelectionConstraint>, c: SelectionConstraint) {
    match c {
        SelectionConstraint::And { children } => into.extend(children),
        other => into.push(other),
    }
}

fn absorb_or_selection(into: &mut Vec<SelectionConstraint>, c: SelectionConstraint) {
    match c {
        SelectionConstraint::Or { children } => into.extend(children),
        other => into.push(other),
    }
}

// ---------------------------------------------------------------------------
// Outer Constraint enum.
// ---------------------------------------------------------------------------

/// Kind-tagged constraint for dispatch against a [`Value`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Constraint {
    /// Constraint over an `i64`.
    Integer(IntConstraint),
    /// Constraint over an `f64`.
    Double(DoubleConstraint),
    /// Constraint over a `bool`.
    Boolean(BoolConstraint),
    /// Constraint over a `String`.
    String(StringConstraint),
    /// Constraint over a selection.
    Selection(SelectionConstraint),
}

impl Constraint {
    /// Test a candidate value against this constraint. Kind-mismatched
    /// values always fail.
    #[must_use]
    pub fn test(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Integer(c), Value::Integer(v)) => c.test(v.value),
            (Self::Double(c), Value::Double(v)) => c.test(v.value),
            (Self::Boolean(c), Value::Boolean(v)) => c.test(v.value),
            (Self::String(c), Value::String(v)) => c.test(&v.value),
            (Self::Selection(c), Value::Selection(v)) => c.test(&v.items),
            _ => false,
        }
    }

    /// Canonicalise the contained per-kind constraint.
    #[must_use]
    pub fn canonicalize(self) -> Self {
        match self {
            Self::Integer(c) => Self::Integer(c.canonicalize()),
            Self::Double(c) => Self::Double(c.canonicalize()),
            Self::Boolean(c) => Self::Boolean(c.canonicalize()),
            Self::String(c) => Self::String(c.canonicalize()),
            Self::Selection(c) => Self::Selection(c.canonicalize()),
        }
    }

    /// Canonical byte form. Delegates to the per-kind encoding — the
    /// first byte already identifies the kind because each kind uses a
    /// disjoint variant-tag range.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::Integer(c) => c.canonical_bytes(),
            Self::Double(c) => c.canonical_bytes(),
            Self::Boolean(c) => c.canonical_bytes(),
            Self::String(c) => c.canonical_bytes(),
            Self::Selection(c) => c.canonical_bytes(),
        }
    }

    /// Canonical fingerprint. Delegates to the per-kind fingerprint,
    /// which auto-canonicalises.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        match self {
            Self::Integer(c) => c.fingerprint(),
            Self::Double(c) => c.fingerprint(),
            Self::Boolean(c) => c.fingerprint(),
            Self::String(c) => c.fingerprint(),
            Self::Selection(c) => c.fingerprint(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::ops::Not;

    use super::*;
    use crate::names::ParameterName;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    // ---------- IntConstraint ----------

    #[test]
    fn int_min_and_max_test() {
        assert!(IntConstraint::Min { n: 5 }.test(5));
        assert!(IntConstraint::Min { n: 5 }.test(100));
        assert!(!IntConstraint::Min { n: 5 }.test(4));
        assert!(IntConstraint::Max { n: 5 }.test(5));
        assert!(IntConstraint::Max { n: 5 }.test(-1));
        assert!(!IntConstraint::Max { n: 5 }.test(6));
    }

    #[test]
    fn int_range_and_multiple_test() {
        let c = IntConstraint::Range { min: 1, max: 10 };
        assert!(c.test(1));
        assert!(c.test(10));
        assert!(!c.test(11));
        let m = IntConstraint::multiple(3).unwrap();
        assert!(m.test(9));
        assert!(!m.test(10));
        // Multiple(0) short-circuits to false, never panics.
        assert!(!IntConstraint::Multiple { n: 0 }.test(0));
    }

    #[test]
    fn int_inset_notinset_test() {
        let s: BTreeSet<i64> = [1, 2, 3].into_iter().collect();
        let in_set = IntConstraint::InSet { values: s.clone() };
        let not_in = IntConstraint::NotInSet { values: s };
        assert!(in_set.test(2));
        assert!(!in_set.test(4));
        assert!(not_in.test(4));
        assert!(!not_in.test(2));
    }

    #[test]
    fn int_and_or_not_flatten_and_test() {
        let c = IntConstraint::Min { n: 0 }
            .and(IntConstraint::Max { n: 10 })
            .and(IntConstraint::multiple(2).unwrap());
        // `and` flattens.
        match &c {
            IntConstraint::And { children } => assert_eq!(children.len(), 3),
            other => panic!("expected And, got {other:?}"),
        }
        assert!(c.test(4));
        assert!(!c.test(5));
        assert!(!c.test(-1));

        let n = IntConstraint::Min { n: 0 }.not();
        assert!(n.test(-1));
        assert!(!n.test(0));
    }

    #[test]
    fn int_canonicalize_collapses_identities() {
        // And(Always, x) -> x
        let c = IntConstraint::Always
            .and(IntConstraint::Min { n: 0 })
            .canonicalize();
        assert_eq!(c, IntConstraint::Min { n: 0 });

        // And(Never, x) -> Never
        let c = IntConstraint::Never
            .and(IntConstraint::Min { n: 0 })
            .canonicalize();
        assert_eq!(c, IntConstraint::Never);

        // Or(Always, x) -> Always
        let c = IntConstraint::Always
            .or(IntConstraint::Min { n: 0 })
            .canonicalize();
        assert_eq!(c, IntConstraint::Always);

        // Or(Never, x) -> x
        let c = IntConstraint::Never
            .or(IntConstraint::Min { n: 0 })
            .canonicalize();
        assert_eq!(c, IntConstraint::Min { n: 0 });
    }

    #[test]
    fn int_canonicalize_flattens_nested() {
        let c = IntConstraint::And {
            children: vec![
                IntConstraint::Min { n: 0 },
                IntConstraint::And {
                    children: vec![
                        IntConstraint::Max { n: 10 },
                        IntConstraint::multiple(2).unwrap(),
                    ],
                },
            ],
        }
        .canonicalize();
        match c {
            IntConstraint::And { children } => assert_eq!(children.len(), 3),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn int_canonicalize_peels_double_negation() {
        let c = IntConstraint::Min { n: 0 }.not().not().canonicalize();
        assert_eq!(c, IntConstraint::Min { n: 0 });
    }

    #[test]
    fn int_canonicalize_dedups_adjacent() {
        let c = IntConstraint::And {
            children: vec![
                IntConstraint::Min { n: 0 },
                IntConstraint::Min { n: 0 },
                IntConstraint::Max { n: 10 },
            ],
        }
        .canonicalize();
        match c {
            IntConstraint::And { children } => assert_eq!(children.len(), 2),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn int_canonicalize_single_child_collapses() {
        let c = IntConstraint::And {
            children: vec![IntConstraint::Min { n: 0 }],
        }
        .canonicalize();
        assert_eq!(c, IntConstraint::Min { n: 0 });

        let c = IntConstraint::Or {
            children: vec![],
        }
        .canonicalize();
        assert_eq!(c, IntConstraint::Never);

        let c = IntConstraint::And {
            children: vec![],
        }
        .canonicalize();
        assert_eq!(c, IntConstraint::Always);
    }

    #[test]
    fn int_canonicalize_not_folds_identities() {
        assert_eq!(
            IntConstraint::Always.not().canonicalize(),
            IntConstraint::Never
        );
        assert_eq!(
            IntConstraint::Never.not().canonicalize(),
            IntConstraint::Always
        );
    }

    // ---------- DoubleConstraint ----------

    #[test]
    fn double_range_test_and_nan_safe() {
        let c = DoubleConstraint::Range {
            min: 0.0,
            max: 1.0,
        };
        assert!(c.test(0.5));
        assert!(!c.test(1.5));
        assert!(!c.test(f64::NAN)); // NaN never satisfies.
    }

    #[test]
    fn double_canonicalize_double_negation() {
        let c = DoubleConstraint::Min { n: 0.0 }
            .not()
            .not()
            .canonicalize();
        assert_eq!(c, DoubleConstraint::Min { n: 0.0 });
    }

    // ---------- BoolConstraint ----------

    #[test]
    fn bool_eq_to_test() {
        assert!(BoolConstraint::EqTo { b: true }.test(true));
        assert!(!BoolConstraint::EqTo { b: true }.test(false));
    }

    #[test]
    fn bool_not_flips_eq_to_under_canonicalize() {
        let c = BoolConstraint::EqTo { b: true }.not().canonicalize();
        assert_eq!(c, BoolConstraint::EqTo { b: false });
    }

    // ---------- StringConstraint ----------

    #[test]
    fn string_regex_test() {
        let c = StringConstraint::Regex {
            pattern: RegexPattern::new("^[a-z]+$").unwrap(),
        };
        assert!(c.test("abc"));
        assert!(!c.test("abc1"));
    }

    #[test]
    fn string_length_range_and_nonempty_test() {
        let lr = StringConstraint::LengthRange { min: 1, max: 5 };
        assert!(lr.test("a"));
        assert!(lr.test("abcde"));
        assert!(!lr.test(""));
        assert!(!lr.test("abcdef"));
        assert!(StringConstraint::NonEmpty.test("x"));
        assert!(!StringConstraint::NonEmpty.test(""));
    }

    #[test]
    fn string_inset_test() {
        let set: BTreeSet<String> = ["red".into(), "blue".into()].into_iter().collect();
        let c = StringConstraint::InSet { values: set };
        assert!(c.test("red"));
        assert!(!c.test("green"));
    }

    #[test]
    fn string_and_or_not_combine() {
        let c = StringConstraint::NonEmpty
            .and(StringConstraint::LengthRange { min: 1, max: 3 });
        assert!(c.test("ab"));
        assert!(!c.test(""));
        assert!(!c.test("abcd"));
    }

    // ---------- SelectionConstraint ----------

    fn sel(xs: &[&str]) -> IndexSet<SelectionItem> {
        xs.iter().map(|s| SelectionItem::new(*s).unwrap()).collect()
    }

    fn sitems(xs: &[&str]) -> BTreeSet<SelectionItem> {
        xs.iter().map(|s| SelectionItem::new(*s).unwrap()).collect()
    }

    #[test]
    fn selection_require_all_and_any() {
        let c = SelectionConstraint::RequireAll {
            items: sitems(&["a", "b"]),
        };
        assert!(c.test(&sel(&["a", "b", "c"])));
        assert!(!c.test(&sel(&["a", "c"])));

        let c = SelectionConstraint::RequireAny {
            items: sitems(&["a", "b"]),
        };
        assert!(c.test(&sel(&["b"])));
        assert!(!c.test(&sel(&["x"])));
    }

    #[test]
    fn selection_forbid_all_and_sizes() {
        let c = SelectionConstraint::ForbidAll {
            items: sitems(&["z"]),
        };
        assert!(c.test(&sel(&["a"])));
        assert!(!c.test(&sel(&["z"])));

        let c = SelectionConstraint::MinSize { n: 2 };
        assert!(c.test(&sel(&["a", "b"])));
        assert!(!c.test(&sel(&["a"])));

        let c = SelectionConstraint::MaxSize { n: 2 };
        assert!(c.test(&sel(&["a", "b"])));
        assert!(!c.test(&sel(&["a", "b", "c"])));
    }

    // ---------- outer Constraint ----------

    #[test]
    fn outer_dispatches_by_kind() {
        let c = Constraint::Integer(IntConstraint::Min { n: 0 });
        let good = Value::integer(pname("n"), 5, None);
        let bad = Value::integer(pname("n"), -1, None);
        let wrong_kind = Value::boolean(pname("n"), true, None);
        assert!(c.test(&good));
        assert!(!c.test(&bad));
        assert!(!c.test(&wrong_kind));
    }

    #[test]
    fn outer_canonicalize_delegates() {
        let c = Constraint::Integer(IntConstraint::Min { n: 0 }.not().not());
        let canonical = c.canonicalize();
        assert_eq!(
            canonical,
            Constraint::Integer(IntConstraint::Min { n: 0 })
        );
    }

    // ---------- serde roundtrips ----------

    #[test]
    fn int_constraint_serde_roundtrip() {
        let c = IntConstraint::Min { n: 0 }
            .and(IntConstraint::Max { n: 10 })
            .not();
        let json = serde_json::to_string(&c).unwrap();
        let back: IntConstraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn string_constraint_with_regex_serde_roundtrip() {
        let c = StringConstraint::Regex {
            pattern: RegexPattern::new("^foo$").unwrap(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: StringConstraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn outer_constraint_serde_roundtrip() {
        let c = Constraint::Boolean(BoolConstraint::EqTo { b: true });
        let json = serde_json::to_string(&c).unwrap();
        let back: Constraint = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }

    // ---------- canonical_bytes / fingerprint ----------

    #[test]
    fn int_canonical_bytes_deterministic() {
        let a = IntConstraint::Range { min: 1, max: 10 }.canonical_bytes();
        let b = IntConstraint::Range { min: 1, max: 10 }.canonical_bytes();
        assert_eq!(a, b);
    }

    #[test]
    fn int_canonical_bytes_distinguish_variants() {
        let min = IntConstraint::Min { n: 5 }.canonical_bytes();
        let max = IntConstraint::Max { n: 5 }.canonical_bytes();
        assert_ne!(min, max);
    }

    #[test]
    fn int_fingerprint_stable_under_child_reordering() {
        let ab = IntConstraint::Min { n: 0 }
            .and(IntConstraint::Max { n: 10 })
            .fingerprint();
        let ba = IntConstraint::Max { n: 10 }
            .and(IntConstraint::Min { n: 0 })
            .fingerprint();
        assert_eq!(ab, ba, "canonicalise should reorder And children");
    }

    #[test]
    fn int_fingerprint_stable_under_nested_and_flattening() {
        let flat = IntConstraint::Min { n: 0 }
            .and(IntConstraint::Max { n: 10 })
            .and(IntConstraint::multiple(2).unwrap())
            .fingerprint();
        let nested = IntConstraint::Min { n: 0 }
            .and(IntConstraint::Max { n: 10 }.and(IntConstraint::multiple(2).unwrap()))
            .fingerprint();
        assert_eq!(flat, nested);
    }

    #[test]
    fn int_fingerprint_stable_under_double_negation() {
        let a = IntConstraint::Min { n: 0 }.fingerprint();
        let b = IntConstraint::Min { n: 0 }.not().not().fingerprint();
        assert_eq!(a, b);
    }

    #[test]
    fn int_fingerprint_distinguishes_different_constraints() {
        let a = IntConstraint::Min { n: 0 }.fingerprint();
        let b = IntConstraint::Min { n: 1 }.fingerprint();
        assert_ne!(a, b);
    }

    #[test]
    fn double_fingerprint_nan_normalises() {
        let a = DoubleConstraint::Min { n: f64::NAN }.fingerprint();
        let b = DoubleConstraint::Min {
            n: f64::from_bits(f64::NAN.to_bits() ^ 1),
        }
        .fingerprint();
        assert_eq!(a, b);
    }

    #[test]
    fn canonicalize_sorts_and_dedups_and_children() {
        let c = IntConstraint::And {
            children: vec![
                IntConstraint::Max { n: 10 },
                IntConstraint::Min { n: 0 },
                IntConstraint::Max { n: 10 }, // duplicate, should dedup
            ],
        }
        .canonicalize();
        let children = match c {
            IntConstraint::And { children } => children,
            other => panic!("expected And, got {other:?}"),
        };
        assert_eq!(children.len(), 2);
        // After canonical sort, the canonically-smaller child comes
        // first. Min has tag 0x12, Max has tag 0x13, so Min sorts first.
        match &children[0] {
            IntConstraint::Min { n: 0 } => {}
            other => panic!("expected Min {{ n: 0 }} first, got {other:?}"),
        }
    }

    #[test]
    fn bool_fingerprint_double_negation_matches() {
        let a = BoolConstraint::EqTo { b: true }.fingerprint();
        let b = BoolConstraint::EqTo { b: true }.not().not().fingerprint();
        assert_eq!(a, b);
    }

    #[test]
    fn string_fingerprint_regex_hashes_source() {
        let a = StringConstraint::Regex {
            pattern: RegexPattern::new("^abc$").unwrap(),
        }
        .fingerprint();
        let b = StringConstraint::Regex {
            pattern: RegexPattern::new("^abc$").unwrap(),
        }
        .fingerprint();
        assert_eq!(a, b);
        let c = StringConstraint::Regex {
            pattern: RegexPattern::new("^abd$").unwrap(),
        }
        .fingerprint();
        assert_ne!(a, c);
    }

    #[test]
    fn selection_fingerprint_stable_under_and_reordering() {
        let req = SelectionConstraint::RequireAll {
            items: sitems(&["a", "b"]),
        };
        let max = SelectionConstraint::MaxSize { n: 3 };
        let ab = req.clone().and(max.clone()).fingerprint();
        let ba = max.and(req).fingerprint();
        assert_eq!(ab, ba);
    }

    #[test]
    fn outer_constraint_fingerprint_delegates() {
        let int_c = IntConstraint::Min { n: 0 };
        let direct = int_c.fingerprint();
        let outer = Constraint::Integer(int_c).fingerprint();
        assert_eq!(direct, outer);
    }

    #[test]
    fn outer_constraint_kinds_do_not_collide() {
        // Integer Always and Double Always use distinct first bytes,
        // so their canonical forms (and fingerprints) differ.
        let i = Constraint::Integer(IntConstraint::Always).fingerprint();
        let d = Constraint::Double(DoubleConstraint::Always).fingerprint();
        let b = Constraint::Boolean(BoolConstraint::Always).fingerprint();
        let s = Constraint::String(StringConstraint::Always).fingerprint();
        let sel = Constraint::Selection(SelectionConstraint::Always).fingerprint();
        let all = [i, d, b, s, sel];
        for (a_i, a) in all.iter().enumerate() {
            for (b_i, b) in all.iter().enumerate() {
                if a_i != b_i {
                    assert_ne!(a, b, "kinds {a_i} and {b_i} collided");
                }
            }
        }
    }
}
