// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `OnFailure::Retry { attempts }` in the
//! executor's per-step retry loop.
//!
//! The tests use a shared-counter runtime that fails its first `N`
//! `materialize` calls, then succeeds. This lets us pin-point how
//! many attempts the executor takes before it converges, or whether
//! it gives up and cascades like `Skip`.

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
    ExecutionPlanMetadata, ExecutionPolicies, OnFailure, OptimizationStrategy,
    PerformanceMetrics, StepHeader, StepId, TestPlanId,
};
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Runtime that fails its first `fail_count` calls, then succeeds.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct FlakyRuntime {
    remaining_failures: AtomicU32,
}

impl FlakyRuntime {
    fn new(fail_count: u32) -> Self {
        Self { remaining_failures: AtomicU32::new(fail_count) }
    }
}

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for FlakyRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        let prev = self.remaining_failures.fetch_sub(1, Ordering::SeqCst);
        if prev > 0 {
            Err(paramodel_elements::ElementError::InvalidMaxConcurrency.into())
        } else {
            // Restore the counter so it doesn't underflow on further
            // calls (not needed for these tests, but tidy).
            self.remaining_failures.store(0, Ordering::SeqCst);
            Ok(MaterializationOutputs::new())
        }
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
// Plan + runtime helpers.
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

fn plan_of(steps: Vec<AtomicStep>, policies: ExecutionPolicies) -> ExecutionPlan {
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
        .policies(policies)
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

fn runtimes_flaky(
    fail_count: u32,
) -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(
        ElementName::new("db").unwrap(),
        Arc::new(FlakyRuntime::new(fail_count)) as Arc<dyn ElementRuntime>,
    );
    m
}

fn fresh_executor(
    runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>>,
    journal:  Arc<InMemoryJournalWriter>,
) -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::new(runtimes)),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        journal,
        Arc::new(NoopObserver),
    )
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

/// A step that fails twice then succeeds, under `Retry { attempts: 3 }`,
/// completes on attempt 3. `step_stats.attempts` must reflect all three
/// tries; the final journal event must be `StepCompleted` with
/// `attempt = 3`.
#[tokio::test]
async fn retry_converges_within_budget() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Retry { attempts: 3 },
        ..ExecutionPolicies::defaults()
    };
    let plan = plan_of(vec![deploy("d", "db", &[])], policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_flaky(2), journal.clone());

    let results = exec.execute(&plan).await.expect("retries converge");
    assert_eq!(results.status, ExecutionStatus::Succeeded);

    let step_stats = results.step_stats.get(&StepId::new("d").unwrap()).unwrap();
    assert_eq!(step_stats.attempts, 3);
    assert_eq!(step_stats.completed, 1);
    assert_eq!(step_stats.failed, 0, "final failure count resets when retry converges");

    // Journal must contain StepStarted attempts 1,2,3 and
    // StepFailed attempts 1,2 and StepCompleted attempt 3.
    let events = journal.snapshot();
    let started_attempts: Vec<u32> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepStarted { attempt, step_id } if step_id.as_str() == "d" => {
                Some(*attempt)
            }
            _ => None,
        })
        .collect();
    assert_eq!(started_attempts, vec![1, 2, 3]);

    let failed_attempts: Vec<u32> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepFailed { attempt, step_id, .. } if step_id.as_str() == "d" => {
                Some(*attempt)
            }
            _ => None,
        })
        .collect();
    assert_eq!(failed_attempts, vec![1, 2]);

    let completed: Vec<u32> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepCompleted { attempt, step_id, .. } if step_id.as_str() == "d" => {
                Some(*attempt)
            }
            _ => None,
        })
        .collect();
    assert_eq!(completed, vec![3]);
}

/// A step that fails 5 times, under `Retry { attempts: 2 }` (3 total
/// tries), exhausts the budget. Final outcome is Failed and the
/// failure cascades to dependents exactly like `Skip`.
#[tokio::test]
async fn retry_exhausted_falls_through_to_skip_cascade() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Retry { attempts: 2 },
        ..ExecutionPolicies::defaults()
    };
    let plan = plan_of(
        vec![
            deploy("d", "db", &[]),
            deploy("e", "db", &["d"]),
        ],
        policies,
    );
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_flaky(5), journal.clone());

    let results = exec.execute(&plan).await.expect("exhaustion does not halt with Retry");
    assert_eq!(results.status, ExecutionStatus::PartialFailure);

    let d_stats = results.step_stats.get(&StepId::new("d").unwrap()).unwrap();
    assert_eq!(d_stats.attempts, 3, "1 initial + 2 retries = 3 tries");
    assert_eq!(d_stats.failed, 1, "final attempt counts as one failure");

    let e_stats = results.step_stats.get(&StepId::new("e").unwrap()).unwrap();
    assert_eq!(e_stats.skipped, 1, "e cascade-skips after d exhausts retries");

    // Journal must show StepFailed for attempts 1, 2, 3 on d, and
    // StepSkipped for e.
    let events = journal.snapshot();
    let d_failed: Vec<u32> = events
        .iter()
        .filter_map(|e| match &e.kind {
            JournalEventKind::StepFailed { attempt, step_id, .. } if step_id.as_str() == "d" => {
                Some(*attempt)
            }
            _ => None,
        })
        .collect();
    assert_eq!(d_failed, vec![1, 2, 3]);

    let e_skipped = events
        .iter()
        .any(|e| matches!(&e.kind, JournalEventKind::StepSkipped { step_id, .. } if step_id.as_str() == "e"));
    assert!(e_skipped, "e must be cascade-skipped");
}

/// `Retry { attempts: 0 }` is equivalent to `Skip` — no retry, one
/// attempt, cascade on failure.
#[tokio::test]
async fn retry_zero_attempts_behaves_like_skip() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Retry { attempts: 0 },
        ..ExecutionPolicies::defaults()
    };
    let plan = plan_of(vec![deploy("d", "db", &[])], policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_flaky(1), journal);

    let results = exec.execute(&plan).await.expect("runs");
    assert_eq!(results.status, ExecutionStatus::PartialFailure);
    let d_stats = results.step_stats.get(&StepId::new("d").unwrap()).unwrap();
    assert_eq!(d_stats.attempts, 1, "zero retries means one try");
    assert_eq!(d_stats.failed, 1);
}

/// Under `OnFailure::Skip` the executor does not retry at all, even
/// if a subsequent attempt would have succeeded.
#[tokio::test]
async fn skip_policy_does_not_retry() {
    let policies = ExecutionPolicies {
        on_failure: OnFailure::Skip,
        ..ExecutionPolicies::defaults()
    };
    let plan = plan_of(vec![deploy("d", "db", &[])], policies);
    let journal = Arc::new(InMemoryJournalWriter::new());
    // FlakyRuntime(1): would succeed on attempt 2. But Skip policy
    // means only attempt 1 runs.
    let exec = fresh_executor(runtimes_flaky(1), journal);

    let results = exec.execute(&plan).await.expect("runs");
    let d_stats = results.step_stats.get(&StepId::new("d").unwrap()).unwrap();
    assert_eq!(d_stats.attempts, 1);
    assert_eq!(d_stats.failed, 1);
}
