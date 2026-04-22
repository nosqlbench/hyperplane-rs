// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Per-trait error types and the top-level `ExecutionError`.
//!
//! Each executor-side trait owns a narrow error enum. `ExecutionError`
//! is the top-level aggregate that `Executor::execute` returns; it
//! wraps the narrower errors plus a few executor-specific variants.

use paramodel_elements::Fingerprint;
use paramodel_plan::{BarrierId, StepId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Orchestrator errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::Orchestrator`].
#[derive(Debug, Error)]
pub enum OrchestratorError {
    /// The plan references an element type that no registered
    /// `ElementRuntime` can serve.
    #[error("no ElementRuntime registered for element '{element}'")]
    NoRuntimeForElement {
        /// Element name.
        element: String,
    },

    /// Element runtime raised an error that bubbled up unchanged.
    #[error(transparent)]
    Element(#[from] paramodel_elements::ElementError),

    /// The orchestrator was asked to act on an instance it doesn't
    /// know about.
    #[error("unknown instance '{instance_id}'")]
    UnknownInstance {
        /// The id the caller handed in.
        instance_id: String,
    },

    /// A step variant isn't handled by the v0.1 orchestrator.
    #[error("orchestrator does not yet handle step '{step}': {reason}")]
    UnsupportedStep {
        /// Step id.
        step:   String,
        /// Why it's unsupported.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Scheduler errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::Scheduler`].
#[derive(Debug, Error)]
pub enum SchedulerError {
    /// The scheduler was initialised against a graph it cannot
    /// process — typically a cycle slipped past plan validation.
    #[error("scheduler could not initialise: {reason}")]
    InitialisationFailed {
        /// Why initialisation failed.
        reason: String,
    },

    /// The scheduler was asked about an unknown step.
    #[error("unknown step '{step}' in scheduler state")]
    UnknownStep {
        /// The id the caller handed in.
        step: String,
    },
}

// ---------------------------------------------------------------------------
// Resource errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::ResourceManager`].
#[derive(Debug, Error)]
pub enum ResourceError {
    /// The request could not be satisfied at this time.
    #[error("insufficient resources: {reason}")]
    Insufficient {
        /// Why the request was rejected.
        reason: String,
    },

    /// The named pool doesn't exist.
    #[error("unknown resource pool '{pool}'")]
    UnknownPool {
        /// Pool name.
        pool: String,
    },

    /// A quota was exceeded.
    #[error("quota exceeded for '{user}': {reason}")]
    QuotaExceeded {
        /// Owning user.
        user:   String,
        /// Quota description.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Artifact errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::ArtifactCollector`].
#[derive(Debug, Error)]
pub enum ArtifactError {
    /// Collection has not started for this trial.
    #[error("no collection in progress for trial '{trial}'")]
    NoCollection {
        /// Trial id.
        trial: String,
    },

    /// Requested artifact doesn't exist.
    #[error("artifact not found: {name}")]
    NotFound {
        /// Artifact name.
        name: String,
    },

    /// Retention policy violation.
    #[error("retention policy violation: {reason}")]
    Retention {
        /// Description.
        reason: String,
    },

    /// Backend / storage error.
    #[error("backend error: {0}")]
    Backend(String),
}

// ---------------------------------------------------------------------------
// Journal errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::JournalWriter`].
#[derive(Debug, Error)]
pub enum JournalError {
    /// Backend write failed.
    #[error("journal write failed: {reason}")]
    WriteFailed {
        /// Reason.
        reason: String,
    },

    /// Backend read failed.
    #[error("journal read failed: {reason}")]
    ReadFailed {
        /// Reason.
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Barrier errors.
// ---------------------------------------------------------------------------

/// Errors produced by [`crate::BarrierHandle`].
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum BarrierError {
    /// The barrier was forced into `Failed` state.
    #[error("barrier '{barrier_id}' failed: {reason}")]
    Failed {
        /// Barrier id.
        barrier_id: BarrierId,
        /// Reason.
        reason:     String,
    },

    /// The barrier waited past its timeout.
    #[error("barrier '{barrier_id}' timed out")]
    TimedOut {
        /// Barrier id.
        barrier_id: BarrierId,
    },
}

// ---------------------------------------------------------------------------
// ExecutionError — top-level aggregate.
// ---------------------------------------------------------------------------

/// Top-level execution error returned by [`crate::Executor::execute`].
#[derive(Debug, Error)]
pub enum ExecutionError {
    /// Orchestrator failure.
    #[error(transparent)]
    Orchestrator(#[from] OrchestratorError),

    /// Scheduler failure.
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),

    /// Resource failure.
    #[error(transparent)]
    Resource(#[from] ResourceError),

    /// Artifact failure.
    #[error(transparent)]
    Artifact(#[from] ArtifactError),

    /// Journal failure.
    #[error(transparent)]
    Journal(#[from] JournalError),

    /// Barrier failure.
    #[error(transparent)]
    Barrier(#[from] BarrierError),

    /// `resume()` given a checkpoint whose plan fingerprint doesn't
    /// match the plan argument — per SRD-0011 D9.
    #[error(
        "plan fingerprint mismatch: checkpoint built from {checkpoint} but plan is {plan}"
    )]
    PlanMismatch {
        /// Checkpoint's fingerprint.
        checkpoint: Fingerprint,
        /// Plan's fingerprint.
        plan:       Fingerprint,
    },

    /// A step failed terminally and the plan's `OnFailure` says
    /// `Stop`.
    #[error("step '{step}' failed and plan policy halts on failure")]
    StopOnFailure {
        /// Step id.
        step: StepId,
    },

    /// The execution was cancelled via `stop()`.
    #[error("execution cancelled: {reason}")]
    Cancelled {
        /// Cancellation reason.
        reason: String,
    },
}

/// Pre-flight diagnostic from [`crate::Executor::dry_run`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDiagnostic {
    /// Machine-readable code.
    pub code:    String,
    /// Human-readable message.
    pub message: String,
}
