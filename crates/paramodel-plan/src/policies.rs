// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Execution policies carried on a `TestPlan`.
//!
//! Per SRD-0008 D7: `ExecutionPolicies` bundles trial retry, element
//! deployment retry, trial timeout, element start timeout,
//! `InterventionMode`, `PartialRunBehavior`, and `OnFailure`.
//! `ExecutionPolicies::defaults()` is the "no retry, no timeout,
//! immediate intervention, retain, skip" starting point.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{PlanError, Result};

// ---------------------------------------------------------------------------
// BackoffStrategy.
// ---------------------------------------------------------------------------

/// How long to wait between retry attempts.
///
/// `delay_for_attempt(attempt)` is 0-indexed in the classic sense: the
/// 1st retry gets `attempt = 0` of the backoff, etc.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackoffStrategy {
    /// No delay.
    Immediate,
    /// Constant delay.
    Fixed {
        /// The delay.
        #[serde(with = "duration_secs")]
        delay: Duration,
    },
    /// Linearly-growing delay: `base * attempt`.
    Linear {
        /// Per-step base delay.
        #[serde(with = "duration_secs")]
        base: Duration,
    },
    /// Exponentially-growing delay: `initial * factor^attempt`.
    Exponential {
        /// Growth factor.
        factor:  f64,
        /// First-attempt delay.
        #[serde(with = "duration_secs")]
        initial: Duration,
    },
}

impl BackoffStrategy {
    /// Compute the delay for the given attempt index (0 = first retry).
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        match self {
            Self::Immediate => Duration::ZERO,
            Self::Fixed { delay } => *delay,
            Self::Linear { base } => *base * attempt,
            Self::Exponential { factor, initial } => {
                let multiplier = factor.powi(i32::try_from(attempt).unwrap_or(i32::MAX));
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "deliberate exponential-backoff projection; deadline cap is logical"
                )]
                {
                    let nanos = initial.as_nanos() as f64 * multiplier;
                    let nanos_u64 = nanos.clamp(0.0, u64::MAX as f64) as u64;
                    Duration::from_nanos(nanos_u64)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RetryPolicy.
// ---------------------------------------------------------------------------

/// Retry behaviour for a unit of work (trial or element deployment).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct RetryPolicy {
    /// Total attempt budget (counting the initial attempt). Must be
    /// `>= 1`.
    pub max_attempts: u32,

    /// Delay schedule between attempts.
    pub backoff: BackoffStrategy,

    /// Error kinds (e.g. `"TimeoutError"`) that trigger retry. If
    /// empty, every error is retryable within the attempt budget.
    #[builder(default)]
    pub retryable_errors: BTreeSet<String>,
}

impl RetryPolicy {
    /// "No retry" — a single attempt, immediate backoff.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            max_attempts:     1,
            backoff:          BackoffStrategy::Immediate,
            retryable_errors: BTreeSet::new(),
        }
    }

    /// Validate the policy. Rejects `max_attempts == 0`.
    pub const fn validate(&self) -> Result<()> {
        if self.max_attempts == 0 {
            return Err(PlanError::ZeroMaxAttempts);
        }
        Ok(())
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::none()
    }
}

// ---------------------------------------------------------------------------
// InterventionMode / PartialRunBehavior.
// ---------------------------------------------------------------------------

/// Whether the executor honours pause/stop requests immediately or
/// after active trials finish.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionMode {
    /// Honour pause/stop requests immediately, even mid-trial.
    #[default]
    Immediate,
    /// Finish active trials before honouring pause/stop.
    AfterActiveTrials,
}

/// What happens to partial results when a run is interrupted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialRunBehavior {
    /// Keep partial results.
    #[default]
    RetainResults,
    /// Treat any interruption as a full-run failure.
    FailRun,
}

// ---------------------------------------------------------------------------
// OnFailure.
// ---------------------------------------------------------------------------

/// Plan-level response to a trial failure.
///
/// Drawn from the hyperplane study-system `on_failure` policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnFailure {
    /// Halt the study on the first failed trial.
    Stop,
    /// Mark the trial failed and continue. Default.
    #[default]
    Skip,
    /// Retry the trial up to `attempts` times before marking failed.
    Retry {
        /// Retry attempt budget.
        attempts: u32,
    },
}

// ---------------------------------------------------------------------------
// ExecutionPolicies.
// ---------------------------------------------------------------------------

/// Aggregate execution policies for a test plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ExecutionPolicies {
    /// Retry behaviour applied to each trial.
    #[builder(default)]
    pub trial_retry: RetryPolicy,

    /// Retry behaviour applied to element-deployment steps.
    #[builder(default)]
    pub element_deployment_retry: RetryPolicy,

    /// Per-trial wall-clock timeout.
    #[serde(default, with = "option_duration_secs")]
    pub trial_timeout: Option<Duration>,

    /// Per-element startup timeout (readiness).
    #[serde(default, with = "option_duration_secs")]
    pub element_start_timeout: Option<Duration>,

    /// How pause/stop requests are honoured.
    #[builder(default)]
    pub intervention_mode: InterventionMode,

    /// Partial-results retention policy on interruption.
    #[builder(default)]
    pub partial_run_behavior: PartialRunBehavior,

    /// What to do when a trial fails.
    #[builder(default)]
    pub on_failure: OnFailure,
}

impl ExecutionPolicies {
    /// Sensible defaults: no retries, no timeouts, immediate
    /// intervention, retain partial results, skip on failure.
    #[must_use]
    pub const fn defaults() -> Self {
        Self {
            trial_retry:              RetryPolicy::none(),
            element_deployment_retry: RetryPolicy::none(),
            trial_timeout:            None,
            element_start_timeout:    None,
            intervention_mode:        InterventionMode::Immediate,
            partial_run_behavior:     PartialRunBehavior::RetainResults,
            on_failure:               OnFailure::Skip,
        }
    }

    /// Validate internal consistency — e.g. retry budgets.
    pub fn validate(&self) -> Result<()> {
        self.trial_retry.validate()?;
        self.element_deployment_retry.validate()?;
        Ok(())
    }
}

impl Default for ExecutionPolicies {
    fn default() -> Self {
        Self::defaults()
    }
}

// ---------------------------------------------------------------------------
// Duration serialisation helpers — seconds as f64 for compact wire
// format, mirroring `paramodel-elements::HealthCheckSpec`.
// ---------------------------------------------------------------------------

mod duration_secs {
    use serde::{Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(d.as_secs_f64())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        use serde::Deserialize;
        let secs = f64::deserialize(d)?;
        if !secs.is_finite() || secs < 0.0 {
            return Err(serde::de::Error::custom(
                "duration seconds must be a finite non-negative f64",
            ));
        }
        Ok(Duration::from_secs_f64(secs))
    }
}

mod option_duration_secs {
    use serde::{Deserializer, Serializer};
    use std::time::Duration;

    #[allow(
        clippy::ref_option,
        reason = "Signature is fixed by `#[serde(with = ...)]`"
    )]
    pub fn serialize<S: Serializer>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(v) => s.serialize_some(&v.as_secs_f64()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Duration>, D::Error> {
        use serde::Deserialize;
        let opt = Option::<f64>::deserialize(d)?;
        Ok(match opt {
            None => None,
            Some(secs) => {
                if !secs.is_finite() || secs < 0.0 {
                    return Err(serde::de::Error::custom(
                        "duration seconds must be a finite non-negative f64",
                    ));
                }
                Some(Duration::from_secs_f64(secs))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy_none_has_single_attempt() {
        let p = RetryPolicy::none();
        assert_eq!(p.max_attempts, 1);
        assert!(p.retryable_errors.is_empty());
    }

    #[test]
    fn retry_policy_validate_rejects_zero() {
        let mut p = RetryPolicy::none();
        p.max_attempts = 0;
        assert!(matches!(p.validate(), Err(PlanError::ZeroMaxAttempts)));
    }

    #[test]
    fn backoff_immediate_is_zero() {
        assert_eq!(
            BackoffStrategy::Immediate.delay_for_attempt(0),
            Duration::ZERO
        );
        assert_eq!(
            BackoffStrategy::Immediate.delay_for_attempt(42),
            Duration::ZERO
        );
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
    fn backoff_linear_scales_with_attempt() {
        let s = BackoffStrategy::Linear {
            base: Duration::from_millis(100),
        };
        assert_eq!(s.delay_for_attempt(0), Duration::ZERO);
        assert_eq!(s.delay_for_attempt(3), Duration::from_millis(300));
    }

    #[test]
    fn backoff_exponential_grows() {
        let s = BackoffStrategy::Exponential {
            factor:  2.0,
            initial: Duration::from_millis(100),
        };
        assert_eq!(s.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(s.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(s.delay_for_attempt(2), Duration::from_millis(400));
    }

    #[test]
    fn execution_policies_defaults_match_spec() {
        let p = ExecutionPolicies::defaults();
        assert_eq!(p.trial_retry.max_attempts, 1);
        assert!(p.trial_timeout.is_none());
        assert_eq!(p.intervention_mode, InterventionMode::Immediate);
        assert_eq!(p.partial_run_behavior, PartialRunBehavior::RetainResults);
        assert!(matches!(p.on_failure, OnFailure::Skip));
    }

    #[test]
    fn execution_policies_validate_propagates_retry_error() {
        let mut p = ExecutionPolicies::defaults();
        p.trial_retry.max_attempts = 0;
        assert!(matches!(p.validate(), Err(PlanError::ZeroMaxAttempts)));
    }

    #[test]
    fn execution_policies_serde_roundtrip() {
        let p = ExecutionPolicies::defaults();
        let json = serde_json::to_string(&p).unwrap();
        let back: ExecutionPolicies = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn on_failure_retry_carries_attempts() {
        let f = OnFailure::Retry { attempts: 3 };
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains("\"kind\":\"retry\""));
        assert!(json.contains("\"attempts\":3"));
        let back: OnFailure = serde_json::from_str(&json).unwrap();
        assert_eq!(f, back);
    }
}
