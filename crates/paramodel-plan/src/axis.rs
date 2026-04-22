// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Axis` — a parameter elevated to a study dimension.
//!
//! Per SRD-0008 D3: an axis carries an explicit
//! `(ElementName, ParameterName)` coordinate plus the ordered value
//! list it sweeps. Axis-level self-validation (non-empty values, no
//! duplicates, boundary subset) lives here; cross-element validation
//! (target parameter exists, value kind matches, parameter is not
//! derived, axis name unique, coordinate unique across axes) belongs
//! to `TestPlan::validate` in `plan.rs`.

use paramodel_elements::{Labels, Tags, Value};
use serde::{Deserialize, Serialize};

use crate::error::{PlanError, Result};
use crate::ids::{AxisName, ElementParameterRef};

/// An axis over an element's parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct Axis {
    /// Plan-unique identifier for the axis.
    pub name: AxisName,

    /// The `(element, parameter)` coordinate this axis varies.
    pub target: ElementParameterRef,

    /// Ordered values the axis sweeps.
    pub values: Vec<Value>,

    /// Boundary values for edge-first orderings. When `None`, the
    /// effective boundary set is `[first(values), last(values)]`.
    /// When `Some`, must be a subset of `values`.
    pub boundary_values: Option<Vec<Value>>,

    /// Free-form description.
    pub description: Option<String>,

    /// Intrinsic facts.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,
}

impl Axis {
    /// Number of values on this axis.
    #[must_use]
    pub const fn cardinality(&self) -> usize {
        self.values.len()
    }

    /// The owning element's name.
    #[must_use]
    pub const fn element_name(&self) -> &paramodel_elements::ElementName {
        &self.target.element
    }

    /// The axis's parameter name (within the owning element).
    #[must_use]
    pub const fn parameter_name(&self) -> &paramodel_elements::ParameterName {
        &self.target.parameter
    }

    /// `true` if `value` is one of the axis's values — compared by
    /// content fingerprint (SRD-0004 D10) so construction-time
    /// provenance differences (timestamps, generator tags) don't
    /// cause "same value" to look distinct.
    #[must_use]
    pub fn contains(&self, value: &Value) -> bool {
        let fp = value.fingerprint();
        self.values.iter().any(|v| v.fingerprint() == fp)
    }

    /// Index of `value` in the axis's value list, if present.
    /// Matches by content fingerprint — see [`Self::contains`].
    #[must_use]
    pub fn index_of(&self, value: &Value) -> Option<usize> {
        let fp = value.fingerprint();
        self.values.iter().position(|v| v.fingerprint() == fp)
    }

    /// The boundary set: either the authored `boundary_values`, or
    /// `[first, last]` of `values`. Empty when the axis itself is
    /// empty — [`Self::validate`] rejects that case before this is
    /// called.
    #[must_use]
    pub fn effective_boundary_values(&self) -> Vec<&Value> {
        if let Some(bvs) = &self.boundary_values {
            return bvs.iter().collect();
        }
        let mut out = Vec::new();
        if let Some(first) = self.values.first() {
            out.push(first);
        }
        if let Some(last) = self.values.last()
            && last.fingerprint() != self.values[0].fingerprint()
        {
            out.push(last);
        }
        out
    }

    /// Self-validation: non-empty values, no duplicates (by content
    /// fingerprint), boundary subset. Cross-element checks are in
    /// [`crate::TestPlan::validate`].
    pub fn validate(&self) -> Result<()> {
        if self.values.is_empty() {
            return Err(PlanError::AxisValuesEmpty {
                axis: self.name.as_str().to_owned(),
            });
        }
        // O(n²) content-fingerprint dedup. Axes are tiny.
        for (i, a) in self.values.iter().enumerate() {
            for b in &self.values[i + 1..] {
                if a.fingerprint() == b.fingerprint() {
                    return Err(PlanError::AxisDuplicateValue {
                        axis: self.name.as_str().to_owned(),
                    });
                }
            }
        }
        if let Some(bvs) = &self.boundary_values {
            for bv in bvs {
                if !self.contains(bv) {
                    return Err(PlanError::AxisBoundaryNotSubset {
                        axis: self.name.as_str().to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use paramodel_elements::{ElementName, ParameterName};

    use super::*;

    fn coord(e: &str, p: &str) -> ElementParameterRef {
        ElementParameterRef::new(
            ElementName::new(e).unwrap(),
            ParameterName::new(p).unwrap(),
        )
    }

    fn ivalue(p: &str, v: i64) -> Value {
        Value::integer(ParameterName::new(p).unwrap(), v, None)
    }

    fn axis_name(s: &str) -> AxisName {
        AxisName::new(s).unwrap()
    }

    fn sample_axis(values: Vec<Value>) -> Axis {
        Axis::builder()
            .name(axis_name("threads_axis"))
            .target(coord("db", "threads"))
            .values(values)
            .build()
    }

    #[test]
    fn validate_accepts_canonical_axis() {
        let a = sample_axis(vec![ivalue("threads", 1), ivalue("threads", 2)]);
        assert!(a.validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_values() {
        let a = sample_axis(Vec::new());
        assert!(matches!(
            a.validate(),
            Err(PlanError::AxisValuesEmpty { .. })
        ));
    }

    #[test]
    fn validate_rejects_duplicate_values() {
        let a = sample_axis(vec![ivalue("threads", 2), ivalue("threads", 2)]);
        assert!(matches!(
            a.validate(),
            Err(PlanError::AxisDuplicateValue { .. })
        ));
    }

    #[test]
    fn validate_rejects_boundary_not_subset() {
        let a = Axis::builder()
            .name(axis_name("threads_axis"))
            .target(coord("db", "threads"))
            .values(vec![ivalue("threads", 1), ivalue("threads", 2)])
            .boundary_values(vec![ivalue("threads", 99)])
            .build();
        assert!(matches!(
            a.validate(),
            Err(PlanError::AxisBoundaryNotSubset { .. })
        ));
    }

    #[test]
    fn effective_boundary_values_defaults_to_first_and_last() {
        let a = sample_axis(vec![
            ivalue("threads", 1),
            ivalue("threads", 4),
            ivalue("threads", 8),
        ]);
        let bvs = a.effective_boundary_values();
        assert_eq!(bvs.len(), 2);
        assert_eq!(bvs[0].as_integer(), Some(1));
        assert_eq!(bvs[1].as_integer(), Some(8));
    }

    #[test]
    fn effective_boundary_values_honours_explicit_override() {
        let a = Axis::builder()
            .name(axis_name("threads_axis"))
            .target(coord("db", "threads"))
            .values(vec![
                ivalue("threads", 1),
                ivalue("threads", 4),
                ivalue("threads", 8),
            ])
            .boundary_values(vec![ivalue("threads", 4)])
            .build();
        assert!(a.validate().is_ok());
        let bvs = a.effective_boundary_values();
        assert_eq!(bvs.len(), 1);
        assert_eq!(bvs[0].as_integer(), Some(4));
    }

    #[test]
    fn effective_boundary_values_single_point_has_one_entry() {
        let a = sample_axis(vec![ivalue("threads", 42)]);
        let bvs = a.effective_boundary_values();
        assert_eq!(bvs.len(), 1);
    }

    #[test]
    fn index_of_and_contains() {
        let a = sample_axis(vec![ivalue("threads", 1), ivalue("threads", 8)]);
        assert_eq!(a.index_of(&ivalue("threads", 8)), Some(1));
        assert!(a.contains(&ivalue("threads", 1)));
        assert!(!a.contains(&ivalue("threads", 99)));
    }

    #[test]
    fn serde_roundtrip() {
        let a = sample_axis(vec![ivalue("threads", 1), ivalue("threads", 8)]);
        let json = serde_json::to_string(&a).unwrap();
        let back: Axis = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }
}
