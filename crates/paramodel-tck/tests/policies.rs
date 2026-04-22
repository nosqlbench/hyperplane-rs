// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionPolicies` / `RetryPolicy` / `BackoffStrategy` /
//! `OnFailure` / `InterventionMode` / `PartialRunBehavior`
//! validation + semantic coverage.

use std::time::Duration;

use paramodel_plan::{
    BackoffStrategy, ExecutionPolicies, InterventionMode, OnFailure, PartialRunBehavior,
    RetryPolicy,
};

// ---------------------------------------------------------------------------
// RetryPolicy::none and validate.
// ---------------------------------------------------------------------------

#[test]
fn retry_policy_none_has_single_attempt() {
    let p = RetryPolicy::none();
    assert_eq!(p.max_attempts, 1);
    assert!(p.retryable_errors.is_empty());
    assert!(p.validate().is_ok());
}

#[test]
fn retry_policy_rejects_zero_attempts() {
    let mut p = RetryPolicy::none();
    p.max_attempts = 0;
    assert!(p.validate().is_err());
}

// ---------------------------------------------------------------------------
// BackoffStrategy delay curves.
// ---------------------------------------------------------------------------

#[test]
fn backoff_immediate_is_zero_every_time() {
    let s = BackoffStrategy::Immediate;
    for attempt in 0..5 {
        assert_eq!(s.delay_for_attempt(attempt), Duration::ZERO);
    }
}

#[test]
fn backoff_fixed_is_constant() {
    let s = BackoffStrategy::Fixed {
        delay: Duration::from_millis(250),
    };
    for attempt in 0..5 {
        assert_eq!(s.delay_for_attempt(attempt), Duration::from_millis(250));
    }
}

#[test]
fn backoff_linear_scales_by_attempt() {
    let s = BackoffStrategy::Linear {
        base: Duration::from_millis(100),
    };
    assert_eq!(s.delay_for_attempt(0), Duration::ZERO);
    assert_eq!(s.delay_for_attempt(1), Duration::from_millis(100));
    assert_eq!(s.delay_for_attempt(3), Duration::from_millis(300));
}

#[test]
fn backoff_exponential_doubles_with_factor_2() {
    let s = BackoffStrategy::Exponential {
        factor:  2.0,
        initial: Duration::from_millis(100),
    };
    assert_eq!(s.delay_for_attempt(0), Duration::from_millis(100));
    assert_eq!(s.delay_for_attempt(1), Duration::from_millis(200));
    assert_eq!(s.delay_for_attempt(2), Duration::from_millis(400));
    assert_eq!(s.delay_for_attempt(3), Duration::from_millis(800));
}

// ---------------------------------------------------------------------------
// ExecutionPolicies defaults + validation.
// ---------------------------------------------------------------------------

#[test]
fn execution_policies_defaults_are_sensible() {
    let p = ExecutionPolicies::defaults();
    assert_eq!(p.trial_retry.max_attempts, 1);
    assert!(p.trial_timeout.is_none());
    assert_eq!(p.intervention_mode, InterventionMode::Immediate);
    assert_eq!(p.partial_run_behavior, PartialRunBehavior::RetainResults);
    assert!(matches!(p.on_failure, OnFailure::Skip));
    assert!(p.validate().is_ok());
}

#[test]
fn execution_policies_validate_propagates_retry_error() {
    let mut p = ExecutionPolicies::defaults();
    p.trial_retry.max_attempts = 0;
    assert!(p.validate().is_err());
}

// ---------------------------------------------------------------------------
// OnFailure serde roundtrips (all variants).
// ---------------------------------------------------------------------------

#[test]
fn on_failure_serde_all_variants() {
    for f in [
        OnFailure::Stop,
        OnFailure::Skip,
        OnFailure::Retry { attempts: 3 },
    ] {
        let json = serde_json::to_string(&f).unwrap();
        let back: OnFailure = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }
}

// ---------------------------------------------------------------------------
// InterventionMode + PartialRunBehavior serde.
// ---------------------------------------------------------------------------

#[test]
fn intervention_mode_serde_roundtrip() {
    for m in [InterventionMode::Immediate, InterventionMode::AfterActiveTrials] {
        let json = serde_json::to_string(&m).unwrap();
        let back: InterventionMode = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }
}

#[test]
fn partial_run_behavior_serde_roundtrip() {
    for b in [PartialRunBehavior::RetainResults, PartialRunBehavior::FailRun] {
        let json = serde_json::to_string(&b).unwrap();
        let back: PartialRunBehavior = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }
}
