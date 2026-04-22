// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Lifecycle metadata: health-check timing, operational state, and
//! shutdown semantics.
//!
//! Per SRD-0007 D9–D11: `HealthCheckSpec` controls readiness polling;
//! `OperationalState` is the 11-variant runtime lifecycle enum;
//! `ShutdownSemantics` decides whether reducto emits a `Teardown` or
//! an `Await` step.

use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// HealthCheckSpec.
// ---------------------------------------------------------------------------

/// Readiness-polling timing.
///
/// The host system owns the health-check mechanism (protocol, endpoint,
/// acceptance); paramodel only needs the timing parameters so the
/// runtime can coordinate.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HealthCheckSpec {
    /// Maximum total duration the runtime waits for readiness.
    #[serde(with = "duration_secs")]
    pub timeout:        Duration,
    /// Number of retry attempts before giving up.
    pub max_retries:    u32,
    /// Delay between retry attempts.
    #[serde(with = "duration_secs")]
    pub retry_interval: Duration,
}

impl HealthCheckSpec {
    /// Construct a health-check spec.
    #[must_use]
    pub const fn new(timeout: Duration, max_retries: u32, retry_interval: Duration) -> Self {
        Self {
            timeout,
            max_retries,
            retry_interval,
        }
    }
}

// Manual Duration serialisation as floating-point seconds keeps the
// wire format compact and portable across languages.
mod duration_secs {
    use serde::{Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(
        d: &Duration,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_f64(d.as_secs_f64())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Duration, D::Error> {
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

// ---------------------------------------------------------------------------
// OperationalState.
// ---------------------------------------------------------------------------

/// Element runtime lifecycle state.
///
/// Normal progression: `Inactive → Provisioning → Starting →
/// HealthCheck → Ready → Running → Stopping → Stopped → Terminated`.
/// `Failed` and `Unknown` are non-sequential and reachable from any
/// state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalState {
    /// Not yet started.
    Inactive,
    /// Infrastructure is being allocated.
    Provisioning,
    /// Process is starting up.
    Starting,
    /// Verifying readiness.
    HealthCheck,
    /// Available for use.
    Ready,
    /// Actively serving a trial.
    Running,
    /// Graceful shutdown in progress.
    Stopping,
    /// Stopped normally; resources still allocated.
    Stopped,
    /// Error state; cannot operate.
    Failed,
    /// Fully torn down; all resources released.
    Terminated,
    /// Status cannot be determined.
    Unknown,
}

// ---------------------------------------------------------------------------
// LiveStatusSummary.
// ---------------------------------------------------------------------------

/// One-shot state snapshot returned by
/// [`ElementRuntime::status_check`](crate::ElementRuntime::status_check).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LiveStatusSummary {
    /// The observed state.
    pub state:   OperationalState,
    /// One-line human-readable evidence / explanation.
    pub summary: String,
}

impl LiveStatusSummary {
    /// Construct an `Unknown` summary with the given explanation.
    #[must_use]
    pub fn unknown(summary: impl Into<String>) -> Self {
        Self {
            state:   OperationalState::Unknown,
            summary: summary.into(),
        }
    }

    /// Construct an `Inactive` summary with a default message.
    #[must_use]
    pub fn inactive() -> Self {
        Self {
            state:   OperationalState::Inactive,
            summary: "element not yet started".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// ShutdownSemantics.
// ---------------------------------------------------------------------------

/// Whether an element terminates by explicit signal or by self-completion.
///
/// Reducto Rule 1 uses this to pick between `Teardown` and `Await`
/// steps. Default is `Service`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownSemantics {
    /// Long-running; requires an explicit stop signal.
    #[default]
    Service,
    /// Self-terminating; awaited rather than stopped.
    Command,
}

// ---------------------------------------------------------------------------
// StateTransition.
// ---------------------------------------------------------------------------

/// One observed transition between two [`OperationalState`]s.
///
/// Emitted by [`ElementRuntime::observe_state`](crate::ElementRuntime::observe_state)
/// listeners. Implementations must deliver a synthetic initial
/// transition from `Unknown` to the current state so registration acts
/// as catch-up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateTransition {
    /// Previous state.
    pub from:      OperationalState,
    /// New state.
    pub to:        OperationalState,
    /// One-line evidence for the transition.
    pub summary:   String,
    /// Observation timestamp.
    pub timestamp: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_default_is_service() {
        assert_eq!(ShutdownSemantics::default(), ShutdownSemantics::Service);
    }

    #[test]
    fn live_status_helpers() {
        let u = LiveStatusSummary::unknown("probe failed");
        assert_eq!(u.state, OperationalState::Unknown);
        assert_eq!(u.summary, "probe failed");
        let i = LiveStatusSummary::inactive();
        assert_eq!(i.state, OperationalState::Inactive);
    }

    #[test]
    fn operational_state_serde_is_snake_case() {
        let s = serde_json::to_string(&OperationalState::HealthCheck).unwrap();
        assert_eq!(s, "\"health_check\"");
        let back: OperationalState = serde_json::from_str(&s).unwrap();
        assert_eq!(back, OperationalState::HealthCheck);
    }

    #[test]
    fn health_check_spec_serde_roundtrip() {
        let spec = HealthCheckSpec::new(
            Duration::from_secs(30),
            5,
            Duration::from_millis(500),
        );
        let json = serde_json::to_string(&spec).unwrap();
        let back: HealthCheckSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn state_transition_serde_roundtrip() {
        let t = StateTransition {
            from:      OperationalState::Unknown,
            to:        OperationalState::Ready,
            summary:   "probe passed".to_owned(),
            timestamp: Timestamp::from_second(0).unwrap(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: StateTransition = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
