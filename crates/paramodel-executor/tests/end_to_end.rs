// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end test: compile a trivial plan, execute it via
//! `DefaultExecutor`, and inspect the resulting journal.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Element, ElementName, ElementRuntime, LabelValue, Labels, LiveStatusSummary,
    MaterializationOutputs, OperationalState, ResolvedConfiguration, StateObservation,
    StateTransitionListener, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionStatus, Executor,
    InMemoryJournalWriter, JournalEventKind, NoopObserver, NoopStateManager,
};
use paramodel_plan::{PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use ulid::Ulid;

#[derive(Debug, Default)]
struct CountingRuntime {
    materialize_calls:   AtomicUsize,
    dematerialize_calls: AtomicUsize,
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
        _resolved: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        self.materialize_calls.fetch_add(1, Ordering::SeqCst);
        Ok(MaterializationOutputs::new())
    }

    async fn dematerialize(&self) -> paramodel_elements::Result<()> {
        self.dematerialize_calls.fetch_add(1, Ordering::SeqCst);
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
        _listener: StateTransitionListener,
    ) -> Box<dyn StateObservation> {
        Box::new(NoopObservation)
    }
}

fn service_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn minimal_test_plan() -> TestPlan {
    let db = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(service_labels())
        .build();
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("trivial").unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

#[tokio::test]
async fn trivial_plan_executes_end_to_end() {
    let plan = minimal_test_plan();
    let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");

    let runtime = Arc::new(CountingRuntime::default());
    let mut runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    runtimes.insert(
        ElementName::new("db").unwrap(),
        runtime.clone() as Arc<dyn ElementRuntime>,
    );

    let journal = Arc::new(InMemoryJournalWriter::new());
    let executor = DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        journal.clone(),
        Arc::new(NoopObserver),
    );

    let results = executor.execute(&compiled).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);

    assert_eq!(runtime.materialize_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.dematerialize_calls.load(Ordering::SeqCst), 1);

    let events = journal.snapshot();
    // ExecutionStarted + (StepStarted + StepCompleted) * 4 +
    // CheckpointCreated * 2 (one per compiler-emitted Checkpoint
    // step) + ExecutionCompleted = 1 + 8 + 2 + 1 = 12.
    assert_eq!(events.len(), 12);

    let has_started = events
        .iter()
        .any(|e| matches!(e.kind, JournalEventKind::ExecutionStarted { .. }));
    let has_completed = events
        .iter()
        .any(|e| matches!(e.kind, JournalEventKind::ExecutionCompleted { success: true, .. }));
    assert!(has_started);
    assert!(has_completed);

    // Sequences are monotonic.
    let mut prev = 0u64;
    for e in &events {
        assert!(e.sequence.get() > prev);
        prev = e.sequence.get();
    }
}
