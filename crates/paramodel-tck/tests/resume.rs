// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `Executor::resume` — load a checkpoint
//! and continue execution from where it left off.
//!
//! The tests run a plan partially (pausing after a known number of
//! steps via `checkpoint_now`), then build a fresh executor and
//! resume from the saved checkpoint. The expectation is that the
//! resumed run re-executes only the remaining steps.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, ElementRuntime, Fingerprint, LabelValue, Labels,
    LiveStatusSummary, MaterializationOutputs, OperationalState, ResolvedConfiguration,
    StateObservation, StateTransitionListener, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionError,
    ExecutionStatus, Executor, InMemoryJournalWriter, InMemoryStateManager,
    JournalEventKind, NoopObserver,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader, StepId,
    TestPlanId, CheckpointId,
};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// A runtime that increments a shared counter on each `materialize`.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct CountingRuntime {
    calls: Arc<AtomicU32>,
}

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for CountingRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(MaterializationOutputs::new())
    }
    async fn dematerialize(&self) -> paramodel_elements::Result<()> {
        Ok(())
    }
    async fn status_check(&self) -> LiveStatusSummary {
        LiveStatusSummary {
            state:   OperationalState::Ready,
            summary: "ok".to_owned(),
        }
    }
    fn observe_state(
        &self,
        _l: StateTransitionListener,
    ) -> Box<dyn StateObservation> {
        Box::new(NoopObservation)
    }
}

// ---------------------------------------------------------------------------
// Plan helpers.
// ---------------------------------------------------------------------------

fn svc() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn header(id: &str, deps: &[&str]) -> StepHeader {
    StepHeader::builder()
        .id(StepId::new(id).unwrap())
        .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
        .reason("test".to_owned())
        .build()
}

fn deploy(id: &str, element: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header:                header(id, deps),
        element:               ElementName::new(element).unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn chain_plan(steps: Vec<AtomicStep>) -> ExecutionPlan {
    let _ = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc())
        .build();
    ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .source_plan_fingerprint(Fingerprint::of(b"src"))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .execution_graph(ExecutionGraph::new(steps).unwrap())
        .element_instance_graph(ElementInstanceGraph::default())
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(0))
                .compiler_version("test".to_owned())
                .optimization_level(OptimizationStrategy::Basic)
                .trial_count(0)
                .step_count(0)
                .barrier_count(0)
                .element_instance_count(0)
                .performance_metrics(PerformanceMetrics {
                    critical_path_duration: None,
                    total_duration:         None,
                    maximum_parallelism:    1,
                    average_parallelism:    1.0,
                    speedup_factor:         1.0,
                })
                .build(),
        )
        .build()
}

fn counting_runtimes(
    calls: Arc<AtomicU32>,
) -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(
        ElementName::new("db").unwrap(),
        Arc::new(CountingRuntime { calls }) as Arc<dyn ElementRuntime>,
    );
    m
}

fn fresh_executor(
    runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>>,
    journal:  Arc<InMemoryJournalWriter>,
    state:    Arc<InMemoryStateManager>,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        state,
        journal,
        Arc::new(NoopObserver),
    )
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Resume with a checkpoint id that isn't known returns `PlanMismatch`.
#[tokio::test]
async fn resume_with_unknown_checkpoint_errors() {
    let plan = chain_plan(vec![deploy("a", "db", &[])]);
    let state = Arc::new(InMemoryStateManager::new());
    let calls = Arc::new(AtomicU32::new(0));
    let exec = fresh_executor(
        counting_runtimes(calls),
        Arc::new(InMemoryJournalWriter::new()),
        state,
    );

    let unknown = CheckpointId::new("never-saved").unwrap();
    let err = exec.resume(&plan, &unknown).await.unwrap_err();
    assert!(matches!(err, ExecutionError::PlanMismatch { .. }));
}

/// Resume against a plan whose fingerprint doesn't match the one in
/// the checkpoint rejects cleanly.
#[tokio::test]
async fn resume_rejects_plan_fingerprint_mismatch() {
    // Run plan A, checkpoint mid-flight, then attempt to resume with
    // a different plan B.
    let state = Arc::new(InMemoryStateManager::new());
    let plan_a = chain_plan(vec![deploy("a", "db", &[])]);
    let calls_a = Arc::new(AtomicU32::new(0));
    let exec_a = fresh_executor(
        counting_runtimes(calls_a),
        Arc::new(InMemoryJournalWriter::new()),
        state.clone(),
    );
    let _ = exec_a.execute(&plan_a).await.expect("runs");
    let checkpoint = exec_a.checkpoint_now().await.unwrap();

    // Plan B has a different shape → different fingerprint.
    let plan_b = chain_plan(vec![deploy("a", "db", &[]), deploy("b", "db", &[])]);
    let calls_b = Arc::new(AtomicU32::new(0));
    let exec_b = fresh_executor(
        counting_runtimes(calls_b),
        Arc::new(InMemoryJournalWriter::new()),
        state.clone(),
    );
    let err = exec_b.resume(&plan_b, &checkpoint.id).await.unwrap_err();
    assert!(matches!(err, ExecutionError::PlanMismatch { .. }));
}

/// Full lifecycle: run → checkpoint → resume. The resumed run does
/// not re-execute already-completed steps and produces an
/// `ExecutionResumed` journal event keyed on the checkpoint id.
#[tokio::test]
async fn resume_from_checkpoint_does_not_rerun_completed_steps() {
    let state = Arc::new(InMemoryStateManager::new());
    let plan = chain_plan(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &["a"]),
        deploy("c", "db", &["b"]),
    ]);

    // First run: execute the whole plan so all steps are completed.
    // Then checkpoint — the checkpoint captures the terminal state.
    let calls1 = Arc::new(AtomicU32::new(0));
    let journal1 = Arc::new(InMemoryJournalWriter::new());
    let exec1 = fresh_executor(
        counting_runtimes(calls1.clone()),
        journal1.clone(),
        state.clone(),
    );
    let r1 = exec1.execute(&plan).await.expect("runs");
    assert_eq!(r1.status, ExecutionStatus::Succeeded);
    assert_eq!(calls1.load(Ordering::SeqCst), 3);
    let checkpoint = exec1.checkpoint_now().await.unwrap();
    assert!(checkpoint.is_valid);

    // Second run: resume from that checkpoint on a fresh executor
    // with a fresh counting runtime. The resumed run should not call
    // materialize again — all three steps are already completed.
    let calls2 = Arc::new(AtomicU32::new(0));
    let journal2 = Arc::new(InMemoryJournalWriter::new());
    let exec2 = fresh_executor(
        counting_runtimes(calls2.clone()),
        journal2.clone(),
        state.clone(),
    );
    let r2 = exec2.resume(&plan, &checkpoint.id).await.expect("resumes");
    assert_eq!(
        calls2.load(Ordering::SeqCst),
        0,
        "resumed run must not re-materialize already-completed steps",
    );
    assert_eq!(r2.status, ExecutionStatus::Succeeded);
    // The resumed execution id matches the checkpoint's execution_id,
    // which in this test is nil (checkpoint_now() was called after
    // the initial execute() finished and cleared running state).
    // The hand-crafted checkpoint test below exercises the non-nil
    // case explicitly.
    let _ = r1;

    // Journal must include `ExecutionResumed { checkpoint_id }`.
    let events = journal2.snapshot();
    let resumed = events.iter().any(|e| {
        matches!(
            &e.kind,
            JournalEventKind::ExecutionResumed { checkpoint_id } if *checkpoint_id == checkpoint.id,
        )
    });
    assert!(resumed, "ExecutionResumed event must be present");

    // step_stats reflects the restored completed counters (all three
    // steps show completed=1 via the seed).
    for sid in ["a", "b", "c"] {
        let s = r2.step_stats.get(&StepId::new(sid).unwrap()).unwrap();
        assert_eq!(s.completed, 1, "{sid} completed count from checkpoint seed");
    }
}

/// When a checkpoint captures a partially completed run, resume
/// continues from where it left off: completed steps are not re-run,
/// but the remaining steps execute to completion.
#[tokio::test]
async fn resume_continues_partial_run_to_completion() {
    // We simulate a partial run by hand-constructing a CheckpointState
    // that marks step `a` as completed and leaves `b`, `c` pending.
    // Then we resume.
    use paramodel_executor::{Checkpoint, CheckpointState, ExecutionStateManager, ExecutionId, JournalSequence};
    use std::collections::BTreeSet;

    let state = Arc::new(InMemoryStateManager::new());
    let plan = chain_plan(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &["a"]),
        deploy("c", "db", &["b"]),
    ]);

    // Hand-built checkpoint: `a` completed, others pending.
    let execution_id = ExecutionId::from_ulid(Ulid::from_parts(9, 9));
    let plan_fp = plan.fingerprint();
    let mut completed = BTreeSet::new();
    completed.insert(StepId::new("a").unwrap());
    let snap = CheckpointState {
        completed_step_ids:  completed,
        journal_sequence:    JournalSequence::ZERO,
        ..Default::default()
    };
    let snap_bytes = serde_json::to_vec(&snap).unwrap();
    let snap_fp = Fingerprint::of(&snap_bytes);
    let cp_id = CheckpointId::new("seed").unwrap();
    let checkpoint = Checkpoint {
        id:               cp_id.clone(),
        execution_id,
        plan_fingerprint: plan_fp,
        created_at:       Timestamp::now(),
        state:            snap,
        fingerprint:      snap_fp,
    };
    state.checkpoint(&checkpoint).await;

    let calls = Arc::new(AtomicU32::new(0));
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(
        counting_runtimes(calls.clone()),
        journal,
        state.clone(),
    );
    let r = exec.resume(&plan, &cp_id).await.expect("resume succeeds");

    // `a` was already done → not re-materialized. `b` and `c` run
    // for the first time.
    assert_eq!(calls.load(Ordering::SeqCst), 2, "only b and c materialize");
    assert_eq!(r.status, ExecutionStatus::Succeeded);
    assert_eq!(r.execution_id, execution_id);

    // a: completed=1 from seed; b, c: completed=1 from the resumed
    // scheduling.
    for sid in ["a", "b", "c"] {
        let s = r.step_stats.get(&StepId::new(sid).unwrap()).unwrap();
        assert_eq!(s.completed, 1, "{sid} completed count");
    }
}
