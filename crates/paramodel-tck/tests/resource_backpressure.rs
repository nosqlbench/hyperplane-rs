// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `ResourceManager`-driven back-pressure.
//!
//! With a bounded [`DefaultResourceManager`] attached, the executor
//! caps concurrent in-flight steps by *resource capacity* in addition
//! to `max_parallel`. Scheduler-level filtering holds back steps whose
//! declared requirements don't fit; executor-level allocate/release
//! ties resource lifecycle to step lifecycle.

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
    DefaultExecutor, DefaultOrchestrator, DefaultResourceManager, DefaultScheduler,
    ExecutionStatus, Executor, InMemoryJournalWriter, NoopObserver, NoopStateManager,
    ResourceCapacity, ResourceManager, ResourceRequest,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics,
    ResourceRequirements, StepHeader, StepId, TestPlanId,
};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// A sleeping runtime that tracks max concurrent materialize calls.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SlowRuntime {
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
impl ElementRuntime for SlowRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        let now_in_flight = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
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

fn header_with_cpu(id: &str, deps: &[&str], cpu: f64) -> StepHeader {
    let req = ResourceRequirements::builder().peak_cpu_cores(cpu).build();
    StepHeader::builder()
        .id(StepId::new(id).unwrap())
        .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
        .reason("test".to_owned())
        .resource_requirements(req)
        .build()
}

fn deploy_with_cpu(id: &str, element: &str, deps: &[&str], cpu: f64) -> AtomicStep {
    AtomicStep::Deploy {
        header:                header_with_cpu(id, deps, cpu),
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

fn slow_runtimes(
    sleep_ms:       u64,
    concurrent:     Arc<AtomicU32>,
    max_concurrent: Arc<AtomicU32>,
) -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(
        ElementName::new("db").unwrap(),
        Arc::new(SlowRuntime {
            sleep_ms,
            concurrent,
            max_concurrent,
        }) as Arc<dyn ElementRuntime>,
    );
    m
}

fn fresh_executor(
    runtimes:        BTreeMap<ElementName, Arc<dyn ElementRuntime>>,
    max_parallel:    usize,
    resource_manager: Arc<dyn ResourceManager>,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        Arc::new(InMemoryJournalWriter::new()),
        Arc::new(NoopObserver),
    )
    .with_parallelism(max_parallel)
    .with_resource_manager(resource_manager)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// With 4 independent steps each asking for 2 CPU, a 4-CPU pool, and
/// `max_parallel == 4`, the executor admits at most 2 concurrently —
/// CPU capacity, not `max_parallel`, is the binding constraint.
#[tokio::test]
async fn bounded_cpu_pool_limits_concurrency_below_max_parallel() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy_with_cpu("a", "db", &[], 2.0),
        deploy_with_cpu("b", "db", &[], 2.0),
        deploy_with_cpu("c", "db", &[], 2.0),
        deploy_with_cpu("d", "db", &[], 2.0),
    ]);
    let rm = Arc::new(DefaultResourceManager::with_capacity(ResourceCapacity {
        cpu_cores: 4.0,
        ..Default::default()
    }));
    let exec = fresh_executor(
        slow_runtimes(50, concurrent, max_concurrent.clone()),
        4,
        rm,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    let observed = max_concurrent.load(Ordering::SeqCst);
    assert!(
        observed <= 2,
        "4 × 2-CPU steps on a 4-CPU pool must not overlap 3-way; got {observed}",
    );
    assert!(
        observed >= 2,
        "pool allows 2 overlaps; observed {observed}",
    );
}

/// All four steps complete despite capacity smaller than their total
/// ask — capacity releases as each step finishes, unblocking the
/// next wave.
#[tokio::test]
async fn bounded_pool_releases_capacity_between_waves() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let plan = plan_of(vec![
        deploy_with_cpu("a", "db", &[], 3.0),
        deploy_with_cpu("b", "db", &[], 3.0),
        deploy_with_cpu("c", "db", &[], 3.0),
        deploy_with_cpu("d", "db", &[], 3.0),
    ]);
    // Only room for one 3-CPU step at a time.
    let rm = Arc::new(DefaultResourceManager::with_capacity(ResourceCapacity {
        cpu_cores: 3.0,
        ..Default::default()
    }));
    let exec = fresh_executor(
        slow_runtimes(20, concurrent, max_concurrent.clone()),
        4,
        rm,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "3-CPU pool can only host one 3-CPU step at a time",
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

/// Custom (adopter-defined) resources are enforced identically to
/// the built-in ones. Two steps each asking for `{ "gpu": 1 }` share
/// a 1-GPU pool and never overlap.
#[tokio::test]
async fn custom_resource_kind_is_honoured() {
    let concurrent = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let mut gpu_req = ResourceRequirements::builder().build();
    gpu_req.custom.insert("gpu".to_owned(), 1);
    let header_a = StepHeader::builder()
        .id(StepId::new("a").unwrap())
        .depends_on(Vec::new())
        .reason("t".to_owned())
        .resource_requirements(gpu_req.clone())
        .build();
    let header_b = StepHeader::builder()
        .id(StepId::new("b").unwrap())
        .depends_on(Vec::new())
        .reason("t".to_owned())
        .resource_requirements(gpu_req)
        .build();
    let plan = plan_of(vec![
        AtomicStep::Deploy {
            header:                header_a,
            element:               ElementName::new("db").unwrap(),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        },
        AtomicStep::Deploy {
            header:                header_b,
            element:               ElementName::new("db").unwrap(),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        },
    ]);
    let mut pool_custom = BTreeMap::new();
    pool_custom.insert("gpu".to_owned(), 1u64);
    let rm = Arc::new(DefaultResourceManager::with_capacity(ResourceCapacity {
        custom: pool_custom,
        ..Default::default()
    }));
    let exec = fresh_executor(
        slow_runtimes(30, concurrent, max_concurrent.clone()),
        4,
        rm,
    );
    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "single-GPU pool must serialize 1-GPU steps",
    );
}

/// Regression: the bounded manager's `available` snapshot decreases
/// when an allocation is held and recovers after release. This tests
/// the ResourceManager surface directly, independent of the executor.
#[tokio::test]
async fn bounded_manager_snapshot_reflects_held_allocations() {
    let rm = DefaultResourceManager::with_capacity(ResourceCapacity {
        cpu_cores: 8.0,
        memory_mb: 4096,
        ..Default::default()
    });
    let snap = rm.available();
    assert!((snap.cpu_cores_free - 8.0).abs() < 1e-6);
    assert_eq!(snap.memory_mb_free, 4096);

    let a = rm
        .allocate(&ResourceRequest {
            cpu_cores: 3.0,
            memory_mb: 1024,
            ..Default::default()
        })
        .await
        .unwrap();
    let snap = rm.available();
    assert!((snap.cpu_cores_free - 5.0).abs() < 1e-6);
    assert_eq!(snap.memory_mb_free, 3072);

    rm.release(a).await;
    let snap = rm.available();
    assert!((snap.cpu_cores_free - 8.0).abs() < 1e-6);
    assert_eq!(snap.memory_mb_free, 4096);

    // Over-asking fails with Insufficient.
    let err = rm
        .allocate(&ResourceRequest {
            cpu_cores: 16.0,
            ..Default::default()
        })
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("insufficient"), "expected insufficient error, got {msg}");
}
