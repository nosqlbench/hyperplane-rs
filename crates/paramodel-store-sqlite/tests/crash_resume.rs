// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end crash + resume against a file-backed `SqliteStore`.
//!
//! The test:
//! 1. Writes a multi-step plan.
//! 2. Executes it with the SqliteStore wired as both
//!    `JournalWriter` and `ExecutionStateManager`.
//! 3. Drops the executor + store, keeping only the file path.
//! 4. Reopens the store from the file + constructs a fresh executor.
//! 5. Resumes from a checkpoint. Asserts the runtime's materialize
//!    counter did not advance — completed steps are not re-run.

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
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, Executor,
    ExecutionStatus, NoopObserver,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader, StepId,
    TestPlanId,
};
use paramodel_store_sqlite::SqliteStore;
use tempfile::tempdir;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Counting runtime — records how many times `materialize` ran.
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
                deploy("c", "db", &["b"]),
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
                .step_count(3)
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

fn runtimes(
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
    store: Arc<SqliteStore>,
    calls: Arc<AtomicU32>,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes(calls))),
        Arc::new(DefaultScheduler::new()),
        store.clone(),
        store,
        Arc::new(NoopObserver),
    )
}

/// End-to-end: run plan → checkpoint → drop → reopen same file →
/// resume. Second run must not re-materialize any step.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_resumes_against_sqlite_file() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("run.db");
    let plan = plan();

    // --- Run 1: execute to completion, then take a checkpoint. ---
    let calls1 = Arc::new(AtomicU32::new(0));
    let cp_handle = {
        let store = Arc::new(SqliteStore::open(&path).await.unwrap());
        let exec = fresh_executor(store.clone(), calls1.clone());
        let r = exec.execute(&plan).await.expect("runs");
        assert_eq!(r.status, ExecutionStatus::Succeeded);
        assert_eq!(calls1.load(Ordering::SeqCst), 3);
        // Checkpoint captures completed-step state — the executor
        // uses its own local set, not the store's journal, so we
        // take a checkpoint here while still in the executor.
        exec.checkpoint_now().await.expect("checkpoint")
    };
    // Executor + store dropped at end of scope.

    // --- Run 2: reopen, resume from saved checkpoint. ---
    let calls2 = Arc::new(AtomicU32::new(0));
    let store = Arc::new(SqliteStore::open(&path).await.unwrap());
    let exec = fresh_executor(store.clone(), calls2.clone());
    let r = exec.resume(&plan, &cp_handle.id).await.expect("resumes");
    assert_eq!(r.status, ExecutionStatus::Succeeded);
    assert_eq!(
        calls2.load(Ordering::SeqCst),
        0,
        "resumed run against file-backed store must not re-materialize",
    );
}
