// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! State-machine coverage for the default scheduler — every
//! transition that `Scheduler::mark_*` surfaces is exercised and
//! the resulting `state()` / `is_complete()` / `statistics()`
//! values are asserted.

use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, Fingerprint, LabelValue, Labels, ResolvedConfiguration,
    attributes::label,
};
use paramodel_executor::{DefaultScheduler, ResourceSnapshot, Scheduler};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader,
    StepId, TestPlanId,
};
use paramodel_trials::ErrorInfo;
use ulid::Ulid;

fn svc() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn deploy(id: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new(id).unwrap())
            .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
            .reason("test".to_owned())
            .build(),
        element:               ElementName::new("db").unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn plan_of(steps: Vec<AtomicStep>) -> ExecutionPlan {
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

fn sid(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn initialise_sets_ready_to_roots_only() {
    let sched = DefaultScheduler::new();
    // a, b, c where c depends on a,b.
    let p = plan_of(vec![deploy("a", &[]), deploy("b", &[]), deploy("c", &["a", "b"])]);
    sched.initialise(&p).await.unwrap();
    let state = sched.state();
    assert_eq!(state.pending_count, 3);
    assert_eq!(state.ready_count, 2);
    assert_eq!(state.running_count, 0);
}

#[tokio::test]
async fn next_steps_respects_max_count() {
    let sched = DefaultScheduler::new();
    let p = plan_of(vec![deploy("a", &[]), deploy("b", &[]), deploy("c", &[])]);
    sched.initialise(&p).await.unwrap();
    let picked = sched.next_steps(1, &ResourceSnapshot::default()).await;
    assert_eq!(picked.len(), 1);
    let state = sched.state();
    assert_eq!(state.running_count, 1);
    assert_eq!(state.ready_count, 2);
}

#[tokio::test]
async fn mark_completed_promotes_dependents() {
    let sched = DefaultScheduler::new();
    let p = plan_of(vec![deploy("a", &[]), deploy("b", &["a"])]);
    sched.initialise(&p).await.unwrap();
    let picked = sched.next_steps(1, &ResourceSnapshot::default()).await;
    assert_eq!(picked[0], sid("a"));
    sched.mark_completed(&sid("a"), ts()).await;
    let state = sched.state();
    assert_eq!(state.completed_count, 1);
    assert_eq!(state.ready_count, 1, "b should be ready once a completes");
}

#[tokio::test]
async fn mark_failed_cascades_dependents_to_skipped() {
    let sched = DefaultScheduler::new();
    // a → b → c. Fail a.
    let p = plan_of(vec![
        deploy("a", &[]),
        deploy("b", &["a"]),
        deploy("c", &["b"]),
    ]);
    sched.initialise(&p).await.unwrap();
    let _ = sched.next_steps(1, &ResourceSnapshot::default()).await;
    sched
        .mark_failed(
            &sid("a"),
            ts(),
            &ErrorInfo::builder()
                .kind("K".to_owned())
                .message("m".to_owned())
                .build(),
        )
        .await;
    let state = sched.state();
    assert_eq!(state.failed_count, 1);
    assert_eq!(state.skipped_count, 2, "b and c should cascade-skip");
}

#[tokio::test]
async fn mark_skipped_promotes_dependents() {
    let sched = DefaultScheduler::new();
    let p = plan_of(vec![deploy("a", &[]), deploy("b", &["a"])]);
    sched.initialise(&p).await.unwrap();
    let _ = sched.next_steps(1, &ResourceSnapshot::default()).await;
    sched.mark_skipped(&sid("a"), ts(), "manual").await;
    let state = sched.state();
    assert_eq!(state.skipped_count, 1);
    assert_eq!(state.ready_count, 1, "b becomes ready after a skips");
}

#[tokio::test]
async fn is_complete_after_all_terminal() {
    let sched = DefaultScheduler::new();
    let p = plan_of(vec![deploy("a", &[])]);
    sched.initialise(&p).await.unwrap();
    assert!(!sched.is_complete());
    let _ = sched.next_steps(1, &ResourceSnapshot::default()).await;
    sched.mark_completed(&sid("a"), ts()).await;
    assert!(sched.is_complete());
}

#[tokio::test]
async fn statistics_accumulate_across_transitions() {
    let sched = DefaultScheduler::new();
    let p = plan_of(vec![deploy("a", &[]), deploy("b", &[]), deploy("c", &[])]);
    sched.initialise(&p).await.unwrap();
    let _ = sched.next_steps(3, &ResourceSnapshot::default()).await;
    sched.mark_completed(&sid("a"), ts()).await;
    sched
        .mark_failed(
            &sid("b"),
            ts(),
            &ErrorInfo::builder()
                .kind("K".to_owned())
                .message("m".to_owned())
                .build(),
        )
        .await;
    sched.mark_skipped(&sid("c"), ts(), "r").await;
    let stats = sched.statistics();
    assert_eq!(stats.total_dispatched, 3);
    assert_eq!(stats.total_completed, 1);
    assert_eq!(stats.total_failed, 1);
    assert_eq!(stats.total_skipped, 1);
}
