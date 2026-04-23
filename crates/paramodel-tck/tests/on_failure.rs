// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `ExecutionPolicies::on_failure`.
//!
//! Exercises the integrated path: `ExecutionPlan` carries policies →
//! executor reads `plan.policies.on_failure` → runtime fails a step →
//! scheduler cascades skips to dependents → executor emits
//! `StepSkipped` journal events for each, or halts on `Stop`.

use std::collections::BTreeMap;
use std::sync::Arc;
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
    ExecutionStatus, Executor, InMemoryJournalWriter, JournalEventKind,
    NoopObserver, NoopStateManager,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, ExecutionPolicies, OnFailure, OptimizationStrategy,
    PerformanceMetrics, StepHeader, StepId, TestPlanId,
};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// A runtime whose `materialize` always errors.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct FailingRuntime;

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for FailingRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        Err(paramodel_elements::ElementError::InvalidMaxConcurrency.into())
    }
    async fn dematerialize(&self) -> paramodel_elements::Result<()> {
        Ok(())
    }
    async fn status_check(&self) -> LiveStatusSummary {
        LiveStatusSummary {
            state:   OperationalState::Failed,
            summary: "boom".to_owned(),
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
// Plan-and-runtime helpers.
// ---------------------------------------------------------------------------

fn svc() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn deploy(id: &str, element: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new(id).unwrap())
            .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
            .reason("test".to_owned())
            .build(),
        element:               ElementName::new(element).unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn chain_plan(policies: ExecutionPolicies) -> ExecutionPlan {
    // a → b → c. `a` targets the `failer` element, b+c target `ok`.
    let _ = Element::builder()
        .name(ElementName::new("failer").unwrap())
        .labels(svc())
        .build();
    let _ = Element::builder()
        .name(ElementName::new("ok").unwrap())
        .labels(svc())
        .build();
    ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .source_plan_fingerprint(Fingerprint::of(b"src"))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .execution_graph(
            ExecutionGraph::new(vec![
                deploy("a", "failer", &[]),
                deploy("b", "ok", &["a"]),
                deploy("c", "ok", &["b"]),
            ])
            .unwrap(),
        )
        .element_instance_graph(ElementInstanceGraph::default())
        .policies(policies)
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(0))
                .compiler_version("test".to_owned())
                .optimization_level(OptimizationStrategy::Basic)
                .trial_count(0)
                .step_count(3)
                .barrier_count(0)
                .element_instance_count(2)
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

// Always-succeed runtime for the non-failing element.
#[derive(Debug, Default)]
struct OkRuntime;

#[async_trait]
impl ElementRuntime for OkRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
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

fn runtimes() -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(
        ElementName::new("failer").unwrap(),
        Arc::new(FailingRuntime) as Arc<dyn ElementRuntime>,
    );
    m.insert(
        ElementName::new("ok").unwrap(),
        Arc::new(OkRuntime) as Arc<dyn ElementRuntime>,
    );
    m
}

fn fresh_executor(
    journal: Arc<InMemoryJournalWriter>,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes())),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        journal,
        Arc::new(NoopObserver),
    )
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn on_failure_skip_cascades_to_dependents_with_journal_events() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Skip,
        ..ExecutionPolicies::defaults()
    };
    let plan = chain_plan(policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(journal.clone());

    let results = exec.execute(&plan).await.expect("Skip policy must not Err");
    assert_eq!(
        results.status,
        ExecutionStatus::PartialFailure,
        "at least one step failed but policy is Skip",
    );

    let events = journal.snapshot();

    // Exactly one StepFailed, for `a`.
    let failed: Vec<&StepId> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepFailed { step_id, .. } => Some(step_id),
            _ => None,
        })
        .collect();
    assert_eq!(failed.len(), 1, "one failure for step a");
    assert_eq!(failed[0].as_str(), "a");

    // Both `b` and `c` cascade-skip and produce StepSkipped events.
    let skipped: Vec<&StepId> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepSkipped { step_id, .. } => Some(step_id),
            _ => None,
        })
        .collect();
    let skipped_ids: Vec<&str> = skipped.iter().map(|s| s.as_str()).collect();
    assert!(
        skipped_ids.contains(&"b") && skipped_ids.contains(&"c"),
        "expected b and c cascade-skipped, got {skipped_ids:?}",
    );

    // step_stats reflects the cascade too.
    assert_eq!(results.step_stats.get(&StepId::new("a").unwrap()).unwrap().failed, 1);
    assert_eq!(results.step_stats.get(&StepId::new("b").unwrap()).unwrap().skipped, 1);
    assert_eq!(results.step_stats.get(&StepId::new("c").unwrap()).unwrap().skipped, 1);

    // Execution completed normally (not halted), so we should see an
    // ExecutionCompleted event.
    assert!(events
        .iter()
        .any(|e| matches!(e.kind, JournalEventKind::ExecutionCompleted { .. })));
}

#[tokio::test]
async fn on_failure_stop_halts_without_cascading_skips() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Stop,
        ..ExecutionPolicies::defaults()
    };
    let plan = chain_plan(policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(journal.clone());

    let err = exec.execute(&plan).await.expect_err("Stop must Err");
    match err {
        ExecutionError::StopOnFailure { step } => {
            assert_eq!(step.as_str(), "a");
        }
        other => panic!("expected StopOnFailure, got {other:?}"),
    }

    let events = journal.snapshot();

    // Exactly one StepFailed, for `a`.
    let failed_count = events
        .iter()
        .filter(|e| matches!(e.kind, JournalEventKind::StepFailed { .. }))
        .count();
    assert_eq!(failed_count, 1);

    // No StepSkipped events — Stop halts without cascading.
    let skipped_count = events
        .iter()
        .filter(|e| matches!(e.kind, JournalEventKind::StepSkipped { .. }))
        .count();
    assert_eq!(skipped_count, 0, "Stop policy must not cascade-skip");
}

#[tokio::test]
async fn on_failure_skip_reason_references_failed_step_id() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Skip,
        ..ExecutionPolicies::defaults()
    };
    let plan = chain_plan(policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(journal.clone());
    let _ = exec.execute(&plan).await.expect("runs");

    let events = journal.snapshot();
    for e in &events {
        if let JournalEventKind::StepSkipped { reason, .. } = &e.kind {
            assert!(
                reason.contains("a"),
                "skipped reason should reference failed step id, got {reason:?}",
            );
        }
    }
}
