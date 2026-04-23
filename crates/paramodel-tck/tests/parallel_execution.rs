// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `DefaultExecutor::with_parallelism(n)`.
//!
//! The tests use a runtime that sleeps during `materialize` and
//! records how many materialize calls are in flight simultaneously.
//! They verify:
//!
//! 1. At `max_parallel == 1`, at most one step runs at a time.
//! 2. At `max_parallel >= 2` with independent steps, multiple
//!    materialize calls overlap.
//! 3. Parallel runs still honour the DAG — a step does not begin
//!    before its dependencies finish.
//! 4. Correctness: every step completes; step_stats is accurate.

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
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionStatus, Executor,
    InMemoryJournalWriter, JournalEventKind, NoopObserver, NoopStateManager,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, StepHeader, StepId,
    TestPlanId,
};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// A runtime that sleeps + tracks concurrent materialize calls.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ConcurrencyTrackingRuntime {
    sleep_ms:       u64,
    concurrent:     Arc<AtomicU32>,
    max_concurrent: Arc<AtomicU32>,
}

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for ConcurrencyTrackingRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        let now_in_flight = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        // Bump max_concurrent to `now_in_flight` if larger.
        loop {
            let cur = self.max_concurrent.load(Ordering::SeqCst);
            if now_in_flight <= cur {
                break;
            }
            if self
                .max_concurrent
                .compare_exchange(cur, now_in_flight, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
        self.concurrent.fetch_sub(1, Ordering::SeqCst);
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

fn tracking_runtimes(
    sleep_ms:       u64,
    concurrent:     Arc<AtomicU32>,
    max_concurrent: Arc<AtomicU32>,
) -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(
        ElementName::new("db").unwrap(),
        Arc::new(ConcurrencyTrackingRuntime {
            sleep_ms,
            concurrent,
            max_concurrent,
        }) as Arc<dyn ElementRuntime>,
    );
    m
}

fn fresh_executor(
    runtimes:     BTreeMap<ElementName, Arc<dyn ElementRuntime>>,
    journal:      Arc<InMemoryJournalWriter>,
    max_parallel: usize,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        journal,
        Arc::new(NoopObserver),
    )
    .with_parallelism(max_parallel)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// Baseline: with `max_parallel == 1`, at most one step is in flight
/// at a time. Four independent deploys all pass through the same
/// single-slot pool.
#[tokio::test]
async fn serial_execution_caps_concurrency_at_one() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &[]),
        deploy("c", "db", &[]),
        deploy("d", "db", &[]),
    ]);
    let exec = fresh_executor(
        tracking_runtimes(30, concurrent, max_concurrent.clone()),
        Arc::new(InMemoryJournalWriter::new()),
        1,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "serial mode must cap in-flight at 1",
    );
}

/// With `max_parallel == 4` and four independent deploys, all four
/// materialize calls overlap — `max_concurrent` hits the pool ceiling.
#[tokio::test]
async fn parallel_execution_overlaps_independent_steps() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &[]),
        deploy("c", "db", &[]),
        deploy("d", "db", &[]),
    ]);
    let exec = fresh_executor(
        tracking_runtimes(50, concurrent, max_concurrent.clone()),
        Arc::new(InMemoryJournalWriter::new()),
        4,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    let observed = max_concurrent.load(Ordering::SeqCst);
    assert!(
        observed >= 2,
        "parallel mode must overlap independent steps; max_concurrent = {observed}",
    );
    // All four steps completed exactly once.
    for sid in ["a", "b", "c", "d"] {
        let s = results.step_stats.get(&StepId::new(sid).unwrap()).unwrap();
        assert_eq!(s.completed, 1);
    }
}

/// Dependencies are honoured in parallel mode: `b` depends on `a`, so
/// `b`'s `materialize` never begins before `a`'s `materialize` ends.
/// Even at `max_parallel == 4`, this chain runs serially.
#[tokio::test]
async fn parallel_execution_respects_dependencies() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &["a"]),
        deploy("c", "db", &["b"]),
        deploy("d", "db", &["c"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(
        tracking_runtimes(30, concurrent, max_concurrent.clone()),
        journal.clone(),
        4,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "serial-chain dependency order must prevent overlap even under parallel",
    );

    // Journal ordering: each step's StepCompleted must precede the
    // dependent step's StepStarted.
    let events = journal.snapshot();
    let mut completed_seq: BTreeMap<String, u64> = BTreeMap::new();
    let mut started_seq:   BTreeMap<String, u64> = BTreeMap::new();
    for e in &events {
        match &e.kind {
            JournalEventKind::StepCompleted { step_id, .. } => {
                completed_seq.insert(step_id.as_str().to_owned(), e.sequence.get());
            }
            JournalEventKind::StepStarted { step_id, .. } => {
                // Only record the first StepStarted per step.
                started_seq
                    .entry(step_id.as_str().to_owned())
                    .or_insert(e.sequence.get());
            }
            _ => {}
        }
    }
    for (from, to) in [("a", "b"), ("b", "c"), ("c", "d")] {
        let c = completed_seq.get(from).unwrap();
        let s = started_seq.get(to).unwrap();
        assert!(
            c < s,
            "{from} StepCompleted(seq={c}) must precede {to} StepStarted(seq={s})",
        );
    }
}

/// Mixed DAG: two independent roots `a` and `c`, each with a
/// dependent (`b` after `a`, `d` after `c`). Under `max_parallel ==
/// 2`, the two roots overlap; the dependents overlap with one
/// another after. Overall the plan completes noticeably faster than
/// the 4× single-step cost.
#[tokio::test]
async fn parallel_execution_diamond_dag_completes_correctly() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy("a", "db", &[]),
        deploy("b", "db", &["a"]),
        deploy("c", "db", &[]),
        deploy("d", "db", &["c"]),
    ]);
    let exec = fresh_executor(
        tracking_runtimes(40, concurrent, max_concurrent.clone()),
        Arc::new(InMemoryJournalWriter::new()),
        2,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert!(
        max_concurrent.load(Ordering::SeqCst) >= 2,
        "expected at least two overlapping steps",
    );
    for sid in ["a", "b", "c", "d"] {
        assert_eq!(
            results
                .step_stats
                .get(&StepId::new(sid).unwrap())
                .unwrap()
                .completed,
            1,
        );
    }
}
