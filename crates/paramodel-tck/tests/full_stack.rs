// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end integration exercising the full paramodel stack —
//! compile → execute → journal → persist results → query.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Element, ElementName, ElementRuntime, LabelValue, Labels, LiveStatusSummary,
    MaterializationOutputs, OperationalState, ParameterName, ResolvedConfiguration,
    StateObservation, StateTransitionListener, Value, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionStatus, Executor,
    JournalEventKind, JournalWriter, NoopObserver,
};
use paramodel_persistence::{
    AggregateResult, Aggregation, PageRequest, ResultFilter, ResultStore,
};
use paramodel_plan::{ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use paramodel_tck::{
    JournalWriterProvider, MockJournalWriterProvider, MockResultStoreProvider,
    MockStateManagerProvider, ExecutionStateManagerProvider, ResultStoreProvider,
};
use paramodel_trials::{ExecutionTiming, Metrics, TrialResult, TrialStatus};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Minimal element runtime that always succeeds.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct AlwaysOkRuntime;

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for AlwaysOkRuntime {
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

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn trivial_plan() -> TestPlan {
    let db = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc_labels())
        .build();
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("full").unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

// ---------------------------------------------------------------------------
// End-to-end: compile → execute → journal has expected events.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_stack_compile_then_execute_writes_journal() {
    let plan = trivial_plan();
    let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");

    let journal = Arc::new(MockJournalWriterProvider.fresh());
    let mut runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    runtimes.insert(
        ElementName::new("db").unwrap(),
        Arc::new(AlwaysOkRuntime) as Arc<dyn ElementRuntime>,
    );

    let exec = DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        Arc::new(MockStateManagerProvider.fresh()),
        journal.clone(),
        Arc::new(NoopObserver),
    );

    let results = exec.execute(&compiled).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);

    // Journal must contain start + end events.
    let events = journal.since(None).await.expect("read");
    assert!(!events.is_empty());
    let has_started = events
        .iter()
        .any(|e| matches!(e.kind, JournalEventKind::ExecutionStarted { .. }));
    let has_completed = events.iter().any(|e| {
        matches!(
            e.kind,
            JournalEventKind::ExecutionCompleted { success: true, .. },
        )
    });
    assert!(has_started && has_completed);
}

// ---------------------------------------------------------------------------
// End-to-end: persist TrialResults → query via ResultStore.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_stack_persist_and_query_trial_results() {
    let store = MockResultStoreProvider.fresh();
    let pname = ParameterName::new("latency_ms").unwrap();
    let ename = ElementName::new("db").unwrap();

    // Seed three completed trials with varying latency.
    for (ord, lat) in [(1u64, 100), (2, 200), (3, 300)] {
        let ord_i64 = i64::try_from(ord).unwrap_or(i64::MAX);
        let mut inner = BTreeMap::new();
        inner.insert(pname.clone(), Value::integer(pname.clone(), lat, None));
        let mut outer = BTreeMap::new();
        outer.insert(ename.clone(), inner);
        let r = TrialResult::builder()
            .trial_id(paramodel_elements::TrialId::from_ulid(Ulid::from_parts(
                ord, 1,
            )))
            .status(TrialStatus::Completed)
            .attempt(1)
            .metrics(Metrics::from_map(outer))
            .timing(
                ExecutionTiming::new(
                    Timestamp::from_second(1_700_000_000 + ord_i64).unwrap(),
                    Timestamp::from_second(1_700_000_001 + ord_i64).unwrap(),
                )
                .unwrap(),
            )
            .build();
        store.save(&r).await.unwrap();
    }

    // Count across the full set.
    assert_eq!(store.count(&ResultFilter::Any).await.unwrap(), 3);

    // Average latency via aggregation.
    let avg = store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Avg {
                metric: ElementParameterRef::new(ename.clone(), pname.clone()),
            },
        )
        .await
        .unwrap();
    match avg {
        AggregateResult::Scalar { value } => {
            let v = value.as_double().unwrap();
            assert!((v - 200.0).abs() < 1e-6, "want 200, got {v}");
        }
        other => panic!("expected Scalar, got {other:?}"),
    }

    // Page through.
    let page = store
        .query(&ResultFilter::Any, PageRequest::first(2))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
}
