// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionObserver` + `InterventionKind`.
//!
//! Observers are synchronous side-effectful callbacks invoked from
//! within the executor. Every method has a default empty body so
//! adopters override only what they care about.

use std::time::Duration;

use paramodel_elements::Trial;
use paramodel_plan::{AtomicStep, BarrierId};
use paramodel_trials::{ErrorInfo, TrialResult};
use serde::{Deserialize, Serialize};

use crate::ids::ExecutionId;
use crate::state::CheckpointHandle;

/// Discriminates the three intervention verbs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterventionKind {
    /// Pause.
    Pause {
        /// Reason string.
        reason: String,
    },
    /// Resume after pause.
    Resume,
    /// Stop (graceful or abrupt).
    Stop {
        /// Reason string.
        reason:   String,
        /// Run teardown steps before terminating?
        graceful: bool,
    },
}

/// Passive observer of executor events.
pub trait ExecutionObserver: Send + Sync + 'static {
    /// Execution has just begun.
    fn on_execution_started(
        &self,
        _execution: &ExecutionId,
        _plan:      &paramodel_plan::ExecutionPlan,
    ) {
    }

    /// Execution completed normally.
    fn on_execution_completed(
        &self,
        _execution: &ExecutionId,
        _results:   &crate::ExecutionResults,
    ) {
    }

    /// Execution failed.
    fn on_execution_failed(
        &self,
        _execution: &ExecutionId,
        _error:     &crate::error::ExecutionError,
    ) {
    }

    /// A step is about to begin.
    fn on_step_started(
        &self,
        _execution: &ExecutionId,
        _step:      &AtomicStep,
        _attempt:   u32,
    ) {
    }

    /// A step completed successfully.
    fn on_step_completed(
        &self,
        _execution: &ExecutionId,
        _step:      &AtomicStep,
        _attempt:   u32,
        _elapsed:   Duration,
    ) {
    }

    /// A step failed.
    fn on_step_failed(
        &self,
        _execution: &ExecutionId,
        _step:      &AtomicStep,
        _attempt:   u32,
        _error:     &ErrorInfo,
    ) {
    }

    /// A step was skipped.
    fn on_step_skipped(
        &self,
        _execution: &ExecutionId,
        _step:      &AtomicStep,
        _reason:    &str,
    ) {
    }

    /// A barrier was satisfied.
    fn on_barrier_reached(&self, _execution: &ExecutionId, _barrier_id: &BarrierId) {}

    /// A barrier timed out.
    fn on_barrier_timed_out(&self, _execution: &ExecutionId, _barrier_id: &BarrierId) {}

    /// A checkpoint was written.
    fn on_checkpoint_created(
        &self,
        _execution:  &ExecutionId,
        _checkpoint: &CheckpointHandle,
    ) {
    }

    /// A trial started.
    fn on_trial_started(&self, _execution: &ExecutionId, _trial: &Trial) {}

    /// A trial completed.
    fn on_trial_completed(
        &self,
        _execution: &ExecutionId,
        _trial:     &Trial,
        _result:    &TrialResult,
    ) {
    }

    /// An intervention (pause/resume/stop) was received.
    fn on_intervention(&self, _execution: &ExecutionId, _kind: &InterventionKind) {}
}

/// Do-nothing observer — the natural baseline.
#[derive(Debug, Default)]
pub struct NoopObserver;

impl ExecutionObserver for NoopObserver {}
