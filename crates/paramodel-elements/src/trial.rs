// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Trial`, `Assignments`, `TrialMetadata`, and the trial canonical
//! byte encoding.
//!
//! A [`Trial`] is one immutable configuration — an assignment of
//! [`Value`]s across every (element, parameter) coordinate that will
//! run together, plus an identifier and optional metadata.
//! Construction goes through [`Trial::builder`]; [`Assignments::new`]
//! enforces self-consistency on the assignment map.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::attributes::{Labels, Tags};
use crate::ids::TrialId;
use crate::names::{ElementName, NameError, ParameterName};
use crate::value::Value;

// ---------------------------------------------------------------------------
// TrialError — construction-time validation for Trial / Assignments.
// ---------------------------------------------------------------------------

/// Errors from `Trial` / `Assignments` construction.
///
/// Separate from the broader crate `Error` so the construction path
/// can have a narrow, matchable type. Wrapped by `Error::Trial`
/// via `#[from]`.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TrialError {
    /// A name newtype failed validation.
    #[error(transparent)]
    Name(#[from] NameError),

    /// An element's inner parameter map is empty.
    #[error("element '{element}' has no parameter assignments; an Assignments entry must be non-empty")]
    EmptyElementAssignments {
        /// The offending element name.
        element: String,
    },

    /// A value is keyed under a parameter name that doesn't match its
    /// own `Provenance.parameter`.
    #[error(
        "mis-addressed value: assignment key '{expected}' does not match value.provenance.parameter '{actual}'"
    )]
    MisaddressedValue {
        /// The outer-map key this value was stored under.
        expected: ParameterName,
        /// The name the value's provenance records.
        actual:   ParameterName,
    },
}

type Result<T, E = TrialError> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// Assignments.
// ---------------------------------------------------------------------------

/// Two-level map: element name → parameter name → value.
///
/// Construction via [`Self::new`] enforces:
///
/// - For every `(element, param) → value` entry,
///   `value.provenance().parameter == param`. No silently
///   mis-addressed values.
/// - No element appears with an empty inner map.
///
/// `BTreeMap` iteration order is natural, so trial canonical bytes
/// (and anything else that walks the assignments) see stable
/// ordering without extra sorting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Assignments(BTreeMap<ElementName, BTreeMap<ParameterName, Value>>);

impl Assignments {
    /// Empty assignments map.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct from a pre-built map, validating invariants.
    pub fn new(map: BTreeMap<ElementName, BTreeMap<ParameterName, Value>>) -> Result<Self> {
        for (element, params) in &map {
            if params.is_empty() {
                return Err(TrialError::EmptyElementAssignments {
                    element: element.as_str().to_owned(),
                });
            }
            for (pname, value) in params {
                if value.parameter() != pname {
                    return Err(TrialError::MisaddressedValue {
                        expected: pname.clone(),
                        actual:   value.parameter().clone(),
                    });
                }
            }
        }
        Ok(Self(map))
    }

    /// Look up one `(element, parameter)` binding.
    #[must_use]
    pub fn get(&self, element: &ElementName, param: &ParameterName) -> Option<&Value> {
        self.0.get(element).and_then(|p| p.get(param))
    }

    /// Borrow the inner parameter map for an element.
    #[must_use]
    pub fn for_element(
        &self,
        element: &ElementName,
    ) -> Option<&BTreeMap<ParameterName, Value>> {
        self.0.get(element)
    }

    /// Sorted iterator over `(element, parameter, value)` triples.
    pub fn iter(&self) -> impl Iterator<Item = (&ElementName, &ParameterName, &Value)> {
        self.0
            .iter()
            .flat_map(|(e, params)| params.iter().map(move |(p, v)| (e, p, v)))
    }

    /// Total assignment count across every element.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.values().map(BTreeMap::len).sum()
    }

    /// `true` when the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of distinct elements covered.
    #[must_use]
    pub fn element_count(&self) -> usize {
        self.0.len()
    }

    /// Internal: borrow the raw map (used for canonical bytes).
    pub(crate) const fn as_map(&self) -> &BTreeMap<ElementName, BTreeMap<ParameterName, Value>> {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// TrialMetadata.
// ---------------------------------------------------------------------------

/// Authoring/enumeration hints attached to a trial.
///
/// Every field is advisory; the planner treats an absent value the
/// same as one that was never set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct TrialMetadata {
    /// Position in the producing `TrialSet`, when that position is
    /// meaningful for reporting (not for scheduling).
    pub enumeration_index: Option<u32>,

    /// Free-form group label (for related-trial clustering).
    pub group: Option<String>,

    /// Human-readable description of how the trial was generated.
    pub generation_method: Option<String>,

    /// Scheduler-hint priority; higher = earlier when otherwise equal.
    pub priority: Option<i32>,
}

// ---------------------------------------------------------------------------
// Trial.
// ---------------------------------------------------------------------------

/// One immutable trial configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct Trial {
    /// Trial id.
    pub id: TrialId,

    /// The parameter assignments for every element in scope.
    pub assignments: Assignments,

    /// Intrinsic facts (e.g. the compiler-stamped `trial_code` label).
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,

    /// Advisory trial metadata.
    pub metadata: Option<TrialMetadata>,
}

/// Canonical encoding tag for a trial. Chosen outside the constraint
/// canonical-byte ranges (`0x10..=0x5A`) and value tag ranges
/// (`0x01..=0x05`) used elsewhere in the workspace.
pub const TRIAL_TAG: u8 = 0x60;

impl Trial {
    /// Stable byte encoding of this trial's *content*. Excludes
    /// `id`, `metadata`, `labels`, and `tags` — those are ancillary
    /// to the trial's identity. See SRD-0006 §Canonical bytes.
    ///
    /// Callers compute a hash at a trust boundary (persistence write,
    /// audit compare) with
    /// `Fingerprint::of(&trial.canonical_bytes())`.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(TRIAL_TAG);

        let assignments = self.assignments.as_map();
        let n_elements = u32::try_from(assignments.len()).expect("element count fits in u32");
        out.extend_from_slice(&n_elements.to_le_bytes());

        for (element_name, params) in assignments {
            write_str_len_prefixed(&mut out, element_name.as_str());
            let n_params = u32::try_from(params.len()).expect("parameter count fits in u32");
            out.extend_from_slice(&n_params.to_le_bytes());
            for (param_name, value) in params {
                write_str_len_prefixed(&mut out, param_name.as_str());
                // The 32-byte value fingerprint is the pre-hashed
                // content address of the assignment — we inherit
                // per-value tamper-checking for free (SRD-0004 D10).
                out.extend_from_slice(value.fingerprint().as_bytes());
            }
        }

        out
    }
}

fn write_str_len_prefixed(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let len = u32::try_from(bytes.len()).expect("string length fits in u32");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{Fingerprint, IntegerValue, LabelKey, LabelValue, Value};
    use ulid::Ulid;

    use super::*;

    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }
    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }
    fn tid(n: u64) -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(n, 1))
    }

    fn one_element_assignments(
        elem:  &str,
        param: &str,
        value: i64,
    ) -> Assignments {
        let mut inner = BTreeMap::new();
        inner.insert(pname(param), Value::integer(pname(param), value, None));
        let mut outer = BTreeMap::new();
        outer.insert(ename(elem), inner);
        Assignments::new(outer).unwrap()
    }

    // ---------- Assignments ----------

    #[test]
    fn assignments_rejects_empty_inner_map() {
        let mut outer = BTreeMap::new();
        outer.insert(ename("db"), BTreeMap::new());
        let err = Assignments::new(outer).unwrap_err();
        assert!(matches!(err, TrialError::EmptyElementAssignments { .. }));
    }

    #[test]
    fn assignments_rejects_misaddressed_value() {
        // Key says "threads" but value's provenance says "connections".
        let mut inner = BTreeMap::new();
        inner.insert(
            pname("threads"),
            Value::integer(pname("connections"), 8, None),
        );
        let mut outer = BTreeMap::new();
        outer.insert(ename("db"), inner);
        let err = Assignments::new(outer).unwrap_err();
        assert!(matches!(err, TrialError::MisaddressedValue { .. }));
    }

    #[test]
    fn assignments_iter_visits_every_triple() {
        let mut inner = BTreeMap::new();
        inner.insert(pname("a"), Value::integer(pname("a"), 1, None));
        inner.insert(pname("b"), Value::integer(pname("b"), 2, None));
        let mut outer = BTreeMap::new();
        outer.insert(ename("x"), inner);
        let a = Assignments::new(outer).unwrap();
        let triples: Vec<(&str, &str, i64)> = a
            .iter()
            .map(|(e, p, v)| (e.as_str(), p.as_str(), v.as_integer().unwrap()))
            .collect();
        assert_eq!(triples, vec![("x", "a", 1), ("x", "b", 2)]);
    }

    #[test]
    fn assignments_len_sums_across_elements() {
        let mut inner1 = BTreeMap::new();
        inner1.insert(pname("a"), Value::integer(pname("a"), 1, None));
        let mut inner2 = BTreeMap::new();
        inner2.insert(pname("a"), Value::integer(pname("a"), 1, None));
        inner2.insert(pname("b"), Value::integer(pname("b"), 2, None));
        let mut outer = BTreeMap::new();
        outer.insert(ename("x"), inner1);
        outer.insert(ename("y"), inner2);
        let a = Assignments::new(outer).unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a.element_count(), 2);
    }

    #[test]
    fn assignments_serde_roundtrip() {
        let a = one_element_assignments("db", "threads", 8);
        let json = serde_json::to_string(&a).unwrap();
        let back: Assignments = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    // ---------- Trial ----------

    #[test]
    fn trial_builds_and_accesses_its_fields() {
        let t = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        assert_eq!(t.id, tid(1));
        assert_eq!(t.assignments.len(), 1);
        assert!(t.labels.is_empty());
        assert!(t.metadata.is_none());
    }

    #[test]
    fn trial_metadata_builder() {
        let md = TrialMetadata::builder()
            .enumeration_index(3)
            .group("baseline".to_owned())
            .priority(10)
            .build();
        assert_eq!(md.enumeration_index, Some(3));
        assert_eq!(md.group.as_deref(), Some("baseline"));
        assert_eq!(md.priority, Some(10));
    }

    #[test]
    fn trial_serde_roundtrip() {
        let t = Trial::builder()
            .id(tid(42))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        let json = serde_json::to_string(&t).unwrap();
        let back: Trial = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    // ---------- canonical_bytes ----------

    #[test]
    fn canonical_bytes_deterministic() {
        let a = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        let b = Trial::builder()
            .id(tid(2)) // different id
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        // Different ids; canonical bytes must match (id is excluded).
        assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    }

    #[test]
    fn canonical_bytes_distinguish_values() {
        let a = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        let b = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 16))
            .build();
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
    }

    #[test]
    fn canonical_bytes_distinguish_element_names() {
        let a = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        let b = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("cache", "threads", 8))
            .build();
        assert_ne!(a.canonical_bytes(), b.canonical_bytes());
    }

    #[test]
    fn canonical_bytes_excludes_labels_and_metadata() {
        let mut labels = Labels::new();
        labels.insert(
            LabelKey::new("trial_code").unwrap(),
            LabelValue::new("0x0001").unwrap(),
        );
        let with_labels = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .labels(labels)
            .metadata(
                TrialMetadata::builder()
                    .enumeration_index(7)
                    .build(),
            )
            .build();
        let plain = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 8))
            .build();
        assert_eq!(with_labels.canonical_bytes(), plain.canonical_bytes());
    }

    #[test]
    fn canonical_bytes_match_hand_built_layout() {
        let t = Trial::builder()
            .id(tid(1))
            .assignments(one_element_assignments("db", "threads", 42))
            .build();
        let got = t.canonical_bytes();

        // Re-create the expected layout independently so a future
        // refactor can't silently change the wire form.
        let mut expected = vec![TRIAL_TAG];
        expected.extend_from_slice(&1u32.to_le_bytes()); // 1 element
        let elem = "db";
        expected.extend_from_slice(&u32::try_from(elem.len()).unwrap().to_le_bytes());
        expected.extend_from_slice(elem.as_bytes());
        expected.extend_from_slice(&1u32.to_le_bytes()); // 1 param
        let param = "threads";
        expected.extend_from_slice(&u32::try_from(param.len()).unwrap().to_le_bytes());
        expected.extend_from_slice(param.as_bytes());
        let fp = IntegerValue::fingerprint_of(&pname("threads"), 42);
        expected.extend_from_slice(fp.as_bytes());
        assert_eq!(got, expected);
        // And the derived hash is a deterministic function of the
        // canonical bytes, as documented.
        let _ = Fingerprint::of(&got);
    }
}
