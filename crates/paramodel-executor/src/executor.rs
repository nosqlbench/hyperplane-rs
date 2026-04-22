// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Executor` trait + `DefaultExecutor`.
//!
//! V0.1 ships a synchronous reference executor: one step at a time,
//! in topological order, no retries, no checkpoint emission, no
//! resumption logic. Parallel execution, retries, and resume arrive
//! in follow-up slices — the trait surface anticipates them.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{Fingerprint, OperationalState, TrialId};
use paramodel_plan::{CheckpointId, ExecutionPlan, InstanceId, OnFailure, StepId};
use paramodel_trials::{ErrorInfo, TrialResult};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{ExecutionDiagnostic, ExecutionError};
use crate::ids::{ExecutionId, JournalSequence};
use crate::journal::{JournalEvent, JournalEventKind, JournalWriter};
use crate::observer::{ExecutionObserver, InterventionKind};
use crate::orchestrator::{Orchestrator, StepOutcome};
use crate::resources::{ResourceSnapshot, ResourceUsage};
use crate::scheduler::Scheduler;
use crate::state::{
    Checkpoint, CheckpointHandle, CheckpointState, ExecutionStateManager,
};

// ---------------------------------------------------------------------------
// Per-step stats + results aggregate.
// ---------------------------------------------------------------------------

/// Per-step timing + outcome counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepStatistics {
    /// Times the step was started.
    pub attempts:  u32,
    /// Times the step finished cleanly.
    pub completed: u32,
    /// Times the step terminated in failure.
    pub failed:    u32,
    /// Times the step was skipped.
    pub skipped:   u32,
}

/// Terminal status of one execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// Every step reached a terminal state with no failures.
    Succeeded,
    /// At least one step failed (and the plan's policy didn't halt).
    PartialFailure,
    /// Plan's `OnFailure::Stop` policy halted the run.
    Halted,
    /// Cancelled via `stop()`.
    Cancelled,
}

/// Aggregate returned by [`Executor::execute`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionResults {
    /// Identifier for this run.
    pub execution_id:   ExecutionId,
    /// Terminal status.
    pub status:         ExecutionStatus,
    /// When the run started.
    pub started_at:     Timestamp,
    /// When the run finished.
    pub completed_at:   Timestamp,
    /// Wall-clock elapsed.
    pub duration:       Duration,
    /// Per-step stats, keyed by step id.
    pub step_stats:     BTreeMap<StepId, StepStatistics>,
    /// Trial results captured during the run.
    pub trial_results:  Vec<TrialResult>,
}

// ---------------------------------------------------------------------------
// Executor trait.
// ---------------------------------------------------------------------------

/// Top-level execution engine.
#[async_trait]
pub trait Executor: Send + Sync + 'static {
    /// Run `plan` to completion.
    async fn execute(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<ExecutionResults, ExecutionError>;

    /// Resume a previous execution from a checkpoint.
    async fn resume(
        &self,
        plan:       &ExecutionPlan,
        checkpoint: &CheckpointId,
    ) -> Result<ExecutionResults, ExecutionError>;

    /// Pause the in-flight execution.
    async fn pause(&self, reason: &str) -> Result<(), ExecutionError>;

    /// Resume a paused execution.
    async fn unpause(&self) -> Result<(), ExecutionError>;

    /// Stop the in-flight execution.
    async fn stop(&self, reason: &str, graceful: bool) -> Result<(), ExecutionError>;

    /// Force a checkpoint now.
    async fn checkpoint_now(&self) -> Result<CheckpointHandle, ExecutionError>;

    /// Enumerate known checkpoints.
    async fn checkpoints(&self) -> Vec<CheckpointHandle>;

    /// Pre-flight lint — returns diagnostics without running the plan.
    async fn dry_run(&self, plan: &ExecutionPlan) -> Vec<ExecutionDiagnostic>;
}

// ---------------------------------------------------------------------------
// DefaultExecutor — synchronous reference implementation.
// ---------------------------------------------------------------------------

/// Reference executor that walks the plan's topological order one
/// step at a time.
///
/// Supported controls:
/// - `pause` / `unpause` — polled at each scheduling iteration.
/// - `stop` — cooperative; the loop exits at the next boundary.
/// - `checkpoint_now` — captures a [`CheckpointState`] snapshot and
///   hands it to the attached [`ExecutionStateManager`].
///
/// Still stubbed: `resume` from a checkpoint id, retry policies.
pub struct DefaultExecutor {
    orchestrator: Arc<dyn Orchestrator>,
    scheduler:    Arc<dyn Scheduler>,
    state:        Arc<dyn ExecutionStateManager>,
    journal:      Arc<dyn JournalWriter>,
    observer:     Arc<dyn ExecutionObserver>,
    running:      Mutex<Option<ExecutionId>>,
    cancelled:    Mutex<Option<String>>,
    paused:       AtomicBool,
    checkpoints:  Mutex<Vec<CheckpointHandle>>,
    plan_fp:      Mutex<Option<Fingerprint>>,
    completed:    Mutex<BTreeSet<StepId>>,
    failed:       Mutex<BTreeSet<StepId>>,
    skipped:      Mutex<BTreeSet<StepId>>,
    pending_checkpoint: Mutex<Option<CheckpointHandleRequest>>,
}

/// Request for a synchronous checkpoint, fulfilled by the main loop
/// at its next scheduling boundary.
#[derive(Debug)]
struct CheckpointHandleRequest {
    id: CheckpointId,
}

impl std::fmt::Debug for DefaultExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultExecutor")
            .field("running", &*self.running.lock().expect("poisoned"))
            .field("cancelled", &*self.cancelled.lock().expect("poisoned"))
            .finish_non_exhaustive()
    }
}

impl DefaultExecutor {
    /// Construct from fully-wired collaborators.
    #[must_use]
    pub fn new(
        orchestrator: Arc<dyn Orchestrator>,
        scheduler:    Arc<dyn Scheduler>,
        state:        Arc<dyn ExecutionStateManager>,
        journal:      Arc<dyn JournalWriter>,
        observer:     Arc<dyn ExecutionObserver>,
    ) -> Self {
        Self {
            orchestrator,
            scheduler,
            state,
            journal,
            observer,
            running: Mutex::new(None),
            cancelled: Mutex::new(None),
            paused: AtomicBool::new(false),
            checkpoints: Mutex::new(Vec::new()),
            plan_fp: Mutex::new(None),
            completed: Mutex::new(BTreeSet::new()),
            failed: Mutex::new(BTreeSet::new()),
            skipped: Mutex::new(BTreeSet::new()),
            pending_checkpoint: Mutex::new(None),
        }
    }

    fn fresh_execution_id() -> ExecutionId {
        ExecutionId::from_ulid(Ulid::new())
    }

    fn take_cancel_reason(&self) -> Option<String> {
        self.cancelled.lock().expect("poisoned").take()
    }

    /// Build a [`Checkpoint`] from current executor state, hand it
    /// to the attached [`ExecutionStateManager`], track the handle
    /// locally, and return it.
    async fn record_checkpoint(
        &self,
        execution_id:    ExecutionId,
        plan_fingerprint: Fingerprint,
        checkpoint_id:   CheckpointId,
    ) -> Result<CheckpointHandle, ExecutionError> {
        let now = Timestamp::now();
        let snapshot = CheckpointState {
            completed_step_ids:  self.completed.lock().expect("poisoned").clone(),
            failed_step_ids:     self.failed.lock().expect("poisoned").clone(),
            skipped_step_ids:    self.skipped.lock().expect("poisoned").clone(),
            in_flight_step_ids:  BTreeSet::new(),
            completed_trials:    BTreeMap::new(),
            in_flight_trials:    BTreeSet::new(),
            instance_states:     BTreeMap::<InstanceId, OperationalState>::new(),
            scheduler_state:     self.scheduler.state(),
            resource_usage:      ResourceUsage::default(),
            journal_sequence:    JournalSequence::ZERO,
        };
        let snapshot_bytes = serde_json::to_vec(&snapshot)
            .map_err(|e| ExecutionError::Cancelled {
                reason: format!("checkpoint serialisation: {e}"),
            })?;
        let fingerprint = Fingerprint::of(&snapshot_bytes);
        let checkpoint = Checkpoint {
            id:               checkpoint_id.clone(),
            execution_id,
            plan_fingerprint,
            created_at:       now,
            state:            snapshot,
            fingerprint,
        };
        self.state.checkpoint(&checkpoint).await;
        let handle = CheckpointHandle {
            id:         checkpoint_id,
            created_at: now,
            is_valid:   true,
        };
        let mut list = self.checkpoints.lock().expect("poisoned");
        if !list.iter().any(|h| h.id == handle.id) {
            list.push(handle.clone());
        }
        let _ = TrialId::from_ulid(Ulid::nil()); // silence unused warning.
        Ok(handle)
    }
}

#[async_trait]
impl Executor for DefaultExecutor {
    #[allow(
        clippy::too_many_lines,
        reason = "single top-level loop; v0.2 parallel executor breaks this apart"
    )]
    async fn execute(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<ExecutionResults, ExecutionError> {
        let execution_id = Self::fresh_execution_id();
        *self.running.lock().expect("poisoned") = Some(execution_id);

        let started_at = Timestamp::now();
        let t0 = Instant::now();
        let plan_fingerprint = plan.fingerprint();

        self.observer.on_execution_started(&execution_id, plan);
        *self.plan_fp.lock().expect("poisoned") = Some(plan_fingerprint);
        self.completed.lock().expect("poisoned").clear();
        self.failed.lock().expect("poisoned").clear();
        self.skipped.lock().expect("poisoned").clear();

        let mut seq = JournalSequence::ZERO;
        seq = seq.next();
        let start_event = JournalEvent {
            sequence:     seq,
            execution_id,
            timestamp:    Timestamp::now(),
            kind:         JournalEventKind::ExecutionStarted { plan_fingerprint },
        };
        self.state.record_event(start_event.clone()).await;
        self.journal.write(start_event).await?;

        self.orchestrator.prepare(plan).await?;
        self.scheduler.initialise(plan).await?;

        let mut step_stats: BTreeMap<StepId, StepStatistics> = BTreeMap::new();
        let mut any_failed = false;
        let mut halted_step: Option<StepId> = None;
        let empty_resources = ResourceSnapshot::default();
        let on_failure = OnFailure::default();

        'sched: loop {
            if let Some(reason) = self.take_cancel_reason() {
                return finalise_cancelled(
                    self,
                    execution_id,
                    started_at,
                    t0,
                    step_stats,
                    seq,
                    &reason,
                )
                .await;
            }

            // Cooperative pause: spin until `unpause` clears it or a
            // stop arrives. The spin is cheap — consumers typically
            // unpause within a few ticks.
            while self.paused.load(Ordering::SeqCst) {
                if self.take_cancel_reason().is_some() {
                    return finalise_cancelled(
                        self,
                        execution_id,
                        started_at,
                        t0,
                        step_stats,
                        seq,
                        "stopped while paused",
                    )
                    .await;
                }
                tokio::task::yield_now().await;
            }

            // Drain any pending checkpoint request at a safe boundary.
            let pending = self.pending_checkpoint.lock().expect("poisoned").take();
            if let Some(req) = pending {
                let handle = self
                    .record_checkpoint(execution_id, plan_fingerprint, req.id.clone())
                    .await?;
                let completed_count = u32::try_from(
                    self.completed.lock().expect("poisoned").len(),
                )
                .unwrap_or(u32::MAX);
                seq = seq.next();
                self.journal
                    .write(JournalEvent {
                        sequence:     seq,
                        execution_id,
                        timestamp:    Timestamp::now(),
                        kind:         JournalEventKind::CheckpointCreated {
                            checkpoint_id:        req.id,
                            completed_step_count: completed_count,
                        },
                    })
                    .await?;
                self.observer.on_checkpoint_created(&execution_id, &handle);
            }

            let ready = self.scheduler.next_steps(1, &empty_resources).await;
            if ready.is_empty() {
                if self.scheduler.is_complete() {
                    break 'sched;
                }
                // Nothing ready but still work outstanding -> cycle / bug;
                // break to avoid spinning.
                break 'sched;
            }

            for step_id in ready {
                let Some(step) = plan.execution_graph.step(&step_id).cloned() else {
                    continue;
                };
                let now = Timestamp::now();
                self.scheduler.mark_started(&step_id, now).await;
                self.observer.on_step_started(&execution_id, &step, 1);

                let entry = step_stats.entry(step_id.clone()).or_default();
                entry.attempts = entry.attempts.saturating_add(1);

                seq = seq.next();
                self.journal
                    .write(JournalEvent {
                        sequence:     seq,
                        execution_id,
                        timestamp:    now,
                        kind:         JournalEventKind::StepStarted {
                            step_id: step_id.clone(),
                            attempt: 1,
                        },
                    })
                    .await?;

                let step_t0 = Instant::now();
                let outcome = self.orchestrator.execute_step(plan, &step).await;

                match outcome {
                    Ok(StepOutcome::Completed { .. }) => {
                        let elapsed = step_t0.elapsed();
                        let at = Timestamp::now();
                        self.scheduler.mark_completed(&step_id, at).await;
                        entry.completed = entry.completed.saturating_add(1);
                        self.completed
                            .lock()
                            .expect("poisoned")
                            .insert(step_id.clone());
                        self.observer
                            .on_step_completed(&execution_id, &step, 1, elapsed);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepCompleted {
                                    step_id:  step_id.clone(),
                                    attempt:  1,
                                    duration: elapsed,
                                },
                            })
                            .await?;
                    }
                    Ok(StepOutcome::Skipped { reason }) => {
                        let at = Timestamp::now();
                        self.scheduler.mark_skipped(&step_id, at, &reason).await;
                        entry.skipped = entry.skipped.saturating_add(1);
                        self.skipped
                            .lock()
                            .expect("poisoned")
                            .insert(step_id.clone());
                        self.observer.on_step_skipped(&execution_id, &step, &reason);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepSkipped {
                                    step_id: step_id.clone(),
                                    reason,
                                },
                            })
                            .await?;
                    }
                    Ok(StepOutcome::Failed { error }) => {
                        any_failed = true;
                        let at = Timestamp::now();
                        self.scheduler.mark_failed(&step_id, at, &error).await;
                        entry.failed = entry.failed.saturating_add(1);
                        self.failed
                            .lock()
                            .expect("poisoned")
                            .insert(step_id.clone());
                        self.observer.on_step_failed(&execution_id, &step, 1, &error);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepFailed {
                                    step_id: step_id.clone(),
                                    attempt: 1,
                                    error,
                                },
                            })
                            .await?;
                        if matches!(on_failure, OnFailure::Stop) {
                            halted_step = Some(step_id.clone());
                            break 'sched;
                        }
                    }
                    Err(err) => {
                        let err_info = ErrorInfo::builder()
                            .kind("OrchestratorError".to_owned())
                            .message(format!("{err}"))
                            .build();
                        let at = Timestamp::now();
                        self.scheduler.mark_failed(&step_id, at, &err_info).await;
                        entry.failed = entry.failed.saturating_add(1);
                        self.observer
                            .on_step_failed(&execution_id, &step, 1, &err_info);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepFailed {
                                    step_id: step_id.clone(),
                                    attempt: 1,
                                    error:   err_info,
                                },
                            })
                            .await?;
                        return Err(err.into());
                    }
                }
            }
        }

        let completed_at = Timestamp::now();
        let duration = t0.elapsed();

        let status = if let Some(step) = halted_step {
            self.orchestrator.shutdown().await?;
            let results = ExecutionResults {
                execution_id,
                status: ExecutionStatus::Halted,
                started_at,
                completed_at,
                duration,
                step_stats,
                trial_results: Vec::new(),
            };
            write_completed(&*self.journal, execution_id, &mut seq, duration, false).await?;
            self.observer.on_execution_completed(&execution_id, &results);
            *self.running.lock().expect("poisoned") = None;
            return Err(ExecutionError::StopOnFailure { step });
        } else if any_failed {
            ExecutionStatus::PartialFailure
        } else {
            ExecutionStatus::Succeeded
        };

        self.orchestrator.shutdown().await?;
        write_completed(
            &*self.journal,
            execution_id,
            &mut seq,
            duration,
            status == ExecutionStatus::Succeeded,
        )
        .await?;

        let results = ExecutionResults {
            execution_id,
            status,
            started_at,
            completed_at,
            duration,
            step_stats,
            trial_results: Vec::new(),
        };
        self.observer.on_execution_completed(&execution_id, &results);
        *self.running.lock().expect("poisoned") = None;
        Ok(results)
    }

    async fn resume(
        &self,
        plan:        &ExecutionPlan,
        _checkpoint: &CheckpointId,
    ) -> Result<ExecutionResults, ExecutionError> {
        Err(ExecutionError::PlanMismatch {
            checkpoint: Fingerprint::of(b"no-checkpoint"),
            plan:       plan.fingerprint(),
        })
    }

    async fn pause(&self, reason: &str) -> Result<(), ExecutionError> {
        self.paused.store(true, Ordering::SeqCst);
        let exec = self
            .running
            .lock()
            .expect("poisoned")
            .unwrap_or_else(|| ExecutionId::from_ulid(Ulid::nil()));
        self.observer.on_intervention(
            &exec,
            &InterventionKind::Pause {
                reason: reason.to_owned(),
            },
        );
        Ok(())
    }

    async fn unpause(&self) -> Result<(), ExecutionError> {
        self.paused.store(false, Ordering::SeqCst);
        let exec = self
            .running
            .lock()
            .expect("poisoned")
            .unwrap_or_else(|| ExecutionId::from_ulid(Ulid::nil()));
        self.observer
            .on_intervention(&exec, &InterventionKind::Resume);
        Ok(())
    }

    async fn stop(&self, reason: &str, graceful: bool) -> Result<(), ExecutionError> {
        *self.cancelled.lock().expect("poisoned") = Some(reason.to_owned());
        self.observer.on_intervention(
            &ExecutionId::from_ulid(Ulid::nil()),
            &InterventionKind::Stop {
                reason: reason.to_owned(),
                graceful,
            },
        );
        Ok(())
    }

    async fn checkpoint_now(&self) -> Result<CheckpointHandle, ExecutionError> {
        // If a run is in flight, queue a request the main loop will
        // drain at its next scheduling boundary. Otherwise build the
        // checkpoint immediately from the last completed run's state.
        let exec = *self.running.lock().expect("poisoned");
        let fp = self
            .plan_fp
            .lock()
            .expect("poisoned")
            .unwrap_or_else(|| Fingerprint::of(b""));
        let checkpoint_id = CheckpointId::new(format!(
            "checkpoint_{}",
            Ulid::new()
        ))
        .map_err(|e| ExecutionError::Cancelled {
            reason: format!("{e}"),
        })?;

        if let Some(execution_id) = exec {
            *self.pending_checkpoint.lock().expect("poisoned") =
                Some(CheckpointHandleRequest {
                    id: checkpoint_id.clone(),
                });
            // Main loop writes the actual checkpoint; synthesize the
            // handle now so callers have something to refer to.
            let handle = CheckpointHandle {
                id:         checkpoint_id,
                created_at: Timestamp::now(),
                is_valid:   true,
            };
            self.checkpoints
                .lock()
                .expect("poisoned")
                .push(handle.clone());
            let _ = execution_id;
            Ok(handle)
        } else {
            // Not running — materialise the checkpoint inline.
            let execution_id = ExecutionId::from_ulid(Ulid::nil());
            let handle = self
                .record_checkpoint(execution_id, fp, checkpoint_id.clone())
                .await?;
            Ok(handle)
        }
    }

    async fn checkpoints(&self) -> Vec<CheckpointHandle> {
        self.checkpoints.lock().expect("poisoned").clone()
    }

    async fn dry_run(&self, plan: &ExecutionPlan) -> Vec<ExecutionDiagnostic> {
        let mut diagnostics = Vec::new();
        if let Err(err) = plan.execution_graph.validate() {
            diagnostics.push(ExecutionDiagnostic {
                code:    "plan.invalid".to_owned(),
                message: format!("{err}"),
            });
        }
        diagnostics
    }
}

async fn write_completed(
    journal:      &dyn JournalWriter,
    execution_id: ExecutionId,
    seq:          &mut JournalSequence,
    duration:     Duration,
    success:      bool,
) -> Result<(), ExecutionError> {
    *seq = seq.next();
    journal
        .write(JournalEvent {
            sequence:     *seq,
            execution_id,
            timestamp:    Timestamp::now(),
            kind:         JournalEventKind::ExecutionCompleted { success, duration },
        })
        .await?;
    Ok(())
}

async fn finalise_cancelled(
    exec:         &DefaultExecutor,
    execution_id: ExecutionId,
    started_at:   Timestamp,
    t0:           Instant,
    step_stats:   BTreeMap<StepId, StepStatistics>,
    mut seq:      JournalSequence,
    reason:       &str,
) -> Result<ExecutionResults, ExecutionError> {
    let completed_at = Timestamp::now();
    let duration = t0.elapsed();
    exec.orchestrator.shutdown().await?;
    write_completed(&*exec.journal, execution_id, &mut seq, duration, false).await?;
    let results = ExecutionResults {
        execution_id,
        status: ExecutionStatus::Cancelled,
        started_at,
        completed_at,
        duration,
        step_stats,
        trial_results: Vec::new(),
    };
    exec.observer.on_execution_completed(&execution_id, &results);
    *exec.running.lock().expect("poisoned") = None;
    Err(ExecutionError::Cancelled {
        reason: reason.to_owned(),
    })
}
