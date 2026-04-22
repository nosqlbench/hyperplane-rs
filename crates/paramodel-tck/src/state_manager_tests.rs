// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::ExecutionStateManager`].
//!
//! Reference implementations that don't persist (like
//! [`paramodel_executor::NoopStateManager`]) still have a contract
//! to honour: recovery returns a `Default`, reads are graceful,
//! writes don't error.

use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, Fingerprint, LabelValue, Labels, attributes::label,
};
use paramodel_executor::{
    ExecutionId, ExecutionStateManager, IdempotencyClass, JournalEvent, JournalEventKind,
    JournalSequence,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader,
    StepId, TestPlanId,
};
use paramodel_trials::{ExecutionTiming, TrialResult, TrialStatus};
use ulid::Ulid;

use crate::providers::ExecutionStateManagerProvider;

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn empty_plan() -> ExecutionPlan {
    let _ = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc_labels())
        .build();
    ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .source_plan_fingerprint(Fingerprint::of(b"src"))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .execution_graph(ExecutionGraph::default())
        .element_instance_graph(ElementInstanceGraph::default())
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(0))
                .compiler_version("tck".to_owned())
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

fn fresh_exec() -> ExecutionId {
    ExecutionId::from_ulid(Ulid::new())
}

fn sample_event(exec: ExecutionId, seq: u64) -> JournalEvent {
    JournalEvent {
        sequence:     JournalSequence::new(seq),
        execution_id: exec,
        timestamp:    Timestamp::now(),
        kind:         JournalEventKind::ExecutionCompleted {
            success:  true,
            duration: Duration::from_millis(seq),
        },
    }
}

/// `record_event` + `checkpoint` never error.
pub async fn tck_state_manager_record_and_checkpoint_infallible<P>(provider: &P)
where
    P: ExecutionStateManagerProvider,
{
    let m = provider.fresh();
    let exec = fresh_exec();
    m.record_event(sample_event(exec, 1)).await;
    // Checkpoint with a trivial snapshot.
    let snap = paramodel_executor::Checkpoint {
        id:               paramodel_plan::CheckpointId::new("cp1").unwrap(),
        execution_id:     exec,
        plan_fingerprint: Fingerprint::of(b"plan"),
        created_at:       Timestamp::now(),
        state:            paramodel_executor::CheckpointState::default(),
        fingerprint:      Fingerprint::of(b"snap"),
    };
    m.checkpoint(&snap).await;
}

/// `recover` on a fresh manager returns a `Default` recovery result
/// — no completed/failed/skipped steps, no trials.
pub async fn tck_state_manager_recover_returns_default<P>(provider: &P)
where
    P: ExecutionStateManagerProvider,
{
    let m = provider.fresh();
    let rr = m.recover(&fresh_exec(), &empty_plan()).await;
    assert!(rr.completed_step_ids.is_empty());
    assert!(rr.failed_step_ids.is_empty());
    assert!(rr.skipped_step_ids.is_empty());
    assert!(rr.in_flight_step_ids.is_empty());
    assert!(rr.completed_trial_ids.is_empty());
    assert!(rr.in_flight_trial_ids.is_empty());
}

/// `is_step_completed` returns false for every step on a fresh
/// manager.
pub async fn tck_state_manager_step_not_completed<P>(provider: &P)
where
    P: ExecutionStateManagerProvider,
{
    let m = provider.fresh();
    assert!(
        !m.is_step_completed(&fresh_exec(), &StepId::new("any").unwrap())
            .await
    );
}

/// `record_suspension` + `save_trial_result` + `cleanup` form a
/// valid sequence on a fresh manager with no errors.
pub async fn tck_state_manager_lifecycle_infallible<P>(provider: &P)
where
    P: ExecutionStateManagerProvider,
{
    use paramodel_elements::TrialId;

    let m = provider.fresh();
    let exec = fresh_exec();
    m.record_suspension(&exec, "user paused").await;
    let r = TrialResult::builder()
        .trial_id(TrialId::from_ulid(Ulid::new()))
        .status(TrialStatus::Completed)
        .attempt(1)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(0).unwrap(),
                Timestamp::from_second(1).unwrap(),
            )
            .unwrap(),
        )
        .build();
    m.save_trial_result(&exec, &r).await;
    m.cleanup(&exec).await;
}

/// `idempotency_class` is a pure function of the step variant.
pub fn tck_state_manager_idempotency_class_by_variant<P>(provider: &P)
where
    P: ExecutionStateManagerProvider,
{
    use paramodel_elements::ResolvedConfiguration;
    use paramodel_plan::{BarrierId, BarrierKind, CheckpointId, TimeoutAction};

    let m = provider.fresh();
    let deploy = AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new("d").unwrap())
            .reason("t".to_owned())
            .build(),
        element:               ElementName::new("db").unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    };
    let teardown = AtomicStep::Teardown {
        header:            StepHeader::builder()
            .id(StepId::new("t").unwrap())
            .reason("t".to_owned())
            .build(),
        element:           ElementName::new("db").unwrap(),
        instance_number:   0,
        collect_artifacts: false,
    };
    let barrier = AtomicStep::Barrier {
        header:         StepHeader::builder()
            .id(StepId::new("b").unwrap())
            .reason("t".to_owned())
            .build(),
        barrier_id:     BarrierId::new("b").unwrap(),
        barrier_kind:   BarrierKind::ElementReady,
        timeout:        None,
        timeout_action: TimeoutAction::FailFast,
    };
    let checkpoint = AtomicStep::Checkpoint {
        header:        StepHeader::builder()
            .id(StepId::new("cp").unwrap())
            .reason("t".to_owned())
            .build(),
        checkpoint_id: CheckpointId::new("cp").unwrap(),
    };

    // Deploy / SaveOutput / Await are idempotent per SRD-0011.
    assert!(matches!(
        m.idempotency_class(&deploy),
        IdempotencyClass::Idempotent,
    ));
    // Teardown / Barrier / Checkpoint / TrialStart / TrialEnd are not.
    assert!(matches!(
        m.idempotency_class(&teardown),
        IdempotencyClass::NonIdempotent,
    ));
    assert!(matches!(
        m.idempotency_class(&barrier),
        IdempotencyClass::NonIdempotent,
    ));
    assert!(matches!(
        m.idempotency_class(&checkpoint),
        IdempotencyClass::NonIdempotent,
    ));
}
