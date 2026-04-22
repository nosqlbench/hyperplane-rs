// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Typed dependency edges between elements.
//!
//! Per SRD-0002 R1 and SRD-0007 D3/D4, every edge in the Element Graph
//! is a `Dependency { target, relationship }` pair. The relationship
//! drives compiler decisions (serialisation, coalescing, lifecycle
//! ordering, lifeline collapse); the composition rules live in the
//! compilation SRD.

use crate::ElementName;
use serde::{Deserialize, Serialize};

/// How a dependent element relates to its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipType {
    /// Concurrent access is allowed. The default edge shape.
    Shared,
    /// Serialise dependents against the target (reducto Rule 2
    /// inserts serialisation barriers).
    Exclusive,
    /// One target instance per dependent — the target gets coalesced
    /// with its owner by the compiler.
    Dedicated,
    /// Full-lifecycle ordering within the same trial scope: the
    /// target must complete before the dependent starts.
    Linear,
    /// Target's teardown subsumes the dependent's — the dependent
    /// has no explicit teardown step.
    Lifeline,
}

impl RelationshipType {
    /// `true` for relationships that require the compiler to insert
    /// a serialisation barrier around the target.
    #[must_use]
    pub const fn requires_serialization_barrier(&self) -> bool {
        matches!(self, Self::Exclusive)
    }

    /// `true` for relationships that require a dedicated target
    /// instance per dependent (coalesced with its owner).
    #[must_use]
    pub const fn requires_dedicated_instance(&self) -> bool {
        matches!(self, Self::Dedicated)
    }

    /// `true` for relationships whose teardown is folded into the
    /// target's.
    #[must_use]
    pub const fn implies_lifecycle_coupling(&self) -> bool {
        matches!(self, Self::Lifeline)
    }

    /// `true` for the `Linear` full-lifecycle-ordering relationship.
    #[must_use]
    pub const fn is_linear(&self) -> bool {
        matches!(self, Self::Linear)
    }
}

/// One edge in the Element Graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Dependency {
    /// The element this dependency points at.
    pub target:       ElementName,
    /// The relationship kind.
    pub relationship: RelationshipType,
}

impl Dependency {
    /// Shared dependency — the default relationship.
    #[must_use]
    pub const fn shared(target: ElementName) -> Self {
        Self {
            target,
            relationship: RelationshipType::Shared,
        }
    }

    /// Exclusive dependency — serialise dependents against `target`.
    #[must_use]
    pub const fn exclusive(target: ElementName) -> Self {
        Self {
            target,
            relationship: RelationshipType::Exclusive,
        }
    }

    /// Dedicated dependency — one target instance per dependent.
    #[must_use]
    pub const fn dedicated(target: ElementName) -> Self {
        Self {
            target,
            relationship: RelationshipType::Dedicated,
        }
    }

    /// Linear dependency — full lifecycle ordering.
    #[must_use]
    pub const fn linear(target: ElementName) -> Self {
        Self {
            target,
            relationship: RelationshipType::Linear,
        }
    }

    /// Lifeline dependency — teardown folded into `target`.
    #[must_use]
    pub const fn lifeline(target: ElementName) -> Self {
        Self {
            target,
            relationship: RelationshipType::Lifeline,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }

    #[test]
    fn helper_predicates_match_variant() {
        assert!(RelationshipType::Exclusive.requires_serialization_barrier());
        assert!(!RelationshipType::Shared.requires_serialization_barrier());
        assert!(RelationshipType::Dedicated.requires_dedicated_instance());
        assert!(RelationshipType::Lifeline.implies_lifecycle_coupling());
        assert!(RelationshipType::Linear.is_linear());
        assert!(!RelationshipType::Shared.is_linear());
    }

    #[test]
    fn constructors_set_the_right_relationship() {
        let t = name("db");
        assert_eq!(Dependency::shared(t.clone()).relationship, RelationshipType::Shared);
        assert_eq!(Dependency::exclusive(t.clone()).relationship, RelationshipType::Exclusive);
        assert_eq!(Dependency::dedicated(t.clone()).relationship, RelationshipType::Dedicated);
        assert_eq!(Dependency::linear(t.clone()).relationship, RelationshipType::Linear);
        assert_eq!(Dependency::lifeline(t).relationship, RelationshipType::Lifeline);
    }

    #[test]
    fn serde_roundtrip() {
        let d = Dependency::exclusive(name("db"));
        let json = serde_json::to_string(&d).unwrap();
        let back: Dependency = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
