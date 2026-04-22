// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `OptimizationStrategy` and `TrialOrdering`.
//!
//! Plan-layer enums that the compiler and executor consume. Exact
//! semantics are pinned in the compilation SRD (SRD-0010). This module
//! only pins the shape and defaults.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// OptimizationStrategy.
// ---------------------------------------------------------------------------

/// Compiler-side optimisation level applied when deriving the
/// execution graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationStrategy {
    /// No optimisation. Every per-trial activate/deactivate step is
    /// materialised.
    None,
    /// Safe, uncontroversial reductions (group coalescing under
    /// PERSIST, transitive-edge removal). Default.
    #[default]
    Basic,
    /// Aggressively eliminate redundant trials where the plan can
    /// prove equivalence.
    PruneRedundant,
    /// All of the above plus reordering for cache locality and
    /// resource packing.
    Aggressive,
}

// ---------------------------------------------------------------------------
// TrialOrdering.
// ---------------------------------------------------------------------------

/// How the compiler / scheduler walks through the trial space.
///
/// `Custom { name }` dispatches through a compiler-side registry at
/// compile time — the name lookup fails as a compile error if
/// unregistered (SRD-0008 D15).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrialOrdering {
    /// Lexicographic traversal in axis definition order.
    #[default]
    Sequential,
    /// Pseudo-random permutation (reproducible given the seed).
    Shuffled {
        /// RNG seed for determinism.
        seed: u64,
    },
    /// Three-phase: corners, edges, interior fill.
    EdgeFirst,
    /// Minimise element deploy/teardown churn.
    DependencyOptimized,
    /// Expensive trials first (fail-fast).
    CostOptimized,
    /// Custom ordering looked up by name in the compiler registry.
    Custom {
        /// Registered ordering name.
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimization_strategy_defaults_to_basic() {
        assert_eq!(
            OptimizationStrategy::default(),
            OptimizationStrategy::Basic
        );
    }

    #[test]
    fn trial_ordering_defaults_to_sequential() {
        assert_eq!(TrialOrdering::default(), TrialOrdering::Sequential);
    }

    #[test]
    fn optimization_strategy_serde_is_snake_case() {
        let s = serde_json::to_string(&OptimizationStrategy::PruneRedundant).unwrap();
        assert_eq!(s, "\"prune_redundant\"");
        let back: OptimizationStrategy = serde_json::from_str(&s).unwrap();
        assert_eq!(back, OptimizationStrategy::PruneRedundant);
    }

    #[test]
    fn trial_ordering_shuffled_and_custom_serde_roundtrip() {
        let s = TrialOrdering::Shuffled { seed: 42 };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"shuffled\""));
        assert!(json.contains("\"seed\":42"));
        let back: TrialOrdering = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);

        let c = TrialOrdering::Custom {
            name: "corners_then_mid".to_owned(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: TrialOrdering = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back);
    }
}
