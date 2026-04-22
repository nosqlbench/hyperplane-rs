// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `TrialSet`, `TrialSetId`, and `SamplingStrategy`.
//!
//! A [`TrialSet`] is an immutable collection of [`Trial`]s plus the
//! [`SamplingStrategy`] that produced the population. The `Vec<Trial>`
//! is authoring/enumeration order — reproducible and useful for
//! reporting — and carries **no execution-order semantic**. See
//! SRD-0006 D5 for the full rationale.

use paramodel_elements::{Labels, Tags, Trial};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{Result, TrialError};

// ---------------------------------------------------------------------------
// TrialSetId.
// ---------------------------------------------------------------------------

/// Unique identifier for a [`TrialSet`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TrialSetId(Ulid);

impl TrialSetId {
    /// Construct from a pre-generated ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }

    /// Borrow the inner ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> &Ulid {
        &self.0
    }

    /// Consume and return the inner ULID.
    #[must_use]
    pub const fn into_ulid(self) -> Ulid {
        self.0
    }
}

impl std::fmt::Display for TrialSetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Ulid> for TrialSetId {
    fn from(u: Ulid) -> Self {
        Self(u)
    }
}

// ---------------------------------------------------------------------------
// SamplingStrategy.
// ---------------------------------------------------------------------------

/// How a trial set was populated.
///
/// Direct port of upstream's sealed interface with the same variants;
/// additional strategies (`Pairwise`, `EdgeFirst`, `Adaptive`, …) slot
/// in as new variants without breaking serde compatibility for
/// existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SamplingStrategy {
    /// Exhaustive cartesian product of axis values.
    Grid,
    /// Random sampling with a deterministic seed.
    Random {
        /// Number of trials to draw.
        count: u32,
        /// RNG seed for determinism.
        seed:  u64,
    },
    /// Evenly-spaced points across numeric axes.
    Linspace {
        /// Number of trials to draw.
        count: u32,
    },
}

impl SamplingStrategy {
    /// Grid — the cartesian product. No parameters; infallible.
    #[must_use]
    pub const fn grid() -> Self {
        Self::Grid
    }

    /// Random — `count` must be `>= 1`.
    pub const fn random(count: u32, seed: u64) -> Result<Self> {
        if count == 0 {
            return Err(TrialError::SamplingZeroCount);
        }
        Ok(Self::Random { count, seed })
    }

    /// Linspace — `count` must be `>= 1`.
    pub const fn linspace(count: u32) -> Result<Self> {
        if count == 0 {
            return Err(TrialError::SamplingZeroCount);
        }
        Ok(Self::Linspace { count })
    }
}

// ---------------------------------------------------------------------------
// TrialSet.
// ---------------------------------------------------------------------------

/// An immutable collection of trials together with the strategy that
/// produced them.
///
/// `trials` is stored as `Vec<Trial>` but that ordering is
/// authoring/enumeration order, *not* an execution schedule — see
/// SRD-0006 D5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct TrialSet {
    /// Set id.
    pub id: TrialSetId,

    /// The sampling strategy that produced the population.
    pub strategy: SamplingStrategy,

    /// The trials themselves — enumeration order only.
    #[builder(default)]
    pub trials: Vec<Trial>,

    /// Intrinsic facts attached to the whole set.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,
}

impl TrialSet {
    /// Number of trials in the set.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.trials.len()
    }

    /// `true` when the set carries no trials.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.trials.is_empty()
    }

    /// Enumeration-order iterator over the trials.
    pub fn iter(&self) -> std::slice::Iter<'_, Trial> {
        self.trials.iter()
    }

    /// Look up a trial by its enumeration index. Note that position is
    /// *not* execution position — see SRD-0006 D5.
    #[must_use]
    pub fn get(&self, i: usize) -> Option<&Trial> {
        self.trials.get(i)
    }
}

impl<'a> IntoIterator for &'a TrialSet {
    type Item = &'a Trial;
    type IntoIter = std::slice::Iter<'a, Trial>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use paramodel_elements::{Assignments, ElementName, ParameterName, Trial, TrialId, Value};

    use super::*;
    use std::collections::BTreeMap;

    fn tid(n: u64) -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(n, 1))
    }
    fn setid(n: u64) -> TrialSetId {
        TrialSetId::from_ulid(Ulid::from_parts(n, 1))
    }
    fn trial(n: u64, v: i64) -> Trial {
        let p = ParameterName::new("threads").unwrap();
        let e = ElementName::new("db").unwrap();
        let mut inner = BTreeMap::new();
        inner.insert(p.clone(), Value::integer(p, v, None));
        let mut outer = BTreeMap::new();
        outer.insert(e, inner);
        Trial::builder()
            .id(tid(n))
            .assignments(Assignments::new(outer).unwrap())
            .build()
    }

    // ---------- TrialSetId ----------

    #[test]
    fn trial_set_id_roundtrips() {
        let u = Ulid::from_parts(1_000, 2);
        let id = TrialSetId::from_ulid(u);
        assert_eq!(id.as_ulid(), &u);
        assert_eq!(id.into_ulid(), u);
        assert_eq!(format!("{id}"), format!("{u}"));
    }

    #[test]
    fn trial_set_id_serde_roundtrip() {
        let id = setid(5);
        let json = serde_json::to_string(&id).unwrap();
        let back: TrialSetId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    // ---------- SamplingStrategy ----------

    #[test]
    fn sampling_strategy_constructors() {
        assert_eq!(SamplingStrategy::grid(), SamplingStrategy::Grid);
        assert_eq!(
            SamplingStrategy::random(10, 42).unwrap(),
            SamplingStrategy::Random { count: 10, seed: 42 }
        );
        assert_eq!(
            SamplingStrategy::linspace(5).unwrap(),
            SamplingStrategy::Linspace { count: 5 }
        );
    }

    #[test]
    fn sampling_strategy_rejects_zero_count() {
        assert!(matches!(
            SamplingStrategy::random(0, 1),
            Err(TrialError::SamplingZeroCount)
        ));
        assert!(matches!(
            SamplingStrategy::linspace(0),
            Err(TrialError::SamplingZeroCount)
        ));
    }

    #[test]
    fn sampling_strategy_serde_roundtrip() {
        let s = SamplingStrategy::Random { count: 10, seed: 42 };
        let json = serde_json::to_string(&s).unwrap();
        let back: SamplingStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        // Serialised form uses the adjacent-tag shape.
        assert!(json.contains("\"kind\":\"random\""));
    }

    // ---------- TrialSet ----------

    #[test]
    fn trial_set_builder_and_accessors() {
        let ts = TrialSet::builder()
            .id(setid(1))
            .strategy(SamplingStrategy::grid())
            .trials(vec![trial(1, 4), trial(2, 8), trial(3, 16)])
            .build();
        assert_eq!(ts.len(), 3);
        assert!(!ts.is_empty());
        assert_eq!(ts.iter().count(), 3);
        assert!(ts.get(5).is_none());
        assert_eq!(ts.get(1).unwrap().id, tid(2));
    }

    #[test]
    fn trial_set_with_empty_trials() {
        let ts = TrialSet::builder()
            .id(setid(1))
            .strategy(SamplingStrategy::grid())
            .build();
        assert!(ts.is_empty());
        assert_eq!(ts.len(), 0);
    }

    #[test]
    fn trial_set_serde_roundtrip() {
        let ts = TrialSet::builder()
            .id(setid(1))
            .strategy(SamplingStrategy::linspace(3).unwrap())
            .trials(vec![trial(1, 4), trial(2, 8)])
            .build();
        let json = serde_json::to_string(&ts).unwrap();
        let back: TrialSet = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, back);
    }
}
