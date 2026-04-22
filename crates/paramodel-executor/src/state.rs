// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionStateManager`, `Checkpoint`, `CheckpointState`, and
//! `RecoveryResult`.
//!
//! V0.1 ships `NoopStateManager` — the reference "nothing is
//! persisted" impl the SRD calls for.

use std::collections::{BTreeMap, BTreeSet};

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{Fingerprint, OperationalState, TrialId};
use paramodel_plan::{AtomicStep, CheckpointId, ExecutionPlan, InstanceId, StepId};
use paramodel_trials::TrialResult;
use serde::{Deserialize, Serialize};

use crate::ids::{ExecutionId, JournalSequence};
use crate::journal::JournalEvent;
use crate::resources::ResourceUsage;
use crate::scheduler::SchedulerState;

// ---------------------------------------------------------------------------
// Checkpoint + state snapshot.
// ---------------------------------------------------------------------------

/// Persistent checkpoint record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint id.
    pub id:                CheckpointId,
    /// Execution this checkpoint belongs to.
    pub execution_id:      ExecutionId,
    /// Fingerprint of the plan the checkpoint was cut from.
    pub plan_fingerprint:  Fingerprint,
    /// When the snapshot was taken.
    pub created_at:        Timestamp,
    /// The snapshot itself.
    pub state:             CheckpointState,
    /// Fingerprint over `state` for tamper detection.
    pub fingerprint:       Fingerprint,
}

/// In-memory snapshot of the executor's state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckpointState {
    /// Terminal-success step ids.
    pub completed_step_ids:  BTreeSet<StepId>,
    /// Terminal-failure step ids.
    pub failed_step_ids:     BTreeSet<StepId>,
    /// Terminal-skipped step ids.
    pub skipped_step_ids:    BTreeSet<StepId>,
    /// Steps that were running when the snapshot was taken.
    pub in_flight_step_ids:  BTreeSet<StepId>,
    /// Completed trial results.
    pub completed_trials:    BTreeMap<TrialId, TrialResult>,
    /// Trials that were running when the snapshot was taken.
    pub in_flight_trials:    BTreeSet<TrialId>,
    /// Last-known instance states.
    pub instance_states:     BTreeMap<InstanceId, OperationalState>,
    /// Scheduler state.
    pub scheduler_state:     SchedulerState,
    /// Resource usage at snapshot time.
    pub resource_usage:      ResourceUsage,
    /// Highest journal sequence reflected in this snapshot.
    pub journal_sequence:    JournalSequence,
}

/// Lightweight descriptor used by [`crate::Executor::checkpoints`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointHandle {
    /// Checkpoint id.
    pub id:         CheckpointId,
    /// When it was created.
    pub created_at: Timestamp,
    /// Whether the checkpoint's plan fingerprint matches the current
    /// plan — if not, resume will reject it.
    pub is_valid:   bool,
}

// ---------------------------------------------------------------------------
// Recovery types.
// ---------------------------------------------------------------------------

/// What `ExecutionStateManager::recover` reports to the executor.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryResult {
    /// Completed step ids.
    pub completed_step_ids:    BTreeSet<StepId>,
    /// Failed step ids.
    pub failed_step_ids:       BTreeSet<StepId>,
    /// Skipped step ids.
    pub skipped_step_ids:      BTreeSet<StepId>,
    /// Steps that were running at the last recorded event.
    pub in_flight_step_ids:    BTreeSet<StepId>,
    /// Completed trial ids.
    pub completed_trial_ids:   BTreeSet<TrialId>,
    /// In-flight trial ids.
    pub in_flight_trial_ids:   BTreeSet<TrialId>,
    /// Per-instance last-known state.
    pub element_states:        BTreeMap<InstanceId, OperationalState>,
    /// Whether the previous run ended cleanly (no in-flight steps).
    pub was_clean_shutdown:    bool,
    /// Per-step resolution guidance.
    pub in_flight_resolutions: BTreeMap<StepId, InFlightResolution>,
}

/// Decision the recovery-reader emits for each in-flight step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InFlightResolution {
    /// Safe to re-run — step is idempotent.
    Retry,
    /// Check side-effects first.
    Inspect {
        /// Implementation-defined query string.
        state_query: String,
    },
    /// Mark the step failed without re-running.
    MarkFailed,
}

/// Whether a step kind is safe to replay on recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyClass {
    /// Safe to replay.
    Idempotent,
    /// Inspect state before replay.
    NonIdempotent,
}

// ---------------------------------------------------------------------------
// ExecutionStateManager trait.
// ---------------------------------------------------------------------------

/// Recovery-oriented state tracking.
#[async_trait]
pub trait ExecutionStateManager: Send + Sync + 'static {
    /// Record one journal event for later replay.
    async fn record_event(&self, event: JournalEvent);

    /// Take a checkpoint snapshot.
    async fn checkpoint(&self, checkpoint: &Checkpoint);

    /// Reconstruct per-execution state for a resume.
    async fn recover(&self, execution: &ExecutionId, plan: &ExecutionPlan) -> RecoveryResult;

    /// Was the given step already completed?
    async fn is_step_completed(&self, execution: &ExecutionId, step: &StepId) -> bool;

    /// Record a pause / stop request so recovery can distinguish
    /// planned suspension from a crash.
    async fn record_suspension(&self, execution: &ExecutionId, reason: &str);

    /// Attach a trial result.
    async fn save_trial_result(&self, execution: &ExecutionId, result: &TrialResult);

    /// Retrieve one trial result.
    async fn get_trial_result(&self, trial: &TrialId) -> Option<TrialResult>;

    /// All trial results for an execution.
    async fn get_trial_results(&self, execution: &ExecutionId) -> Vec<TrialResult>;

    /// Drop everything stored for an execution.
    async fn cleanup(&self, execution: &ExecutionId);

    /// Classify a step's replay safety. Pure function of the variant.
    fn idempotency_class(&self, step: &AtomicStep) -> IdempotencyClass {
        match step {
            AtomicStep::Deploy { .. }
            | AtomicStep::Await { .. }
            | AtomicStep::SaveOutput { .. } => IdempotencyClass::Idempotent,
            AtomicStep::Teardown { .. }
            | AtomicStep::TrialStart { .. }
            | AtomicStep::TrialEnd { .. }
            | AtomicStep::Barrier { .. }
            | AtomicStep::Checkpoint { .. } => IdempotencyClass::NonIdempotent,
        }
    }
}

// ---------------------------------------------------------------------------
// NoopStateManager — the "I don't persist" reference impl.
// ---------------------------------------------------------------------------

/// No-op state manager. Plans that don't need resume use this.
#[derive(Debug, Default)]
pub struct NoopStateManager;

impl NoopStateManager {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ExecutionStateManager for NoopStateManager {
    async fn record_event(&self, _event: JournalEvent) {}
    async fn checkpoint(&self, _checkpoint: &Checkpoint) {}
    async fn recover(
        &self,
        _execution: &ExecutionId,
        _plan:      &ExecutionPlan,
    ) -> RecoveryResult {
        RecoveryResult::default()
    }
    async fn is_step_completed(&self, _execution: &ExecutionId, _step: &StepId) -> bool {
        false
    }
    async fn record_suspension(&self, _execution: &ExecutionId, _reason: &str) {}
    async fn save_trial_result(&self, _execution: &ExecutionId, _result: &TrialResult) {}
    async fn get_trial_result(&self, _trial: &TrialId) -> Option<TrialResult> {
        None
    }
    async fn get_trial_results(&self, _execution: &ExecutionId) -> Vec<TrialResult> {
        Vec::new()
    }
    async fn cleanup(&self, _execution: &ExecutionId) {}
}
