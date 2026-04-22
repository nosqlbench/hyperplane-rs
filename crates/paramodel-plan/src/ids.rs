// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Plan-layer identity newtypes.
//!
//! Per SRD-0008 §`TestPlanId`: `TestPlanId` is ULID-based; `PlanName`
//! and `AxisName` are validated string newtypes. `ElementParameterRef`
//! is the canonical `(ElementName, ParameterName)` coordinate used by
//! axes and plan bindings.

use paramodel_elements::{ElementName, NameError, ParameterName, name_type};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// TestPlanId.
// ---------------------------------------------------------------------------

/// Unique identifier for a [`super::TestPlan`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TestPlanId(Ulid);

impl TestPlanId {
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

impl std::fmt::Display for TestPlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Ulid> for TestPlanId {
    fn from(u: Ulid) -> Self {
        Self(u)
    }
}

// ---------------------------------------------------------------------------
// PlanName — validated string newtype.
// ---------------------------------------------------------------------------

/// Human-authored identifier for a test plan.
///
/// Validation: non-empty, ≤ 128 bytes, ASCII alphanumeric + `_-.`
/// (first char must be a letter or underscore). Slightly looser than
/// [`paramodel_elements::ParameterName`] on length only.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlanName(String);

const PLAN_NAME_MAX_LEN: usize = 128;

impl PlanName {
    /// Construct a new plan name, validating the candidate.
    pub fn new(candidate: impl Into<String>) -> std::result::Result<Self, NameError> {
        let s = candidate.into();
        if s.is_empty() {
            return Err(NameError::Empty);
        }
        if s.len() > PLAN_NAME_MAX_LEN {
            return Err(NameError::TooLong {
                length: s.len(),
                max:    PLAN_NAME_MAX_LEN,
            });
        }
        let first = s.chars().next().expect("non-empty checked above");
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

    /// Consume and return the inner string.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for PlanName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PlanName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for PlanName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PlanName({:?})", self.0)
    }
}

impl Serialize for PlanName {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PlanName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// AxisName — validated identifier newtype via the shared macro.
// ---------------------------------------------------------------------------

name_type! {
    /// Plan-unique axis label. Follows the same character class as
    /// [`paramodel_elements::ParameterName`].
    pub struct AxisName { kind: "AxisName" }
}

// ---------------------------------------------------------------------------
// ElementParameterRef — (element, parameter) coordinate.
// ---------------------------------------------------------------------------

/// The canonical `(element, parameter)` pair used by axes and plan
/// bindings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ElementParameterRef {
    /// The owning element's name.
    pub element:   ElementName,
    /// The parameter's name within that element.
    pub parameter: ParameterName,
}

impl ElementParameterRef {
    /// Construct a new coordinate.
    #[must_use]
    pub const fn new(element: ElementName, parameter: ParameterName) -> Self {
        Self { element, parameter }
    }
}

impl std::fmt::Display for ElementParameterRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.element, self.parameter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_id_roundtrip_and_display() {
        let u = Ulid::from_parts(1_000, 2);
        let id = TestPlanId::from_ulid(u);
        assert_eq!(id.as_ulid(), &u);
        assert_eq!(id.into_ulid(), u);
        assert_eq!(format!("{id}"), format!("{u}"));
    }

    #[test]
    fn plan_name_rejects_bad_inputs() {
        assert!(PlanName::new("").is_err());
        assert!(PlanName::new("1starts_with_digit").is_err());
        assert!(PlanName::new("has space").is_err());
        let overlong = "a".repeat(PLAN_NAME_MAX_LEN + 1);
        assert!(PlanName::new(overlong).is_err());
    }

    #[test]
    fn plan_name_accepts_valid_candidates() {
        for s in ["my_study", "vectorbench-v1", "study.42", "_scratch"] {
            PlanName::new(s).expect(s);
        }
    }

    #[test]
    fn plan_name_serde_roundtrip() {
        let n = PlanName::new("vectorbench").unwrap();
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(json, "\"vectorbench\"");
        let back: PlanName = serde_json::from_str(&json).unwrap();
        assert_eq!(n, back);
    }

    #[test]
    fn plan_name_deserialise_rejects_invalid() {
        assert!(serde_json::from_str::<PlanName>("\"\"").is_err());
    }

    #[test]
    fn axis_name_via_macro_validates() {
        AxisName::new("axis_a").unwrap();
        assert!(AxisName::new("1starts_with_digit").is_err());
    }

    #[test]
    fn element_parameter_ref_display_is_dotted() {
        let r = ElementParameterRef::new(
            ElementName::new("db").unwrap(),
            ParameterName::new("threads").unwrap(),
        );
        assert_eq!(format!("{r}"), "db.threads");
    }

    #[test]
    fn element_parameter_ref_serde_roundtrip() {
        let r = ElementParameterRef::new(
            ElementName::new("db").unwrap(),
            ParameterName::new("threads").unwrap(),
        );
        let json = serde_json::to_string(&r).unwrap();
        let back: ElementParameterRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
