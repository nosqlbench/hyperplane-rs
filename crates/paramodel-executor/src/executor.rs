// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Executor` trait + `DefaultExecutor`.
//!
//! V0.1 ships a synchronous reference executor: one step at a time,
//! in topological order, no retries, no checkpoint emission, no
//! resumption logic. Parallel execution, retries, and resume arrive
//! in follow-up slices — the trait surface anticipates them.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{FuturesUnordered, StreamExt};
use jiff::Timestamp;
use paramodel_elements::{Fingerprint, OperationalState, TrialId};
use paramodel_plan::{
    AtomicStep, CheckpointId, ExecutionPlan, InstanceId, InterventionMode, OnFailure,
    PartialRunBehavior, StepId,
};
use paramodel_trials::{
    ArtifactRef, ErrorInfo, ExecutionTiming, Metrics, TrialResult, TrialStatus,
};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{ExecutionDiagnostic, ExecutionError, OrchestratorError};
use crate::ids::{ExecutionId, JournalSequence};
use crate::journal::{JournalEvent, JournalEventKind, JournalWriter};
use crate::observer::{ExecutionObserver, InterventionKind};
use crate::orchestrator::{Orchestrator, StepOutcome};
use crate::resources::{
    DefaultResourceManager, ResourceAllocation, ResourceManager, ResourceRequest,
    ResourceSnapshot, ResourceUsage,
};
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
    orchestrator:     Arc<dyn Orchestrator>,
    scheduler:        Arc<dyn Scheduler>,
    state:            Arc<dyn ExecutionStateManager>,
    journal:          Arc<dyn JournalWriter>,
    observer:         Arc<dyn ExecutionObserver>,
    resource_manager: Arc<dyn ResourceManager>,
    running:      Mutex<Option<ExecutionId>>,
    cancelled:    Mutex<Option<String>>,
    paused:       AtomicBool,
    checkpoints:  Mutex<Vec<CheckpointHandle>>,
    plan_fp:      Mutex<Option<Fingerprint>>,
    completed:    Mutex<BTreeSet<StepId>>,
    failed:       Mutex<BTreeSet<StepId>>,
    skipped:      Mutex<BTreeSet<StepId>>,
    pending_checkpoint: Mutex<Option<CheckpointHandleRequest>>,
    /// Upper bound on concurrently in-flight steps. Defaults to 1
    /// (serial execution). Set via
    /// [`DefaultExecutor::with_parallelism`].
    max_parallel: usize,
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
            resource_manager: Arc::new(DefaultResourceManager::new()),
            running: Mutex::new(None),
            cancelled: Mutex::new(None),
            paused: AtomicBool::new(false),
            checkpoints: Mutex::new(Vec::new()),
            plan_fp: Mutex::new(None),
            completed: Mutex::new(BTreeSet::new()),
            failed: Mutex::new(BTreeSet::new()),
            skipped: Mutex::new(BTreeSet::new()),
            pending_checkpoint: Mutex::new(None),
            max_parallel: 1,
        }
    }

    /// Opt-in: use the supplied resource manager instead of the
    /// default unbounded one. A bounded manager lets the executor
    /// apply back-pressure: steps whose resource requirements don't
    /// fit the current pool stay in the ready set until capacity
    /// frees up.
    #[must_use]
    pub fn with_resource_manager(mut self, rm: Arc<dyn ResourceManager>) -> Self {
        self.resource_manager = rm;
        self
    }

    /// Opt-in: run up to `n` steps concurrently (floor 1). The
    /// scheduler hands out up to `n` ready steps per wave; each runs
    /// its retry loop independently. Journal writes, step-stats
    /// updates, and trial-aggregation stay serialised in the main
    /// loop to preserve event ordering and avoid cross-task races.
    #[must_use]
    pub fn with_parallelism(mut self, n: usize) -> Self {
        self.max_parallel = n.max(1);
        self
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

    /// Record a checkpoint *and* emit the matching journal event,
    /// pruning the local list to `max_checkpoints` if set. The shared
    /// hook used by `AtomicStep::Checkpoint`, `CheckpointStrategy`
    /// (barrier / error / interval), and manual `checkpoint_now()`
    /// drains.
    async fn emit_checkpoint(
        &self,
        execution_id:     ExecutionId,
        plan_fingerprint: Fingerprint,
        checkpoint_id:    Option<CheckpointId>,
        kind_label:       &str,
        mut seq:          JournalSequence,
        max_checkpoints:  Option<u32>,
    ) -> Result<JournalSequence, ExecutionError> {
        let id = match checkpoint_id {
            Some(id) => id,
            None => CheckpointId::new(format!("auto_{}_{}", kind_label, Ulid::new()))
                .map_err(|e| ExecutionError::Cancelled {
                    reason: format!("checkpoint id: {e}"),
                })?,
        };
        let handle = self
            .record_checkpoint(execution_id, plan_fingerprint, id.clone())
            .await?;

        if let Some(max) = max_checkpoints {
            let mut list = self.checkpoints.lock().expect("poisoned");
            let cap = usize::try_from(max).unwrap_or(usize::MAX);
            while list.len() > cap {
                list.remove(0);
            }
        }

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
                    checkpoint_id:        id,
                    completed_step_count: completed_count,
                },
            })
            .await?;
        self.observer.on_checkpoint_created(&execution_id, &handle);
        Ok(seq)
    }
}

#[async_trait]
impl Executor for DefaultExecutor {
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

        self.run_to_completion(
            plan,
            plan_fingerprint,
            execution_id,
            started_at,
            t0,
            seq,
            None,
        )
        .await
    }

    async fn resume(
        &self,
        plan:       &ExecutionPlan,
        checkpoint: &CheckpointId,
    ) -> Result<ExecutionResults, ExecutionError> {
        let loaded = self.state.load_checkpoint(checkpoint).await.ok_or_else(|| {
            ExecutionError::PlanMismatch {
                checkpoint: Fingerprint::of(b"no-checkpoint"),
                plan:       plan.fingerprint(),
            }
        })?;
        let plan_fingerprint = plan.fingerprint();
        if loaded.plan_fingerprint != plan_fingerprint {
            return Err(ExecutionError::PlanMismatch {
                checkpoint: loaded.plan_fingerprint,
                plan:       plan_fingerprint,
            });
        }
        let execution_id = loaded.execution_id;
        *self.running.lock().expect("poisoned") = Some(execution_id);
        *self.plan_fp.lock().expect("poisoned") = Some(plan_fingerprint);
        // Seed self-local bookkeeping from the checkpoint.
        *self.completed.lock().expect("poisoned") =
            loaded.state.completed_step_ids.clone();
        *self.failed.lock().expect("poisoned") =
            loaded.state.failed_step_ids.clone();
        *self.skipped.lock().expect("poisoned") =
            loaded.state.skipped_step_ids.clone();

        self.observer.on_execution_started(&execution_id, plan);

        let mut seq = loaded.state.journal_sequence;
        seq = seq.next();
        let resume_event = JournalEvent {
            sequence:     seq,
            execution_id,
            timestamp:    Timestamp::now(),
            kind:         JournalEventKind::ExecutionResumed {
                checkpoint_id: loaded.id.clone(),
            },
        };
        self.state.record_event(resume_event.clone()).await;
        self.journal.write(resume_event).await?;

        let started_at = Timestamp::now();
        let t0 = Instant::now();

        self.run_to_completion(
            plan,
            plan_fingerprint,
            execution_id,
            started_at,
            t0,
            seq,
            Some(&loaded.state),
        )
        .await
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

impl DefaultExecutor {
    #[allow(
        clippy::too_many_lines,
        clippy::too_many_arguments,
        reason = "single top-level scheduling loop shared by execute + resume; \
                  v0.2 parallel executor breaks this apart"
    )]
    async fn run_to_completion(
        &self,
        plan:             &ExecutionPlan,
        plan_fingerprint: Fingerprint,
        execution_id:     ExecutionId,
        started_at:       Timestamp,
        t0:               Instant,
        mut seq:          JournalSequence,
        resume_from:      Option<&CheckpointState>,
    ) -> Result<ExecutionResults, ExecutionError> {
        self.orchestrator.prepare(plan).await?;
        self.scheduler.initialise(plan).await?;

        let mut step_stats: BTreeMap<StepId, StepStatistics> = BTreeMap::new();

        // Seed scheduler + step_stats from the checkpoint, if any.
        // Order: completed first (so their dependents advance without
        // triggering cascades), then skipped, then failed. The
        // scheduler's cascade on mark_failed only affects pending/
        // ready steps, so marking already-terminal dependents later
        // is a no-op for the set.
        if let Some(seed) = resume_from {
            let now = Timestamp::now();
            for sid in &seed.completed_step_ids {
                self.scheduler.mark_completed(sid, now).await;
                let entry = step_stats.entry(sid.clone()).or_default();
                entry.attempts = entry.attempts.saturating_add(1);
                entry.completed = entry.completed.saturating_add(1);
            }
            for sid in &seed.skipped_step_ids {
                self.scheduler
                    .mark_skipped(sid, now, "restored from checkpoint")
                    .await;
                let entry = step_stats.entry(sid.clone()).or_default();
                entry.skipped = entry.skipped.saturating_add(1);
            }
            for sid in &seed.failed_step_ids {
                let _ = self
                    .scheduler
                    .mark_failed(
                        sid,
                        now,
                        &ErrorInfo::builder()
                            .kind("RestoredFromCheckpoint".to_owned())
                            .message("failure observed before checkpoint".to_owned())
                            .build(),
                    )
                    .await;
                let entry = step_stats.entry(sid.clone()).or_default();
                entry.attempts = entry.attempts.saturating_add(1);
                entry.failed = entry.failed.saturating_add(1);
            }
        }

        let mut any_failed = false;
        let mut halted_step: Option<StepId> = None;
        let empty_resources = ResourceSnapshot::default();
        let on_failure = plan.policies.on_failure;
        let intervention_mode = plan.policies.intervention_mode;
        let partial_run_behavior = plan.policies.partial_run_behavior;
        // Trial-level aggregation. `trial_starts` records the wall-clock
        // time a `TrialStart` step completed; `trial_failed` tracks
        // which trials saw a step-level failure; `trial_metrics` and
        // `trial_artifacts` collect outputs captured by `SaveOutput`
        // steps, keyed by trial. `aggregated_results` accumulates one
        // `TrialResult` per completed `TrialEnd`.
        let mut trial_starts: BTreeMap<TrialId, Timestamp> = BTreeMap::new();
        let mut trial_failed: BTreeSet<TrialId> = BTreeSet::new();
        let mut trial_errors: BTreeMap<TrialId, ErrorInfo> = BTreeMap::new();
        let mut trial_metrics:
            BTreeMap<TrialId, BTreeMap<
                paramodel_elements::ElementName,
                BTreeMap<paramodel_elements::ParameterName, paramodel_elements::Value>,
            >> = BTreeMap::new();
        let mut trial_artifacts: BTreeMap<TrialId, Vec<ArtifactRef>> = BTreeMap::new();
        let mut aggregated_results: Vec<TrialResult> = Vec::new();

        let retry_budget = match on_failure {
            OnFailure::Retry { attempts } => attempts,
            _ => 0,
        };
        let max_attempts = retry_budget.saturating_add(1);

        // Pool of in-flight step runs. Filled from
        // `scheduler.next_steps` up to `self.max_parallel`; each
        // future owns Arc-cloned collaborators and reports back a
        // `StepExecution` the main loop consumes.
        let mut in_flight: FuturesUnordered<
            Pin<Box<dyn Future<Output = StepExecution> + Send + '_>>,
        > = FuturesUnordered::new();
        // Allocations currently held on behalf of in-flight steps.
        // Keyed by step id; removed + released when the step's task
        // finishes.
        let mut in_flight_allocations: BTreeMap<StepId, ResourceAllocation> =
            BTreeMap::new();
        // When the last checkpoint (manual or auto) was recorded.
        // `CheckpointStrategy::interval` uses this to decide when to
        // fire the next interval-triggered checkpoint.
        let mut last_checkpoint_at: Option<Instant> = None;

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
                    aggregated_results,
                    partial_run_behavior,
                )
                .await;
            }

            // Cooperative pause. Under `InterventionMode::Immediate`
            // we spin the moment `paused` is set; under
            // `AfterActiveTrials` we keep draining in-flight work
            // first and only spin once the pool is empty — the
            // top-up section below won't admit new steps while
            // `paused` is set.
            while self.paused.load(Ordering::SeqCst) {
                if matches!(intervention_mode, InterventionMode::AfterActiveTrials)
                    && !in_flight.is_empty()
                {
                    break;
                }
                if self.take_cancel_reason().is_some() {
                    return finalise_cancelled(
                        self,
                        execution_id,
                        started_at,
                        t0,
                        step_stats,
                        seq,
                        "stopped while paused",
                        Vec::new(),
                        partial_run_behavior,
                    )
                    .await;
                }
                tokio::task::yield_now().await;
            }

            // Drain any pending checkpoint request at a safe boundary.
            let pending = self.pending_checkpoint.lock().expect("poisoned").take();
            if let Some(req) = pending {
                seq = self
                    .emit_checkpoint(
                        execution_id,
                        plan_fingerprint,
                        Some(req.id),
                        "manual",
                        seq,
                        plan.checkpoint_strategy
                            .as_ref()
                            .and_then(|s| s.max_checkpoints),
                    )
                    .await?;
                last_checkpoint_at = Some(Instant::now());
            }

            // `CheckpointStrategy::interval` — if enough wall-clock
            // has elapsed since the last checkpoint, fire a new one.
            if let Some(strategy) = plan.checkpoint_strategy.as_ref()
                && let Some(interval) = strategy.interval
            {
                let fire = match last_checkpoint_at {
                    None => true,
                    Some(t) => t.elapsed() >= interval,
                };
                if fire {
                    seq = self
                        .emit_checkpoint(
                            execution_id,
                            plan_fingerprint,
                            None,
                            "interval",
                            seq,
                            strategy.max_checkpoints,
                        )
                        .await?;
                    last_checkpoint_at = Some(Instant::now());
                }
            }

            // Top up the in-flight pool from the scheduler. On halt
            // or pause we stop admitting new work but still drain
            // what's in flight so their journal events land and their
            // results get recorded.
            let _ = &empty_resources; // preserved for cancellation paths
            if halted_step.is_none() && !self.paused.load(Ordering::SeqCst) {
                let slots = self.max_parallel.saturating_sub(in_flight.len());
                if slots > 0 {
                    let snapshot = self.resource_manager.available();
                    let ready = self.scheduler.next_steps(slots, &snapshot).await;
                    for step_id in ready {
                        let Some(step) = plan.execution_graph.step(&step_id).cloned() else {
                            continue;
                        };
                        // Allocate this step's declared resources up
                        // front. Scheduler filtering means this should
                        // only fail if the manager's view is stricter
                        // than the snapshot — surface as step-level
                        // failure via `StepFailed` rather than tearing
                        // down the run.
                        let req = resource_request_for(&step);
                        let allocation = match self.resource_manager.allocate(&req).await {
                            Ok(a) => a,
                            Err(err) => {
                                // Return step to ready for a future
                                // iteration once capacity frees up.
                                self.scheduler.mark_skipped(
                                    &step_id,
                                    Timestamp::now(),
                                    &format!("resource allocation failed: {err}"),
                                ).await;
                                continue;
                            }
                        };
                        in_flight_allocations.insert(step_id.clone(), allocation);
                        let sched = self.scheduler.clone();
                        let orch = self.orchestrator.clone();
                        let fut = run_step_with_retries(
                            sched,
                            orch,
                            plan,
                            step_id.clone(),
                            step,
                            max_attempts,
                            plan.policies.trial_timeout,
                            plan.policies.element_start_timeout,
                        );
                        in_flight.push(Box::pin(fut)
                            as Pin<Box<dyn Future<Output = StepExecution> + Send + '_>>);
                    }
                }
            }

            if in_flight.is_empty() {
                if self.scheduler.is_complete() {
                    break 'sched;
                }
                // Nothing ready, nothing in flight, not complete —
                // cycle or bug; break to avoid spinning.
                break 'sched;
            }

            // Await the next completed step. All journal writes + state
            // mutation for that step happen serially below.
            let completion = in_flight.next().await.expect("non-empty in_flight");
            let StepExecution {
                step_id,
                step,
                intermediate_failures,
                final_attempt,
                final_started_at,
                final_finished_at,
                final_elapsed,
                final_outcome,
            } = completion;

            // Release this step's resource allocation now — doing it
            // before we touch the scheduler means the next iteration's
            // `scheduler.next_steps` sees fresh capacity and can
            // unblock any step that was held back by back-pressure.
            if let Some(alloc) = in_flight_allocations.remove(&step_id) {
                self.resource_manager.release(alloc).await;
            }

            // Per-step stats accounting: intermediate failures all count
            // as attempts; the final attempt is one more.
            {
                let entry = step_stats.entry(step_id.clone()).or_default();
                entry.attempts = entry.attempts.saturating_add(
                    u32::try_from(intermediate_failures.len())
                        .unwrap_or(u32::MAX)
                        .saturating_add(1),
                );
            }

            // Emit the per-attempt journal trail for retries, in
            // order: StepStarted(n), StepFailed(n), for each
            // intermediate. The final attempt's StepStarted + terminal
            // event come after.
            for att in &intermediate_failures {
                self.observer.on_step_started(&execution_id, &step, att.attempt);
                seq = seq.next();
                self.journal
                    .write(JournalEvent {
                        sequence:     seq,
                        execution_id,
                        timestamp:    att.started_at,
                        kind:         JournalEventKind::StepStarted {
                            step_id: step_id.clone(),
                            attempt: att.attempt,
                        },
                    })
                    .await?;
                self.observer.on_step_failed(
                    &execution_id,
                    &step,
                    att.attempt,
                    &att.error,
                );
                seq = seq.next();
                self.journal
                    .write(JournalEvent {
                        sequence:     seq,
                        execution_id,
                        timestamp:    att.finished_at,
                        kind:         JournalEventKind::StepFailed {
                            step_id: step_id.clone(),
                            attempt: att.attempt,
                            error:   att.error.clone(),
                        },
                    })
                    .await?;
            }

            // Final attempt: StepStarted + terminal event. Bind
            // aliases so the existing outcome-dispatch logic stays
            // readable.
            self.observer.on_step_started(&execution_id, &step, final_attempt);
            seq = seq.next();
            self.journal
                .write(JournalEvent {
                    sequence:     seq,
                    execution_id,
                    timestamp:    final_started_at,
                    kind:         JournalEventKind::StepStarted {
                        step_id: step_id.clone(),
                        attempt: final_attempt,
                    },
                })
                .await?;

            let attempt = final_attempt;
            let now = final_started_at;
            let at = final_finished_at;
            let elapsed = final_elapsed;
            let outcome = final_outcome;

            // Synthesise the same step_stats entry the old inline
            // code relied on — retries already bumped `attempts`; the
            // match arms below bump `completed` / `skipped` / `failed`
            // as before.
            let entry = step_stats.entry(step_id.clone()).or_default();

            match outcome {
                    Ok(StepOutcome::Completed {
                        ref metrics,
                        ref artifacts,
                        ..
                    }) => {
                        self.scheduler.mark_completed(&step_id, at).await;
                        entry.completed = entry.completed.saturating_add(1);
                        self.completed
                            .lock()
                            .expect("poisoned")
                            .insert(step_id.clone());
                        self.observer
                            .on_step_completed(&execution_id, &step, attempt, elapsed);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepCompleted {
                                    step_id:  step_id.clone(),
                                    attempt,
                                    duration: elapsed,
                                },
                            })
                            .await?;
                        // A `Checkpoint` atomic step auto-triggers a
                        // checkpoint at this boundary.
                        if let AtomicStep::Checkpoint { checkpoint_id, .. } = &step {
                            seq = self
                                .emit_checkpoint(
                                    execution_id,
                                    plan_fingerprint,
                                    Some(checkpoint_id.clone()),
                                    "step",
                                    seq,
                                    plan.checkpoint_strategy
                                        .as_ref()
                                        .and_then(|s| s.max_checkpoints),
                                )
                                .await?;
                            last_checkpoint_at = Some(Instant::now());
                        }
                        // `CheckpointStrategy::checkpoint_on_barriers`
                        // — if this step was a Barrier and the strategy
                        // is set, auto-checkpoint after it clears.
                        if matches!(&step, AtomicStep::Barrier { .. })
                            && plan
                                .checkpoint_strategy
                                .as_ref()
                                .is_some_and(|s| s.checkpoint_on_barriers)
                        {
                            seq = self
                                .emit_checkpoint(
                                    execution_id,
                                    plan_fingerprint,
                                    None,
                                    "barrier",
                                    seq,
                                    plan.checkpoint_strategy
                                        .as_ref()
                                        .and_then(|s| s.max_checkpoints),
                                )
                                .await?;
                            last_checkpoint_at = Some(Instant::now());
                        }
                        // Trial-level aggregation hooks.
                        if let AtomicStep::TrialStart { trial_id, .. } = &step {
                            trial_starts.insert(trial_id.clone(), now);
                        }
                        if let AtomicStep::SaveOutput { element, trial_id, .. } = &step {
                            if !metrics.is_empty() {
                                let bucket =
                                    trial_metrics.entry(trial_id.clone()).or_default();
                                let by_param = bucket.entry(element.clone()).or_default();
                                for (pname, value) in metrics {
                                    by_param.insert(pname.clone(), value.clone());
                                }
                            }
                            if !artifacts.is_empty() {
                                trial_artifacts
                                    .entry(trial_id.clone())
                                    .or_default()
                                    .extend(artifacts.iter().cloned());
                            }
                        }
                        if let AtomicStep::TrialEnd { trial_id, .. } = &step {
                            aggregated_results.push(synthesise_trial_result(
                                trial_id,
                                trial_starts.get(trial_id).copied().unwrap_or(now),
                                at,
                                trial_failed.contains(trial_id),
                                trial_errors.get(trial_id).cloned(),
                                trial_metrics.remove(trial_id).unwrap_or_default(),
                                trial_artifacts.remove(trial_id).unwrap_or_default(),
                            ));
                        }
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
                        if let Some(tid) = step.trial_id() {
                            trial_failed.insert(tid.clone());
                            trial_errors.entry(tid.clone()).or_insert_with(|| error.clone());
                        }
                        let at = Timestamp::now();
                        let cascaded = self
                            .scheduler
                            .mark_failed(&step_id, at, &error)
                            .await;
                        entry.failed = entry.failed.saturating_add(1);
                        self.failed
                            .lock()
                            .expect("poisoned")
                            .insert(step_id.clone());
                        self.observer.on_step_failed(&execution_id, &step, attempt, &error);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepFailed {
                                    step_id: step_id.clone(),
                                    attempt,
                                    error,
                                },
                            })
                            .await?;
                        if matches!(on_failure, OnFailure::Stop) {
                            halted_step = Some(step_id.clone());
                            break 'sched;
                        }
                        // OnFailure::Skip (or Retry after budget is
                        // exhausted) — emit journal events for each
                        // cascaded-skipped dependent so the journal
                        // reflects the scheduler's internal cascade.
                        seq = emit_cascaded_skips(
                            &*self.journal,
                            &self.skipped,
                            execution_id,
                            &mut step_stats,
                            cascaded,
                            &format!("depends on failed step {}", step_id.as_str()),
                            seq,
                        )
                        .await?;
                        // `CheckpointStrategy::checkpoint_on_errors`
                        // — fire a checkpoint on terminal failure so
                        // a future resume can pick up near the error.
                        if plan
                            .checkpoint_strategy
                            .as_ref()
                            .is_some_and(|s| s.checkpoint_on_errors)
                        {
                            seq = self
                                .emit_checkpoint(
                                    execution_id,
                                    plan_fingerprint,
                                    None,
                                    "error",
                                    seq,
                                    plan.checkpoint_strategy
                                        .as_ref()
                                        .and_then(|s| s.max_checkpoints),
                                )
                                .await?;
                            last_checkpoint_at = Some(Instant::now());
                        }
                    }
                    Err(err) => {
                        let err_info = ErrorInfo::builder()
                            .kind("OrchestratorError".to_owned())
                            .message(format!("{err}"))
                            .build();
                        let at = Timestamp::now();
                        let _cascaded = self
                            .scheduler
                            .mark_failed(&step_id, at, &err_info)
                            .await;
                        entry.failed = entry.failed.saturating_add(1);
                        self.observer
                            .on_step_failed(&execution_id, &step, attempt, &err_info);
                        seq = seq.next();
                        self.journal
                            .write(JournalEvent {
                                sequence:     seq,
                                execution_id,
                                timestamp:    at,
                                kind:         JournalEventKind::StepFailed {
                                    step_id: step_id.clone(),
                                    attempt,
                                    error:   err_info,
                                },
                            })
                            .await?;
                        return Err(err.into());
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
                trial_results: aggregated_results.clone(),
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
            trial_results: aggregated_results,
        };
        self.observer.on_execution_completed(&execution_id, &results);
        *self.running.lock().expect("poisoned") = None;
        Ok(results)
    }
}

/// Build a [`ResourceRequest`] from a step's declared
/// [`ResourceRequirements`]. The request inherits `peak_*` fields
/// and any custom asks; pool / owner / duration are left unset.
fn resource_request_for(step: &AtomicStep) -> ResourceRequest {
    let req = &step.header().resource_requirements;
    ResourceRequest {
        cpu_cores:    req.peak_cpu_cores,
        memory_mb:    req.peak_memory_mb,
        storage_gb:   req.peak_storage_gb,
        network_gbps: req.peak_network_gbps,
        custom:       req.custom.clone(),
        pool:         None,
        owner:        None,
        duration:     None,
    }
}

/// Captured record of one step's end-to-end run, including every
/// attempt. Produced by a scheduled task, consumed by the main loop
/// which emits journal events and updates shared state.
struct StepExecution {
    step_id:               StepId,
    step:                  AtomicStep,
    /// One entry per intermediate (retried) failure, in order. Each
    /// represents one full (StepStarted, StepFailed) journal pair.
    intermediate_failures: Vec<IntermediateAttempt>,
    /// Attempt number of the final try (1 if no retries).
    final_attempt:         u32,
    /// Timestamp when the final attempt began.
    final_started_at:      Timestamp,
    /// Timestamp when the final attempt ended.
    final_finished_at:     Timestamp,
    /// Elapsed wall-clock for the final attempt.
    final_elapsed:         Duration,
    /// Final outcome — what the terminal journal event reflects.
    final_outcome:         Result<StepOutcome, OrchestratorError>,
}

struct IntermediateAttempt {
    attempt:     u32,
    started_at:  Timestamp,
    finished_at: Timestamp,
    error:       ErrorInfo,
}

/// Runs the per-step retry loop. Task-local: does not write journal
/// events or update shared state — it reports everything via the
/// returned `StepExecution`. The main loop serializes journal writes
/// and state mutation.
///
/// `trial_timeout` caps any step whose `trial_id()` is `Some` (the
/// trial-lifecycle steps). `element_start_timeout` caps `Deploy`
/// steps specifically. On timeout the step is reported as
/// `StepOutcome::Failed` with `kind = "StepTimedOut"` so the
/// executor's `on_failure` policy applies as usual.
#[allow(clippy::too_many_arguments, reason = "step execution threads several collaborators + policy knobs")]
async fn run_step_with_retries(
    scheduler:             Arc<dyn Scheduler>,
    orchestrator:          Arc<dyn Orchestrator>,
    plan:                  &ExecutionPlan,
    step_id:               StepId,
    step:                  AtomicStep,
    max_attempts:          u32,
    trial_timeout:         Option<Duration>,
    element_start_timeout: Option<Duration>,
) -> StepExecution {
    let timeout_budget: Option<Duration> = match &step {
        AtomicStep::Deploy { .. } => element_start_timeout,
        _ if step.trial_id().is_some() => trial_timeout,
        _ => None,
    };
    let mut intermediate_failures: Vec<IntermediateAttempt> = Vec::new();
    let mut attempt: u32 = 1;
    loop {
        let started_at = Timestamp::now();
        scheduler.mark_started(&step_id, started_at).await;
        let step_t0 = Instant::now();
        let outcome = match timeout_budget {
            Some(budget) => {
                match tokio::time::timeout(
                    budget,
                    orchestrator.execute_step(plan, &step),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Ok(StepOutcome::Failed {
                        error: ErrorInfo::builder()
                            .kind("StepTimedOut".to_owned())
                            .message(format!(
                                "step '{}' exceeded timeout budget of {:?}",
                                step_id.as_str(),
                                budget,
                            ))
                            .build(),
                    }),
                }
            }
            None => orchestrator.execute_step(plan, &step).await,
        };
        let finished_at = Timestamp::now();
        let elapsed = step_t0.elapsed();

        if let Ok(StepOutcome::Failed { error: ref err_payload }) = outcome {
            if attempt < max_attempts {
                intermediate_failures.push(IntermediateAttempt {
                    attempt,
                    started_at,
                    finished_at,
                    error: err_payload.clone(),
                });
                attempt = attempt.saturating_add(1);
                continue;
            }
        }
        return StepExecution {
            step_id,
            step,
            intermediate_failures,
            final_attempt: attempt,
            final_started_at: started_at,
            final_finished_at: finished_at,
            final_elapsed: elapsed,
            final_outcome: outcome,
        };
    }
}

fn synthesise_trial_result(
    trial_id:   &TrialId,
    started_at: Timestamp,
    ended_at:   Timestamp,
    failed:     bool,
    error:      Option<ErrorInfo>,
    metrics:    BTreeMap<
        paramodel_elements::ElementName,
        BTreeMap<paramodel_elements::ParameterName, paramodel_elements::Value>,
    >,
    artifacts:  Vec<ArtifactRef>,
) -> TrialResult {
    let timing = ExecutionTiming::new(started_at, ended_at).unwrap_or_else(|_| {
        ExecutionTiming::new(started_at, started_at)
            .expect("zero-duration timing is always valid")
    });
    let metrics = Metrics::from_map(metrics);
    if failed {
        let err = error.unwrap_or_else(|| {
            ErrorInfo::builder()
                .kind("TrialFailed".to_owned())
                .message("one or more steps failed".to_owned())
                .build()
        });
        TrialResult::builder()
            .trial_id(trial_id.clone())
            .status(TrialStatus::Failed)
            .attempt(1)
            .timing(timing)
            .metrics(metrics)
            .artifacts(artifacts)
            .error(err)
            .build()
    } else {
        TrialResult::builder()
            .trial_id(trial_id.clone())
            .status(TrialStatus::Completed)
            .attempt(1)
            .timing(timing)
            .metrics(metrics)
            .artifacts(artifacts)
            .build()
    }
}

async fn emit_cascaded_skips(
    journal:      &dyn JournalWriter,
    skipped_set:  &Mutex<BTreeSet<StepId>>,
    execution_id: ExecutionId,
    step_stats:   &mut BTreeMap<StepId, StepStatistics>,
    cascaded:     Vec<StepId>,
    reason:       &str,
    mut seq:      JournalSequence,
) -> Result<JournalSequence, ExecutionError> {
    for skipped_id in cascaded {
        {
            let mut set = skipped_set.lock().expect("poisoned");
            if !set.insert(skipped_id.clone()) {
                continue;
            }
        }
        let entry = step_stats.entry(skipped_id.clone()).or_default();
        entry.skipped = entry.skipped.saturating_add(1);
        seq = seq.next();
        journal
            .write(JournalEvent {
                sequence:     seq,
                execution_id,
                timestamp:    Timestamp::now(),
                kind:         JournalEventKind::StepSkipped {
                    step_id: skipped_id,
                    reason:  reason.to_owned(),
                },
            })
            .await?;
    }
    Ok(seq)
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

#[allow(clippy::too_many_arguments, reason = "cancellation path threads the full run context")]
async fn finalise_cancelled(
    exec:                 &DefaultExecutor,
    execution_id:         ExecutionId,
    started_at:           Timestamp,
    t0:                   Instant,
    step_stats:           BTreeMap<StepId, StepStatistics>,
    mut seq:              JournalSequence,
    reason:               &str,
    aggregated_results:   Vec<TrialResult>,
    partial_run_behavior: PartialRunBehavior,
) -> Result<ExecutionResults, ExecutionError> {
    let completed_at = Timestamp::now();
    let duration = t0.elapsed();
    exec.orchestrator.shutdown().await?;
    write_completed(&*exec.journal, execution_id, &mut seq, duration, false).await?;
    // `PartialRunBehavior::FailRun` discards any trial results
    // captured before cancellation; `RetainResults` (default) hands
    // them off via `on_execution_completed`.
    let trial_results = match partial_run_behavior {
        PartialRunBehavior::RetainResults => aggregated_results,
        PartialRunBehavior::FailRun       => Vec::new(),
    };
    let results = ExecutionResults {
        execution_id,
        status: ExecutionStatus::Cancelled,
        started_at,
        completed_at,
        duration,
        step_stats,
        trial_results,
    };
    exec.observer.on_execution_completed(&execution_id, &results);
    *exec.running.lock().expect("poisoned") = None;
    Err(ExecutionError::Cancelled {
        reason: reason.to_owned(),
    })
}
