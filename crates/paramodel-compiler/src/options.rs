// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `CompilerOptions` and its auxiliary types.

use std::collections::BTreeSet;

use crate::error::WarningCode;

/// Tunable compiler knobs.
#[derive(Debug, Clone)]
pub struct CompilerOptions {
    /// Whether to insert `ReadinessGate` steps after `Activate` nodes
    /// for elements carrying a `HealthCheckSpec`.
    ///
    /// v0.1 compiler ignores this field; defaults to `true` so future
    /// slices pick it up transparently.
    pub include_health_gates: bool,

    /// Whether to run Rule 8 transitive reduction after assembling the
    /// graph. v0.1 ignores this; defaults to `true`.
    pub include_transitive_reduction: bool,

    /// Whether to insert `Checkpoint` steps per the plan's
    /// `CheckpointStrategy`. v0.1 ignores this; defaults to `true`.
    pub include_checkpoints: bool,

    /// Hard cap on warnings reported per compile. `None` = uncapped.
    pub max_warnings: Option<u32>,

    /// Warning codes to escalate to errors.
    pub fail_on_warning_codes: BTreeSet<WarningCode>,
}

impl Default for CompilerOptions {
    fn default() -> Self {
        Self {
            include_health_gates:         true,
            include_transitive_reduction: true,
            include_checkpoints:          true,
            max_warnings:                 None,
            fail_on_warning_codes:        BTreeSet::new(),
        }
    }
}

/// Host-provided registry mapping custom `TrialOrdering::Custom { name }`
/// values to their scheduling policies.
///
/// v0.1 compiler emits a warning for any unknown custom name but does
/// not refuse to compile — execution-time dispatch is the executor
/// SRD's concern.
pub trait CustomOrderingRegistry: std::fmt::Debug + Send + Sync + 'static {
    /// Names that are registered.
    fn known_names(&self) -> Vec<String>;

    /// `true` if a given name is registered.
    fn is_registered(&self, name: &str) -> bool {
        self.known_names().iter().any(|n| n == name)
    }
}

/// A registry with no custom orderings — always answers `false`.
#[derive(Debug, Default)]
pub struct EmptyCustomOrderingRegistry;

impl CustomOrderingRegistry for EmptyCustomOrderingRegistry {
    fn known_names(&self) -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_permissive() {
        let o = CompilerOptions::default();
        assert!(o.include_health_gates);
        assert!(o.include_transitive_reduction);
        assert!(o.include_checkpoints);
        assert!(o.max_warnings.is_none());
        assert!(o.fail_on_warning_codes.is_empty());
    }

    #[test]
    fn empty_registry_knows_nothing() {
        let r = EmptyCustomOrderingRegistry;
        assert!(r.known_names().is_empty());
        assert!(!r.is_registered("anything"));
    }
}
