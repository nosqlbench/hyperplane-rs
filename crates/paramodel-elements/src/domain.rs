// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Parameter domains.
//!
//! A domain is the set of values a parameter may legally take. Per
//! SRD-0004 D2 each parameter kind owns a sub-enum of its admissible
//! shapes (`IntegerDomain`, `DoubleDomain`, `StringDomain`,
//! `SelectionDomain`; `Boolean` has no payload), and a view enum
//! [`Domain`] borrows into whichever sub-enum a [`Parameter`] carries.
//! This module defines the owned sub-enums, their native-type
//! operations (membership, cardinality, boundaries, sampling,
//! enumeration), the [`Domain`] view wrapper that lifts those ops into
//! [`Value`] form, and the [`SelectionResolver`] trait the Selection
//! `External` variant defers to at plan-compilation time.
//!
//! Gaps flagged for later SRD revisions:
//!
//! - `StringDomain::Regex` sampling. The `regex` crate does not provide
//!   regex-backed generation; a future slice can plug in a generator
//!   crate or defer sampling to the caller. Until then, [`Domain::sample`]
//!   panics for the `Regex` variant.
//! - `SelectionDomain::External` sampling. Registry-aware sampling is
//!   specified in SRD-0004 D15 but not yet wired up; [`Domain::sample`]
//!   panics for the `External` variant.
//!
//! [`Parameter`]: crate::value::Value

use std::collections::BTreeSet;
use std::sync::Arc;

use indexmap::IndexSet;
use rand::Rng;
use rand::seq::IteratorRandom;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::names::{NameError, ParameterName};
use crate::value::{SelectionItem, Value};

// ---------------------------------------------------------------------------
// Cardinality.
// ---------------------------------------------------------------------------

/// How many distinct values a domain contains.
///
/// Cardinality is reported at domain granularity, not value-kind
/// granularity. It is `Unbounded` for continuous (`DoubleDomain::Range`)
/// and open-ended (`StringDomain::Any` / `Regex`,
/// `SelectionDomain::External`) domains; otherwise it is `Finite(n)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Cardinality {
    /// Finite count of distinct values, saturating at `u64::MAX` if the
    /// true count would exceed it.
    Finite {
        /// The count.
        count: u64,
    },
    /// Too many values to count (continuous or open-ended).
    Unbounded,
}

impl Cardinality {
    /// Construct a finite cardinality.
    #[must_use]
    pub const fn finite(count: u64) -> Self {
        Self::Finite { count }
    }
}

// ---------------------------------------------------------------------------
// DomainError.
// ---------------------------------------------------------------------------

/// Errors produced by domain constructors and operations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainError {
    /// The domain is not enumerable (continuous or open-ended). Returned
    /// by [`Domain::enumerate`].
    #[error("domain is not enumerable")]
    NotEnumerable,

    /// A numeric range was rejected because `min > max`, a bound was
    /// `NaN`, or a bound was non-finite.
    #[error("invalid range: min={min}, max={max}")]
    InvalidRange {
        /// The rejected minimum bound as a display string.
        min: String,
        /// The rejected maximum bound as a display string.
        max: String,
    },

    /// A discrete integer domain was constructed with zero values.
    #[error("discrete integer domain must contain at least one value")]
    EmptyDiscrete,

    /// A fixed selection domain was constructed with zero values.
    #[error("fixed selection domain must contain at least one value")]
    EmptySelection,

    /// A selection domain was constructed with `max_selections = 0`.
    #[error("selection domain max_selections must be at least 1")]
    ZeroMaxSelections,
}

// ---------------------------------------------------------------------------
// RegexPattern.
// ---------------------------------------------------------------------------

/// A validated regex, carrying both the source pattern and the compiled
/// automaton.
///
/// Equality and hashing use the source string (the compiled automaton
/// has no stable representation). Serde emits the source string and
/// recompiles on deserialisation — the only place in paramodel where
/// deserialisation can fail due to payload validity.
#[derive(Debug, Clone)]
pub struct RegexPattern {
    source:   String,
    compiled: Regex,
}

impl RegexPattern {
    /// Compile a source pattern.
    pub fn new(source: impl Into<String>) -> Result<Self, regex::Error> {
        let source = source.into();
        let compiled = Regex::new(&source)?;
        Ok(Self { source, compiled })
    }

    /// Borrow the source pattern.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Test whether the given text matches.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        self.compiled.is_match(text)
    }
}

impl PartialEq for RegexPattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for RegexPattern {}

impl std::hash::Hash for RegexPattern {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.source.hash(state);
    }
}

impl Serialize for RegexPattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.source)
    }
}

impl<'de> Deserialize<'de> for RegexPattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let source = String::deserialize(d)?;
        Self::new(source).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// ResolverId.
// ---------------------------------------------------------------------------

/// Opaque identifier naming an external selection resolver.
///
/// Resolver ids are short ASCII identifiers — they follow the same
/// validation rules as a [`ParameterName`] so they show up cleanly in
/// logs and generated code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResolverId(String);

const RESOLVER_ID_MAX: usize = 64;

impl ResolverId {
    /// Construct a new id, validating the candidate.
    pub fn new(candidate: impl Into<String>) -> std::result::Result<Self, NameError> {
        let s = candidate.into();
        if s.is_empty() {
            return Err(NameError::Empty);
        }
        if s.len() > RESOLVER_ID_MAX {
            return Err(NameError::TooLong {
                length: s.len(),
                max:    RESOLVER_ID_MAX,
            });
        }
        let first = s.chars().next().expect("non-empty");
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(NameError::BadStart { ch: first });
        }
        for (offset, ch) in s.char_indices() {
            if !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
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
}

impl std::fmt::Display for ResolverId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for ResolverId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ResolverId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// IntegerDomain.
// ---------------------------------------------------------------------------

/// Integer domain: either an inclusive numeric range or a finite discrete
/// set.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum IntegerDomain {
    /// Inclusive `[min, max]`.
    Range {
        /// Lower bound (inclusive).
        min: i64,
        /// Upper bound (inclusive).
        max: i64,
    },
    /// Explicit value set; at least one element.
    Discrete {
        /// The permitted values.
        values: BTreeSet<i64>,
    },
}

impl IntegerDomain {
    /// Construct an inclusive range. Rejects `min > max`.
    pub fn range(min: i64, max: i64) -> Result<Self> {
        if min > max {
            return Err(Error::Domain(DomainError::InvalidRange {
                min: min.to_string(),
                max: max.to_string(),
            }));
        }
        Ok(Self::Range { min, max })
    }

    /// Construct a discrete domain. Rejects empty sets.
    pub fn discrete(values: BTreeSet<i64>) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::Domain(DomainError::EmptyDiscrete));
        }
        Ok(Self::Discrete { values })
    }

    /// Membership test on the native payload.
    #[must_use]
    pub fn contains_native(&self, value: i64) -> bool {
        match self {
            Self::Range { min, max } => value >= *min && value <= *max,
            Self::Discrete { values } => values.contains(&value),
        }
    }

    /// Count of distinct values. Saturates at `u64::MAX` if a range's
    /// width exceeds `u64`.
    #[must_use]
    pub fn cardinality(&self) -> Cardinality {
        match self {
            Self::Range { min, max } => {
                let min = i128::from(*min);
                let max = i128::from(*max);
                let width = max - min + 1;
                let count = u64::try_from(width).unwrap_or(u64::MAX);
                Cardinality::finite(count)
            }
            Self::Discrete { values } => Cardinality::finite(values.len() as u64),
        }
    }

    /// Boundary values (at most two).
    #[must_use]
    pub fn boundaries_native(&self) -> Vec<i64> {
        match self {
            Self::Range { min, max } => {
                if min == max {
                    vec![*min]
                } else {
                    vec![*min, *max]
                }
            }
            Self::Discrete { values } => {
                let mut out = Vec::with_capacity(2);
                if let Some(v) = values.iter().next() {
                    out.push(*v);
                }
                if let Some(v) = values.iter().next_back()
                    && out.last() != Some(v)
                {
                    out.push(*v);
                }
                out
            }
        }
    }

    /// Uniformly sample a value using the given RNG.
    pub fn sample_native<R: Rng + ?Sized>(&self, rng: &mut R) -> i64 {
        match self {
            Self::Range { min, max } => rng.gen_range(*min..=*max),
            Self::Discrete { values } => {
                let idx = rng.gen_range(0..values.len());
                *values.iter().nth(idx).expect("idx < len")
            }
        }
    }

    /// Iterator over every value in the domain.
    #[must_use]
    pub fn iter_native<'a>(&'a self) -> Box<dyn Iterator<Item = i64> + 'a> {
        match self {
            Self::Range { min, max } => Box::new(*min..=*max),
            Self::Discrete { values } => Box::new(values.iter().copied()),
        }
    }
}

// ---------------------------------------------------------------------------
// DoubleDomain.
// ---------------------------------------------------------------------------

/// Double-precision float domain: inclusive range with finite, non-`NaN`
/// bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum DoubleDomain {
    /// Inclusive `[min, max]`.
    Range {
        /// Lower bound (inclusive).
        min: f64,
        /// Upper bound (inclusive).
        max: f64,
    },
}

impl DoubleDomain {
    /// Construct an inclusive range. Rejects `NaN`, non-finite bounds,
    /// and reversed bounds.
    pub fn range(min: f64, max: f64) -> Result<Self> {
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(Error::Domain(DomainError::InvalidRange {
                min: format!("{min}"),
                max: format!("{max}"),
            }));
        }
        Ok(Self::Range { min, max })
    }

    /// Membership test. `NaN` is never contained.
    #[must_use]
    pub fn contains_native(&self, value: f64) -> bool {
        if value.is_nan() {
            return false;
        }
        match self {
            Self::Range { min, max } => value >= *min && value <= *max,
        }
    }

    /// Continuous ranges are `Unbounded`.
    #[must_use]
    pub const fn cardinality(&self) -> Cardinality {
        Cardinality::Unbounded
    }

    /// Range endpoints, deduplicated if `min == max`.
    #[must_use]
    #[allow(clippy::float_cmp, reason = "exact equality here detects a single-point range")]
    pub fn boundaries_native(&self) -> Vec<f64> {
        match self {
            Self::Range { min, max } => {
                if min == max {
                    vec![*min]
                } else {
                    vec![*min, *max]
                }
            }
        }
    }

    /// Uniformly sample from the range.
    pub fn sample_native<R: Rng + ?Sized>(&self, rng: &mut R) -> f64 {
        match self {
            Self::Range { min, max } => rng.gen_range(*min..=*max),
        }
    }
}

// ---------------------------------------------------------------------------
// StringDomain.
// ---------------------------------------------------------------------------

/// String domain: accept-all or regex-matched.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum StringDomain {
    /// Any UTF-8 string is a member.
    Any,
    /// Members are the strings matching this regex.
    Regex {
        /// The compiled pattern.
        pattern: RegexPattern,
    },
}

impl StringDomain {
    /// Construct an accept-all domain.
    #[must_use]
    pub const fn any() -> Self {
        Self::Any
    }

    /// Construct a regex domain from a source pattern.
    pub fn regex(source: impl Into<String>) -> Result<Self> {
        Ok(Self::Regex {
            pattern: RegexPattern::new(source)?,
        })
    }

    /// Membership test on the native payload.
    #[must_use]
    pub fn contains_native(&self, value: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Regex { pattern } => pattern.is_match(value),
        }
    }

    /// String domains are `Unbounded` regardless of shape.
    #[must_use]
    pub const fn cardinality(&self) -> Cardinality {
        Cardinality::Unbounded
    }

    /// Conservative boundary set. `Any` returns `[""]`; `Regex` also
    /// returns `[""]` as a placeholder until regex-sampling lands —
    /// see module docs.
    #[must_use]
    pub fn boundaries_native(&self) -> Vec<String> {
        vec![String::new()]
    }

    /// Conservative sample. Returns `""` for `Any`. For `Regex`,
    /// sampling is unimplemented; see module docs.
    ///
    /// # Panics
    ///
    /// Panics on `StringDomain::Regex`.
    pub fn sample_native<R: Rng + ?Sized>(&self, _rng: &mut R) -> String {
        match self {
            Self::Any => String::new(),
            Self::Regex { .. } => unimplemented!(
                "sampling StringDomain::Regex requires a regex generator crate \
                 (see SRD-0004 follow-ups); call the authored default instead."
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// SelectionDomain.
// ---------------------------------------------------------------------------

/// Selection domain: a set the user can pick multiple items from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum SelectionDomain {
    /// The available items are enumerated inline.
    Fixed {
        /// The available items, in authored order.
        values:         IndexSet<SelectionItem>,
        /// Maximum number of items that may be selected together.
        max_selections: u32,
    },
    /// The available items come from an external resolver looked up by
    /// id at plan-compilation time.
    External {
        /// Opaque resolver id for the registry.
        resolver:       ResolverId,
        /// Maximum number of items that may be selected together.
        max_selections: u32,
    },
}

impl SelectionDomain {
    /// Construct a fixed selection domain. Rejects empty value sets or
    /// zero `max_selections`.
    pub fn fixed(values: IndexSet<SelectionItem>, max_selections: u32) -> Result<Self> {
        if values.is_empty() {
            return Err(Error::Domain(DomainError::EmptySelection));
        }
        if max_selections == 0 {
            return Err(Error::Domain(DomainError::ZeroMaxSelections));
        }
        Ok(Self::Fixed {
            values,
            max_selections,
        })
    }

    /// Construct an external selection domain.
    pub fn external(resolver: ResolverId, max_selections: u32) -> Result<Self> {
        if max_selections == 0 {
            return Err(Error::Domain(DomainError::ZeroMaxSelections));
        }
        Ok(Self::External {
            resolver,
            max_selections,
        })
    }

    /// Maximum number of items selectable at once.
    #[must_use]
    pub const fn max_selections(&self) -> u32 {
        match self {
            Self::Fixed { max_selections, .. } | Self::External { max_selections, .. } => {
                *max_selections
            }
        }
    }

    /// Membership test: every item in `items` must be a legal
    /// member. For `External` variants, legality can only be confirmed
    /// through the resolver — [`Domain::contains`] handles that case by
    /// degrading to shape-only checks.
    #[must_use]
    pub fn contains_items_fixed(&self, items: &IndexSet<SelectionItem>) -> bool {
        match self {
            Self::Fixed {
                values,
                max_selections,
            } => items.len() <= *max_selections as usize && items.iter().all(|i| values.contains(i)),
            Self::External { .. } => false,
        }
    }

    /// Available-item count for the `Fixed` variant.
    #[must_use]
    pub fn cardinality(&self) -> Cardinality {
        match self {
            Self::Fixed { values, .. } => Cardinality::finite(values.len() as u64),
            Self::External { .. } => Cardinality::Unbounded,
        }
    }

    /// Single-item selections at the first and last boundary items.
    #[must_use]
    pub fn boundaries_fixed(&self) -> Vec<IndexSet<SelectionItem>> {
        match self {
            Self::Fixed { values, .. } => {
                let mut out: Vec<IndexSet<SelectionItem>> = Vec::new();
                if let Some(first) = values.iter().next() {
                    let mut one = IndexSet::new();
                    one.insert(first.clone());
                    out.push(one);
                }
                if let Some(last) = values.iter().next_back() {
                    let mut one = IndexSet::new();
                    one.insert(last.clone());
                    if out.first().is_none_or(|f| f.iter().next() != Some(last)) {
                        out.push(one);
                    }
                }
                out
            }
            Self::External { .. } => Vec::new(),
        }
    }

    /// Sample a random subset of size `1..=max_selections`.
    ///
    /// # Panics
    ///
    /// Panics on `SelectionDomain::External` — registry-aware sampling
    /// is a future slice; see module docs.
    pub fn sample_fixed<R: Rng + ?Sized>(&self, rng: &mut R) -> IndexSet<SelectionItem> {
        match self {
            Self::Fixed {
                values,
                max_selections,
            } => {
                let cap = (*max_selections as usize).min(values.len()).max(1);
                let k = rng.gen_range(1..=cap);
                let picks: Vec<SelectionItem> =
                    values.iter().cloned().choose_multiple(rng, k);
                picks.into_iter().collect()
            }
            Self::External { .. } => unimplemented!(
                "sampling SelectionDomain::External requires a \
                 SelectionResolverRegistry (see SRD-0004 D15)."
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// SelectionResolver traits.
// ---------------------------------------------------------------------------

/// Label attached to an external selection value for display purposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabeledEntry {
    /// Canonical value id.
    pub value: SelectionItem,
    /// Human-readable label.
    pub label: String,
}

/// Resolves the set of valid values for one external selection.
///
/// Embedding systems implement this trait to plug their domain-specific
/// catalogs (datasets, templates, …) into paramodel. The trait object
/// lives outside the parameter algebra (see SRD-0004 D15): parameters
/// hold only the opaque [`ResolverId`], and the registry is consulted at
/// plan-compilation / binding time.
pub trait SelectionResolver: Send + Sync + std::fmt::Debug + 'static {
    /// Resolver identity.
    fn id(&self) -> &ResolverId;

    /// Current snapshot of valid values. Cheap to call; implementations
    /// may cache internally.
    fn valid_values(&self) -> Result<IndexSet<SelectionItem>>;

    /// Membership test. The default delegates to [`Self::valid_values`].
    fn is_valid(&self, value: &SelectionItem) -> Result<bool> {
        Ok(self.valid_values()?.contains(value))
    }

    /// Human-readable description for UI / logs.
    fn describe(&self) -> &str;
}

/// Extension trait: values carry UI labels in addition to their
/// canonical ids.
pub trait LabeledSelectionResolver: SelectionResolver {
    /// Labelled version of [`SelectionResolver::valid_values`].
    fn labeled_values(&self) -> Result<Vec<LabeledEntry>>;
}

/// Host registry. The host (typically the embedding system) provides an
/// implementation that knows how to look up resolvers by id.
pub trait SelectionResolverRegistry: Send + Sync + std::fmt::Debug + 'static {
    /// Resolver lookup. Returns `None` if the id is unknown.
    fn get(&self, id: &ResolverId) -> Option<Arc<dyn SelectionResolver>>;

    /// Ids of every registered resolver.
    fn ids(&self) -> Vec<ResolverId>;
}

// ---------------------------------------------------------------------------
// Domain view.
// ---------------------------------------------------------------------------

/// Borrowed view over a parameter's domain, pinned to the parameter's
/// name so operations can produce properly-provenanced [`Value`]s.
///
/// This is a small deviation from SRD-0004 §Domain, which shows the
/// view enum without the parameter reference. Threading the name
/// through is the simplest way to let `sample`, `boundary_values`, and
/// `enumerate` hand back owned [`Value`]s without asking the caller to
/// pass the name at every call site.
#[derive(Debug, Clone, Copy)]
pub enum Domain<'a> {
    /// Integer domain view.
    Integer {
        /// Owning parameter's name.
        parameter: &'a ParameterName,
        /// Borrowed per-kind domain.
        domain:    &'a IntegerDomain,
    },
    /// Double domain view.
    Double {
        /// Owning parameter's name.
        parameter: &'a ParameterName,
        /// Borrowed per-kind domain.
        domain:    &'a DoubleDomain,
    },
    /// Boolean domain view — no payload.
    Boolean {
        /// Owning parameter's name.
        parameter: &'a ParameterName,
    },
    /// String domain view.
    String {
        /// Owning parameter's name.
        parameter: &'a ParameterName,
        /// Borrowed per-kind domain.
        domain:    &'a StringDomain,
    },
    /// Selection domain view.
    Selection {
        /// Owning parameter's name.
        parameter: &'a ParameterName,
        /// Borrowed per-kind domain.
        domain:    &'a SelectionDomain,
    },
}

impl<'a> Domain<'a> {
    /// The owning parameter's name.
    #[must_use]
    pub const fn parameter(&self) -> &'a ParameterName {
        match self {
            Self::Integer { parameter, .. }
            | Self::Double { parameter, .. }
            | Self::Boolean { parameter }
            | Self::String { parameter, .. }
            | Self::Selection { parameter, .. } => parameter,
        }
    }

    /// Membership test. Kind-mismatched values always return `false`.
    /// For `SelectionDomain::External`, only the shape is checked —
    /// full validation requires a resolver and is deferred.
    #[must_use]
    pub fn contains(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Integer { domain, .. }, Value::Integer(v)) => {
                domain.contains_native(v.value)
            }
            (Self::Double { domain, .. }, Value::Double(v)) => {
                domain.contains_native(v.value)
            }
            (Self::Boolean { .. }, Value::Boolean(_)) => true,
            (Self::String { domain, .. }, Value::String(v)) => {
                domain.contains_native(&v.value)
            }
            (Self::Selection { domain, .. }, Value::Selection(v)) => match domain {
                SelectionDomain::Fixed { .. } => domain.contains_items_fixed(&v.items),
                SelectionDomain::External { max_selections, .. } => {
                    v.items.len() <= *max_selections as usize
                }
            },
            _ => false,
        }
    }

    /// Count of distinct values.
    #[must_use]
    pub fn cardinality(&self) -> Cardinality {
        match self {
            Self::Integer { domain, .. } => domain.cardinality(),
            Self::Double { domain, .. } => domain.cardinality(),
            Self::Boolean { .. } => Cardinality::finite(2),
            Self::String { domain, .. } => domain.cardinality(),
            Self::Selection { domain, .. } => domain.cardinality(),
        }
    }

    /// Boundary values, lifted to `Value`.
    #[must_use]
    pub fn boundary_values(&self) -> Vec<Value> {
        match self {
            Self::Integer { parameter, domain } => domain
                .boundaries_native()
                .into_iter()
                .map(|v| Value::integer((*parameter).clone(), v, None))
                .collect(),
            Self::Double { parameter, domain } => domain
                .boundaries_native()
                .into_iter()
                .map(|v| Value::double((*parameter).clone(), v, None))
                .collect(),
            Self::Boolean { parameter } => vec![
                Value::boolean((*parameter).clone(), false, None),
                Value::boolean((*parameter).clone(), true, None),
            ],
            Self::String { parameter, domain } => domain
                .boundaries_native()
                .into_iter()
                .map(|v| Value::string((*parameter).clone(), v, None))
                .collect(),
            Self::Selection { parameter, domain } => domain
                .boundaries_fixed()
                .into_iter()
                .map(|items| Value::selection((*parameter).clone(), items, None))
                .collect(),
        }
    }

    /// Uniformly sample a value from the domain.
    ///
    /// # Panics
    ///
    /// Panics for `StringDomain::Regex` and `SelectionDomain::External`
    /// — see module docs.
    pub fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> Value {
        match self {
            Self::Integer { parameter, domain } => {
                Value::integer((*parameter).clone(), domain.sample_native(rng), None)
            }
            Self::Double { parameter, domain } => {
                Value::double((*parameter).clone(), domain.sample_native(rng), None)
            }
            Self::Boolean { parameter } => {
                Value::boolean((*parameter).clone(), rng.gen_bool(0.5), None)
            }
            Self::String { parameter, domain } => {
                Value::string((*parameter).clone(), domain.sample_native(rng), None)
            }
            Self::Selection { parameter, domain } => {
                Value::selection((*parameter).clone(), domain.sample_fixed(rng), None)
            }
        }
    }

    /// Enumerate every value in the domain as `Value`s. Fails for
    /// continuous and open-ended domains.
    pub fn enumerate(&self) -> Result<Box<dyn Iterator<Item = Value> + 'a>> {
        match self {
            Self::Integer { parameter, domain } => {
                let parameter = (*parameter).clone();
                match domain {
                    IntegerDomain::Range { min, max } => {
                        let (min, max) = (*min, *max);
                        Ok(Box::new((min..=max).map(move |v| {
                            Value::integer(parameter.clone(), v, None)
                        })))
                    }
                    IntegerDomain::Discrete { values } => {
                        Ok(Box::new(values.iter().copied().map(move |v| {
                            Value::integer(parameter.clone(), v, None)
                        })))
                    }
                }
            }
            Self::Double { .. } | Self::String { .. } => {
                Err(Error::Domain(DomainError::NotEnumerable))
            }
            Self::Boolean { parameter } => {
                let parameter = (*parameter).clone();
                Ok(Box::new([false, true].into_iter().map(move |v| {
                    Value::boolean(parameter.clone(), v, None)
                })))
            }
            Self::Selection { parameter, domain } => match domain {
                SelectionDomain::Fixed { values, .. } => {
                    let parameter = (*parameter).clone();
                    Ok(Box::new(values.iter().cloned().map(move |item| {
                        let mut one = IndexSet::new();
                        one.insert(item);
                        Value::selection(parameter.clone(), one, None)
                    })))
                }
                SelectionDomain::External { .. } => {
                    Err(Error::Domain(DomainError::NotEnumerable))
                }
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn selitems(xs: &[&str]) -> IndexSet<SelectionItem> {
        xs.iter().map(|s| SelectionItem::new(*s).unwrap()).collect()
    }

    // ---------- Cardinality / DomainError ----------

    #[test]
    fn cardinality_equality() {
        assert_eq!(Cardinality::finite(3), Cardinality::finite(3));
        assert_ne!(Cardinality::finite(3), Cardinality::Unbounded);
    }

    // ---------- RegexPattern ----------

    #[test]
    fn regex_pattern_compiles_and_matches() {
        let p = RegexPattern::new("^[a-z]+$").unwrap();
        assert!(p.is_match("abc"));
        assert!(!p.is_match("abc1"));
        assert_eq!(p.as_str(), "^[a-z]+$");
    }

    #[test]
    fn regex_pattern_serde_roundtrip() {
        let p = RegexPattern::new("^foo$").unwrap();
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, "\"^foo$\"");
        let back: RegexPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn regex_pattern_invalid_source_is_deserialise_error() {
        let bad: std::result::Result<RegexPattern, _> = serde_json::from_str("\"[\"");
        assert!(bad.is_err());
    }

    // ---------- ResolverId ----------

    #[test]
    fn resolver_id_accepts_simple_ids() {
        ResolverId::new("datasets").unwrap();
        ResolverId::new("study-templates").unwrap();
    }

    #[test]
    fn resolver_id_rejects_bad_start() {
        assert!(matches!(
            ResolverId::new("1ds"),
            Err(NameError::BadStart { .. })
        ));
    }

    // ---------- IntegerDomain ----------

    #[test]
    fn integer_range_constructor_and_ops() {
        let d = IntegerDomain::range(1, 5).unwrap();
        assert!(d.contains_native(1));
        assert!(d.contains_native(5));
        assert!(!d.contains_native(0));
        assert!(!d.contains_native(6));
        assert_eq!(d.cardinality(), Cardinality::finite(5));
        assert_eq!(d.boundaries_native(), vec![1, 5]);
    }

    #[test]
    fn integer_range_single_point_has_one_boundary() {
        let d = IntegerDomain::range(7, 7).unwrap();
        assert_eq!(d.boundaries_native(), vec![7]);
        assert_eq!(d.cardinality(), Cardinality::finite(1));
    }

    #[test]
    fn integer_range_rejects_reversed_bounds() {
        let err = IntegerDomain::range(5, 1).unwrap_err();
        assert!(matches!(err, Error::Domain(DomainError::InvalidRange { .. })));
    }

    #[test]
    fn integer_discrete_constructor_and_ops() {
        let mut set = BTreeSet::new();
        set.insert(3);
        set.insert(1);
        set.insert(5);
        let d = IntegerDomain::discrete(set).unwrap();
        assert!(d.contains_native(1));
        assert!(!d.contains_native(2));
        assert_eq!(d.cardinality(), Cardinality::finite(3));
        assert_eq!(d.boundaries_native(), vec![1, 5]);
    }

    #[test]
    fn integer_discrete_rejects_empty() {
        let err = IntegerDomain::discrete(BTreeSet::new()).unwrap_err();
        assert!(matches!(err, Error::Domain(DomainError::EmptyDiscrete)));
    }

    #[test]
    fn integer_range_cardinality_saturates() {
        let d = IntegerDomain::range(i64::MIN, i64::MAX).unwrap();
        assert_eq!(d.cardinality(), Cardinality::finite(u64::MAX));
    }

    #[test]
    fn integer_sample_in_range() {
        let mut rng = StdRng::seed_from_u64(42);
        let d = IntegerDomain::range(10, 20).unwrap();
        for _ in 0..50 {
            let v = d.sample_native(&mut rng);
            assert!((10..=20).contains(&v));
        }
    }

    #[test]
    fn integer_iter_covers_range() {
        let d = IntegerDomain::range(1, 3).unwrap();
        let got: Vec<i64> = d.iter_native().collect();
        assert_eq!(got, vec![1, 2, 3]);
    }

    // ---------- DoubleDomain ----------

    #[test]
    fn double_range_rejects_nan_and_reversed() {
        assert!(DoubleDomain::range(f64::NAN, 1.0).is_err());
        assert!(DoubleDomain::range(0.0, f64::NAN).is_err());
        assert!(DoubleDomain::range(f64::INFINITY, 1.0).is_err());
        assert!(DoubleDomain::range(2.0, 1.0).is_err());
    }

    #[test]
    fn double_range_contains() {
        let d = DoubleDomain::range(0.0, 1.0).unwrap();
        assert!(d.contains_native(0.0));
        assert!(d.contains_native(1.0));
        assert!(d.contains_native(0.5));
        assert!(!d.contains_native(-0.1));
        assert!(!d.contains_native(f64::NAN));
    }

    #[test]
    fn double_cardinality_is_unbounded() {
        let d = DoubleDomain::range(0.0, 1.0).unwrap();
        assert_eq!(d.cardinality(), Cardinality::Unbounded);
    }

    // ---------- StringDomain ----------

    #[test]
    fn string_any_contains_anything() {
        let d = StringDomain::any();
        assert!(d.contains_native(""));
        assert!(d.contains_native("hello"));
        assert_eq!(d.cardinality(), Cardinality::Unbounded);
    }

    #[test]
    fn string_regex_contains_matches_only() {
        let d = StringDomain::regex("^[a-z]+$").unwrap();
        assert!(d.contains_native("abc"));
        assert!(!d.contains_native("abc1"));
    }

    #[test]
    fn string_regex_rejects_malformed_source() {
        let err = StringDomain::regex("[").unwrap_err();
        assert!(matches!(err, Error::Regex(_)));
    }

    // ---------- SelectionDomain ----------

    #[test]
    fn selection_fixed_constructor_and_ops() {
        let d = SelectionDomain::fixed(selitems(&["a", "b", "c"]), 2).unwrap();
        assert_eq!(d.max_selections(), 2);
        assert_eq!(d.cardinality(), Cardinality::finite(3));
        assert!(d.contains_items_fixed(&selitems(&["a"])));
        assert!(d.contains_items_fixed(&selitems(&["a", "b"])));
        assert!(!d.contains_items_fixed(&selitems(&["a", "b", "c"])));
        assert!(!d.contains_items_fixed(&selitems(&["x"])));
    }

    #[test]
    fn selection_fixed_rejects_empty_and_zero_max() {
        assert!(matches!(
            SelectionDomain::fixed(IndexSet::new(), 1),
            Err(Error::Domain(DomainError::EmptySelection))
        ));
        assert!(matches!(
            SelectionDomain::fixed(selitems(&["a"]), 0),
            Err(Error::Domain(DomainError::ZeroMaxSelections))
        ));
    }

    #[test]
    fn selection_external_constructor() {
        let id = ResolverId::new("datasets").unwrap();
        let d = SelectionDomain::external(id, 1).unwrap();
        assert_eq!(d.cardinality(), Cardinality::Unbounded);
        assert!(d.boundaries_fixed().is_empty());
    }

    #[test]
    fn selection_sample_respects_max() {
        let mut rng = StdRng::seed_from_u64(7);
        let d = SelectionDomain::fixed(selitems(&["a", "b", "c", "d"]), 2).unwrap();
        for _ in 0..50 {
            let pick = d.sample_fixed(&mut rng);
            assert!(!pick.is_empty());
            assert!(pick.len() <= 2);
            for item in &pick {
                assert!(["a", "b", "c", "d"].contains(&item.as_str()));
            }
        }
    }

    // ---------- Domain view ----------

    #[test]
    fn domain_view_contains_dispatches_by_kind() {
        let name = pname("threads");
        let id = IntegerDomain::range(1, 10).unwrap();
        let view = Domain::Integer {
            parameter: &name,
            domain:    &id,
        };
        let in_range = Value::integer(name.clone(), 5, None);
        let out_of_range = Value::integer(name.clone(), 42, None);
        let wrong_kind = Value::boolean(name.clone(), true, None);
        assert!(view.contains(&in_range));
        assert!(!view.contains(&out_of_range));
        assert!(!view.contains(&wrong_kind));
    }

    #[test]
    fn domain_view_boundaries_return_values() {
        let name = pname("threads");
        let id = IntegerDomain::range(1, 10).unwrap();
        let view = Domain::Integer {
            parameter: &name,
            domain:    &id,
        };
        let bs = view.boundary_values();
        assert_eq!(bs.len(), 2);
        assert_eq!(bs[0].as_integer(), Some(1));
        assert_eq!(bs[1].as_integer(), Some(10));
        assert_eq!(bs[0].parameter().as_str(), "threads");
    }

    #[test]
    fn domain_view_enumerate_integer_range() {
        let name = pname("n");
        let id = IntegerDomain::range(1, 3).unwrap();
        let view = Domain::Integer {
            parameter: &name,
            domain:    &id,
        };
        let values: Vec<i64> = view
            .enumerate()
            .unwrap()
            .map(|v| v.as_integer().unwrap())
            .collect();
        assert_eq!(values, vec![1, 2, 3]);
    }

    #[test]
    fn domain_view_enumerate_double_range_is_not_enumerable() {
        let name = pname("r");
        let dd = DoubleDomain::range(0.0, 1.0).unwrap();
        let view = Domain::Double {
            parameter: &name,
            domain:    &dd,
        };
        match view.enumerate() {
            Ok(_) => panic!("expected NotEnumerable"),
            Err(Error::Domain(DomainError::NotEnumerable)) => {}
            Err(other) => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn domain_view_enumerate_boolean_yields_both() {
        let name = pname("b");
        let view = Domain::Boolean { parameter: &name };
        let got: Vec<bool> = view
            .enumerate()
            .unwrap()
            .map(|v| v.as_boolean().unwrap())
            .collect();
        assert_eq!(got, vec![false, true]);
    }

    #[test]
    fn domain_view_sample_produces_valid_integer_value() {
        let mut rng = StdRng::seed_from_u64(9);
        let name = pname("n");
        let id = IntegerDomain::range(1, 100).unwrap();
        let view = Domain::Integer {
            parameter: &name,
            domain:    &id,
        };
        let v = view.sample(&mut rng);
        assert!(view.contains(&v));
        assert!(v.verify_fingerprint());
    }

    #[test]
    fn domain_view_selection_contains_checks_items_fixed() {
        let name = pname("s");
        let sd = SelectionDomain::fixed(selitems(&["a", "b", "c"]), 2).unwrap();
        let view = Domain::Selection {
            parameter: &name,
            domain:    &sd,
        };
        let good = Value::selection(name.clone(), selitems(&["a"]), None);
        let too_many = Value::selection(name.clone(), selitems(&["a", "b", "c"]), None);
        let bad_item = Value::selection(name.clone(), selitems(&["z"]), None);
        assert!(view.contains(&good));
        assert!(!view.contains(&too_many));
        assert!(!view.contains(&bad_item));
    }

    #[test]
    fn domain_view_enumerate_selection_gives_single_item_values() {
        let name = pname("s");
        let sd = SelectionDomain::fixed(selitems(&["a", "b"]), 2).unwrap();
        let view = Domain::Selection {
            parameter: &name,
            domain:    &sd,
        };
        let picks: Vec<String> = view
            .enumerate()
            .unwrap()
            .map(|v| v.as_selection().unwrap().iter().next().unwrap().as_str().to_owned())
            .collect();
        assert_eq!(picks, vec!["a".to_owned(), "b".to_owned()]);
    }

    // ---------- serde ----------

    #[test]
    fn integer_domain_serde_roundtrip_range() {
        let d = IntegerDomain::range(1, 10).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        let back: IntegerDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn selection_domain_serde_roundtrip_fixed() {
        let d = SelectionDomain::fixed(selitems(&["a", "b"]), 2).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        let back: SelectionDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    #[test]
    fn string_domain_serde_roundtrip_regex() {
        let d = StringDomain::regex("^foo$").unwrap();
        let json = serde_json::to_string(&d).unwrap();
        let back: StringDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
