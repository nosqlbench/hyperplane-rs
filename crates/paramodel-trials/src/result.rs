// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `TrialResult` and its supporting types: `Metrics`, `ArtifactRef`,
//! `ExecutionTiming`, `TrialProvenance`, `ErrorInfo`.
//!
//! Per SRD-0006 D9 the invariants on a `TrialResult` are:
//!
//! - `attempt >= 1`.
//! - `status == Failed` ⇒ `error.is_some()`.
//! - `status == Skipped` ⇒ `skip_reason.is_some()`.
//! - `status != Failed` ⇒ `error.is_none()`.
//! - `status != Skipped` ⇒ `skip_reason.is_none()`.
//! - `timing.completed_at >= timing.started_at`.
//!
//! The builder assembles fields; callers run [`TrialResult::validate`]
//! to enforce these invariants at whatever boundary they care about.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{ElementName, Labels, ParameterName, Tags, TrialId, Value};
use serde::{Deserialize, Serialize};

use crate::error::{Result, TrialError};
use crate::status::TrialStatus;
use crate::trial_set::TrialSetId;

// ---------------------------------------------------------------------------
// Metrics.
// ---------------------------------------------------------------------------

/// Typed trial metrics.
///
/// Same two-level shape as `Assignments`, but populated from
/// elements' `@result`-declared output parameters rather than input
/// parameters. A metric is "the value of an element's result
/// parameter for this trial," not a free-form key/value pair.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Metrics(BTreeMap<ElementName, BTreeMap<ParameterName, Value>>);

impl Metrics {
    /// Empty metrics.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct from a pre-built map.
    #[must_use]
    pub const fn from_map(
        map: BTreeMap<ElementName, BTreeMap<ParameterName, Value>>,
    ) -> Self {
        Self(map)
    }

    /// Look up a single metric value.
    #[must_use]
    pub fn get(&self, element: &ElementName, param: &ParameterName) -> Option<&Value> {
        self.0.get(element).and_then(|p| p.get(param))
    }

    /// Sorted iterator over `(element, param, value)` triples.
    pub fn iter(&self) -> impl Iterator<Item = (&ElementName, &ParameterName, &Value)> {
        self.0
            .iter()
            .flat_map(|(e, params)| params.iter().map(move |(p, v)| (e, p, v)))
    }

    /// Number of metrics recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.values().map(BTreeMap::len).sum()
    }

    /// `true` when no metrics are recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// ArtifactRef.
// ---------------------------------------------------------------------------

/// Reference to a trial artifact stored externally (logs, traces,
/// captured outputs, …).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, bon::Builder)]
pub struct ArtifactRef {
    /// Element that produced the artifact.
    pub element: ElementName,
    /// Author-supplied name within the element.
    pub name: String,
    /// Opaque uri — resolved by the `ArtifactStore` (persistence SRD).
    pub uri: String,
    /// MIME-shaped content type.
    pub content_type: String,
    /// Payload size in bytes. `None` = unknown (upstream's `-1`
    /// convention).
    pub size_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// ExecutionTiming.
// ---------------------------------------------------------------------------

/// Start/end timing for a trial run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionTiming {
    /// Wall-clock time the trial started.
    pub started_at: Timestamp,
    /// Wall-clock time the trial finished.
    pub completed_at: Timestamp,
}

impl ExecutionTiming {
    /// Construct a timing record. Rejects `completed_at < started_at`.
    pub fn new(started_at: Timestamp, completed_at: Timestamp) -> Result<Self> {
        if completed_at < started_at {
            return Err(TrialError::NegativeDuration);
        }
        Ok(Self {
            started_at,
            completed_at,
        })
    }

    /// Elapsed wall-clock duration between `started_at` and
    /// `completed_at`. The construction-time check guarantees the span
    /// is non-negative, so the conversion to `std::time::Duration`
    /// cannot fail in well-formed values.
    #[must_use]
    pub fn duration(&self) -> Duration {
        let span = self.completed_at.duration_since(self.started_at);
        Duration::try_from(span).unwrap_or(Duration::ZERO)
    }
}

// ---------------------------------------------------------------------------
// TrialProvenance.
// ---------------------------------------------------------------------------

/// Minimal provenance record for a completed trial.
///
/// Per SRD-0006 D13: no fingerprints here. Verification / tamper
/// detection at trust boundaries uses `Trial::canonical_bytes()`
/// computed on demand by the persistence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct TrialProvenance {
    /// Owning trial-set, when applicable.
    pub trial_set_id: Option<TrialSetId>,

    /// Host/runtime environment snapshot.
    #[builder(default)]
    pub environment: BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// ErrorInfo.
// ---------------------------------------------------------------------------

/// Structured error record attached to a failed trial result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct ErrorInfo {
    /// Error class / kind (e.g. `"TimeoutError"`).
    pub kind: String,
    /// Human-readable message.
    pub message: String,
    /// Optional stack trace / diagnostic payload.
    pub stack_trace: Option<String>,
    /// Hint that retrying may succeed.
    #[builder(default)]
    pub is_retryable: bool,
    /// Optional stable error code.
    pub error_code: Option<String>,
}

// ---------------------------------------------------------------------------
// TrialResult.
// ---------------------------------------------------------------------------

/// Outcome record for one trial run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct TrialResult {
    /// The trial this result is for.
    pub trial_id: TrialId,

    /// Terminal status of the run.
    pub status: TrialStatus,

    /// Attempt number, starting at 1.
    pub attempt: u32,

    /// Typed metrics keyed by `(element, result_parameter)`.
    #[builder(default)]
    pub metrics: Metrics,

    /// Artifact references produced by the run.
    #[builder(default)]
    pub artifacts: Vec<ArtifactRef>,

    /// Start/end timing.
    pub timing: ExecutionTiming,

    /// Minimal provenance record.
    #[builder(default)]
    pub provenance: TrialProvenance,

    /// Present iff `status == Failed`.
    pub error: Option<ErrorInfo>,

    /// Present iff `status == Skipped`.
    pub skip_reason: Option<String>,

    /// Intrinsic facts attached to the result.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,
}

impl TrialResult {
    /// Check the status-conditional invariants pinned in SRD-0006 D9.
    ///
    /// Callers run this at whatever boundary they care about
    /// (persistence write, API return). The builder assembles the
    /// fields without fallibility; validation is a separate step so
    /// users pay only when they need the guarantee.
    pub fn validate(&self) -> Result<()> {
        if self.attempt == 0 {
            return Err(TrialError::ZeroAttempt);
        }
        match self.status {
            TrialStatus::Failed if self.error.is_none() => {
                return Err(TrialError::MissingErrorForFailure);
            }
            TrialStatus::Skipped if self.skip_reason.is_none() => {
                return Err(TrialError::MissingSkipReason);
            }
            _ => {}
        }
        if self.status != TrialStatus::Failed && self.error.is_some() {
            return Err(TrialError::ErrorWithoutFailure);
        }
        if self.status != TrialStatus::Skipped && self.skip_reason.is_some() {
            return Err(TrialError::SkipReasonWithoutSkip);
        }
        if self.timing.completed_at < self.timing.started_at {
            return Err(TrialError::NegativeDuration);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ulid::Ulid;

    use super::*;

    fn tid(n: u64) -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(n, 1))
    }
    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_second(seconds).unwrap()
    }
    fn timing_ok() -> ExecutionTiming {
        ExecutionTiming::new(ts(1_000), ts(2_000)).unwrap()
    }

    // ---------- ExecutionTiming ----------

    #[test]
    fn timing_rejects_negative_duration() {
        let err = ExecutionTiming::new(ts(2_000), ts(1_000)).unwrap_err();
        assert_eq!(err, TrialError::NegativeDuration);
    }

    #[test]
    fn timing_duration_is_positive() {
        let t = timing_ok();
        assert_eq!(t.duration(), Duration::from_secs(1_000));
    }

    // ---------- Metrics / ArtifactRef ----------

    #[test]
    fn metrics_from_map_and_query() {
        let e = ElementName::new("db").unwrap();
        let p = ParameterName::new("qps").unwrap();
        let mut inner = BTreeMap::new();
        inner.insert(p.clone(), Value::double(p.clone(), 1234.5, None));
        let mut outer = BTreeMap::new();
        outer.insert(e.clone(), inner);
        let m = Metrics::from_map(outer);
        assert_eq!(m.get(&e, &p).and_then(Value::as_double), Some(1234.5));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn artifact_ref_builder_defaults_size_to_none() {
        let a = ArtifactRef::builder()
            .element(ElementName::new("db").unwrap())
            .name("log".to_owned())
            .uri("s3://bucket/log.txt".to_owned())
            .content_type("text/plain".to_owned())
            .build();
        assert!(a.size_bytes.is_none());
    }

    // ---------- TrialResult validation ----------

    #[test]
    fn result_validate_accepts_completed() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Completed)
            .attempt(1)
            .timing(timing_ok())
            .build();
        assert!(r.validate().is_ok());
    }

    #[test]
    fn result_validate_rejects_zero_attempt() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Completed)
            .attempt(0)
            .timing(timing_ok())
            .build();
        assert_eq!(r.validate().unwrap_err(), TrialError::ZeroAttempt);
    }

    #[test]
    fn result_validate_requires_error_for_failed() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Failed)
            .attempt(1)
            .timing(timing_ok())
            .build();
        assert_eq!(
            r.validate().unwrap_err(),
            TrialError::MissingErrorForFailure
        );
    }

    #[test]
    fn result_validate_accepts_failed_with_error() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Failed)
            .attempt(2)
            .timing(timing_ok())
            .error(
                ErrorInfo::builder()
                    .kind("TimeoutError".to_owned())
                    .message("deadline exceeded".to_owned())
                    .is_retryable(true)
                    .build(),
            )
            .build();
        assert!(r.validate().is_ok());
    }

    #[test]
    fn result_validate_rejects_error_without_failure() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Completed)
            .attempt(1)
            .timing(timing_ok())
            .error(
                ErrorInfo::builder()
                    .kind("x".to_owned())
                    .message("y".to_owned())
                    .build(),
            )
            .build();
        assert_eq!(r.validate().unwrap_err(), TrialError::ErrorWithoutFailure);
    }

    #[test]
    fn result_validate_requires_skip_reason_when_skipped() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Skipped)
            .attempt(1)
            .timing(timing_ok())
            .build();
        assert_eq!(r.validate().unwrap_err(), TrialError::MissingSkipReason);
    }

    #[test]
    fn result_validate_rejects_skip_reason_without_skip() {
        let r = TrialResult::builder()
            .trial_id(tid(1))
            .status(TrialStatus::Completed)
            .attempt(1)
            .timing(timing_ok())
            .skip_reason("unused".to_owned())
            .build();
        assert_eq!(
            r.validate().unwrap_err(),
            TrialError::SkipReasonWithoutSkip
        );
    }

    // ---------- Serde ----------

    #[test]
    fn trial_result_serde_roundtrip() {
        let r = TrialResult::builder()
            .trial_id(tid(42))
            .status(TrialStatus::Completed)
            .attempt(1)
            .timing(timing_ok())
            .build();
        let json = serde_json::to_string(&r).unwrap();
        let back: TrialResult = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
