// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Parameter / value validation outcomes.
//!
//! Direct port of the upstream Java sealed interface, but with the
//! "no nested `Warning`" invariant enforced by the [`ValidationResult::warn`]
//! constructor: any caller that hands a `Warning` as the inner result has
//! that inner `Warning` unwrapped before construction.

use serde::{Deserialize, Serialize};

/// The outcome of validating a value against a parameter or constraint.
///
/// See SRD-0004 §`ValidationResult`. The `Warning` variant is flattened by
/// construction (never `Warning { underlying: Warning { .. } }`); callers
/// should use [`Self::warn`] rather than constructing the variant directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValidationResult {
    /// The value passed all checks.
    Passed,

    /// The value failed at least one check.
    Failed {
        /// High-level summary of the failure.
        message:    String,
        /// Per-check violation messages (in encounter order).
        violations: Vec<String>,
    },

    /// The value is acceptable but worth flagging. Exactly one level of
    /// warning may wrap an inner result.
    Warning {
        /// High-level summary of the warning.
        message:    String,
        /// The wrapped result (never itself a `Warning`).
        underlying: Box<Self>,
    },
}

impl ValidationResult {
    /// Construct a passing result. Provided for symmetry with the other
    /// constructors.
    #[must_use]
    pub const fn passed() -> Self {
        Self::Passed
    }

    /// Construct a failing result.
    #[must_use]
    pub fn failed(message: impl Into<String>, violations: Vec<String>) -> Self {
        Self::Failed {
            message: message.into(),
            violations,
        }
    }

    /// Wrap an inner result with a warning. Flattens nested warnings.
    ///
    /// If `inner` is itself a `Warning`, its own inner result is lifted
    /// so the returned value has exactly one warning layer. Messages
    /// from discarded inner warnings are lost; use the outer `message`
    /// to carry anything the caller needs to preserve.
    #[must_use]
    pub fn warn(message: impl Into<String>, inner: Self) -> Self {
        let underlying = match inner {
            Self::Warning { underlying, .. } => underlying,
            other => Box::new(other),
        };
        Self::Warning {
            message: message.into(),
            underlying,
        }
    }

    /// `true` when the effective outcome is a pass (either `Passed` or a
    /// warning whose underlying result is `Passed`).
    #[must_use]
    pub fn is_passed(&self) -> bool {
        match self {
            Self::Passed => true,
            Self::Warning { underlying, .. } => matches!(**underlying, Self::Passed),
            Self::Failed { .. } => false,
        }
    }

    /// `true` when the effective outcome is a failure.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        match self {
            Self::Failed { .. } => true,
            Self::Warning { underlying, .. } => matches!(**underlying, Self::Failed { .. }),
            Self::Passed => false,
        }
    }

    /// Borrow the violation list, if any. Descends one level of warning.
    #[must_use]
    pub fn violations(&self) -> &[String] {
        match self {
            Self::Failed { violations, .. } => violations,
            Self::Warning { underlying, .. } => match underlying.as_ref() {
                Self::Failed { violations, .. } => violations,
                _ => &[],
            },
            Self::Passed => &[],
        }
    }

    /// Borrow the top-level summary message, if any.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Passed => None,
            Self::Failed { message, .. } | Self::Warning { message, .. } => Some(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passed_is_passed() {
        let r = ValidationResult::passed();
        assert!(r.is_passed());
        assert!(!r.is_failed());
        assert!(r.violations().is_empty());
        assert_eq!(r.message(), None);
    }

    #[test]
    fn failed_is_failed() {
        let r = ValidationResult::failed("bad", vec!["value < 0".into()]);
        assert!(!r.is_passed());
        assert!(r.is_failed());
        assert_eq!(r.violations(), &["value < 0".to_owned()]);
        assert_eq!(r.message(), Some("bad"));
    }

    #[test]
    fn warn_around_passed_reports_passed() {
        let r = ValidationResult::warn("fyi", ValidationResult::passed());
        assert!(r.is_passed());
        assert!(!r.is_failed());
        assert_eq!(r.message(), Some("fyi"));
    }

    #[test]
    fn warn_around_failed_reports_failed() {
        let inner = ValidationResult::failed("bad", vec!["out of range".into()]);
        let r = ValidationResult::warn("note", inner);
        assert!(!r.is_passed());
        assert!(r.is_failed());
        assert_eq!(r.violations(), &["out of range".to_owned()]);
    }

    #[test]
    fn warn_flattens_nested_warnings() {
        let inner = ValidationResult::failed("bad", vec!["x".into()]);
        let once = ValidationResult::warn("w1", inner);
        let twice = ValidationResult::warn("w2", once);
        match &twice {
            ValidationResult::Warning { message, underlying } => {
                assert_eq!(message, "w2");
                assert!(matches!(
                    underlying.as_ref(),
                    ValidationResult::Failed { .. }
                ));
            }
            _ => panic!("expected Warning"),
        }
    }

    #[test]
    fn serde_roundtrip_failed() {
        let r = ValidationResult::failed("bad", vec!["v1".into(), "v2".into()]);
        let json = serde_json::to_string(&r).unwrap();
        let back: ValidationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn serde_roundtrip_warning() {
        let r = ValidationResult::warn("fyi", ValidationResult::passed());
        let json = serde_json::to_string(&r).unwrap();
        let back: ValidationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
