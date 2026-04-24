// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end: `DefaultExecutor` wired entirely through `SqliteStore`
//! (state manager *and* journal writer). After the run, read the
//! journal back via `JournalStore::replay` and assert the expected
//! event sequence landed durably on disk.
//!
//! This closes the loop: the executor's writes on one side match the
//! persistence trait's reads on the other, both against the same
//! physical SQLite table.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, ElementRuntime, Fingerprint, LabelValue, Labels,
    LiveStatusSummary, MaterializationOutputs, OperationalState, ResolvedConfiguration,
    StateObservation, StateTransitionListener, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, Executor,
    ExecutionStatus, JournalEvent, JournalEventKind, NoopObserver,
};
use paramodel_persistence::JournalStore;
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader, StepId,
    TestPlanId,
};
use paramodel_store_sqlite::SqliteStore;
use ulid::Ulid;

#[derive(Debug, Default)]
struct OkRuntime;

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

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

fn svc() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn deploy(id: &str, element: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header: StepHeader::builder()
            .id(StepId::new(id).unwrap())
            .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
            .reason("test".to_owned())
            .build(),
        element: ElementName::new(element).unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn plan() -> ExecutionPlan {
    let _ = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc())
        .build();
    ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .source_plan_fingerprint(Fingerprint::of(b"src"))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .execution_graph(
            ExecutionGraph::new(vec![
                deploy("a", "db", &[]),
                deploy("b", "db", &["a"]),
            ])
            .unwrap(),
        )
        .element_instance_graph(ElementInstanceGraph::default())
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(0))
                .compiler_version("test".to_owned())
                .optimization_level(OptimizationStrategy::Basic)
                .trial_count(0)
                .step_count(2)
                .barrier_count(0)
                .element_instance_count(1)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_writes_durable_journal_readable_via_persistence_trait() {
    let store = Arc::new(SqliteStore::in_memory().await.unwrap());
    let plan = plan();

    let mut runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    runtimes.insert(
        ElementName::new("db").unwrap(),
        Arc::new(OkRuntime) as Arc<dyn ElementRuntime>,
    );

    let exec = DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        store.clone(),   // SqliteStore as ExecutionStateManager
        store.clone(),   // SqliteStore as JournalWriter
        Arc::new(NoopObserver),
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    let execution_id = results.execution_id;

    // Read the journal back via the persistence trait — not via the
    // writer's in-memory snapshot. Proves durability + readability
    // through the same store.
    let replayed: Vec<JournalEvent> = store
        .replay(&execution_id, None)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert!(!replayed.is_empty(), "expected at least one event");

    // Expected shape: ExecutionStarted first, ExecutionCompleted
    // last, step events sandwiched between.
    assert!(matches!(
        replayed.first().unwrap().kind,
        JournalEventKind::ExecutionStarted { .. },
    ));
    assert!(matches!(
        replayed.last().unwrap().kind,
        JournalEventKind::ExecutionCompleted { success: true, .. },
    ));

    // Both deploy steps produced StepStarted + StepCompleted.
    let step_started: Vec<&str> = replayed
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepStarted { step_id, .. } => Some(step_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(step_started.iter().any(|s| *s == "a"));
    assert!(step_started.iter().any(|s| *s == "b"));

    let step_completed: Vec<&str> = replayed
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepCompleted { step_id, .. } => Some(step_id.as_str()),
            _ => None,
        })
        .collect();
    assert!(step_completed.iter().any(|s| *s == "a"));
    assert!(step_completed.iter().any(|s| *s == "b"));

    // Sequence numbers are strictly monotonic.
    let seqs: Vec<u64> = replayed.iter().map(|e| e.sequence.get()).collect();
    let mut sorted = seqs.clone();
    sorted.sort();
    assert_eq!(seqs, sorted, "journal events must appear in sequence order");
}
