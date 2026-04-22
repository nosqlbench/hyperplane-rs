// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Append-only journal of executor-observable transitions.
//!
//! V0.1 ships `InMemoryJournalWriter` for tests. `JournalStore`
//! (persistence) is defined in SRD-0012 and can wrap arbitrary
//! backends.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{Fingerprint, OperationalState, TrialId};
use paramodel_plan::{BarrierId, CheckpointId, InstanceId, StepId};
use paramodel_trials::{ErrorInfo, TrialStatus};
use serde::{Deserialize, Serialize};

use crate::error::JournalError;
use crate::ids::{ExecutionId, JournalSequence};

/// One journal event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEvent {
    /// Monotonic sequence within the execution.
    pub sequence:     JournalSequence,
    /// Execution id.
    pub execution_id: ExecutionId,
    /// Wall-clock timestamp.
    pub timestamp:    Timestamp,
    /// Kind-specific payload.
    pub kind:         JournalEventKind,
}

/// Event payload discriminator + fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEventKind {
    /// Execution began.
    ExecutionStarted { plan_fingerprint: Fingerprint },
    /// Execution resumed from a checkpoint.
    ExecutionResumed { checkpoint_id: CheckpointId },
    /// Execution paused.
    ExecutionPaused { reason: String },
    /// Execution stopped.
    ExecutionStopped { reason: String, graceful: bool },
    /// Execution ended.
    ExecutionCompleted { success: bool, duration: Duration },

    /// A step began.
    StepStarted { step_id: StepId, attempt: u32 },
    /// A step completed.
    StepCompleted {
        step_id:  StepId,
        attempt:  u32,
        duration: Duration,
    },
    /// A step failed.
    StepFailed {
        step_id: StepId,
        attempt: u32,
        error:   ErrorInfo,
    },
    /// A step was skipped.
    StepSkipped { step_id: StepId, reason: String },

    /// Instance operational-state transition.
    InstanceStateChange {
        instance_id: InstanceId,
        from:        OperationalState,
        to:          OperationalState,
    },

    /// A barrier satisfied.
    BarrierReached { barrier_id: BarrierId },
    /// A barrier timed out.
    BarrierTimedOut {
        barrier_id: BarrierId,
        waited:     Duration,
    },

    /// A checkpoint was written.
    CheckpointCreated {
        checkpoint_id:        CheckpointId,
        completed_step_count: u32,
    },

    /// A trial started.
    TrialStarted {
        trial_id:    TrialId,
        trial_index: u32,
    },
    /// A trial finished.
    TrialCompleted {
        trial_id: TrialId,
        status:   TrialStatus,
    },
}

/// Append-only journal writer.
#[async_trait]
pub trait JournalWriter: Send + Sync + 'static {
    /// Append an event.
    async fn write(&self, event: JournalEvent) -> Result<(), JournalError>;
    /// Flush any buffered writes to the backing store.
    async fn flush(&self) -> Result<(), JournalError>;
    /// Read events after the given sequence (exclusive). `None` =
    /// from the start.
    async fn since(
        &self,
        after: Option<JournalSequence>,
    ) -> Result<Vec<JournalEvent>, JournalError>;
    /// Look up the most recent event for one execution.
    async fn last_event(&self, execution: &ExecutionId) -> Option<JournalEvent>;
}

/// Simple in-memory journal for tests.
#[derive(Debug, Default)]
pub struct InMemoryJournalWriter {
    events: Mutex<Vec<JournalEvent>>,
}

impl InMemoryJournalWriter {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Snapshot the current event log. Cheap; used by tests.
    pub fn snapshot(&self) -> Vec<JournalEvent> {
        self.events.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl JournalWriter for InMemoryJournalWriter {
    async fn write(&self, event: JournalEvent) -> Result<(), JournalError> {
        self.events.lock().expect("poisoned").push(event);
        Ok(())
    }

    async fn flush(&self) -> Result<(), JournalError> {
        Ok(())
    }

    async fn since(
        &self,
        after: Option<JournalSequence>,
    ) -> Result<Vec<JournalEvent>, JournalError> {
        let events = self.events.lock().expect("poisoned");
        let gate = after.map_or(0, |s| s.get());
        Ok(events
            .iter()
            .filter(|e| e.sequence.get() > gate)
            .cloned()
            .collect())
    }

    async fn last_event(&self, execution: &ExecutionId) -> Option<JournalEvent> {
        self.events
            .lock()
            .expect("poisoned")
            .iter()
            .rev()
            .find(|e| &e.execution_id == execution)
            .cloned()
    }
}
