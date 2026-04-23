// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end coverage of `ExecutionResults.trial_results`.
//!
//! A plan with `TrialStart / (work) / TrialEnd` steps must produce
//! one `TrialResult` per completed `TrialEnd`, with the trial's
//! status derived from whether any step in that trial failed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, ElementRuntime, Fingerprint, LabelValue, Labels,
    LiveStatusSummary, MaterializationOutputs, OperationalState, ParameterName,
    ResolvedConfiguration, StateObservation, StateTransitionListener, TrialId, Value,
    attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionStatus, Executor,
    InMemoryJournalWriter, NoopObserver, NoopStateManager,
};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, OutputSelector, PerformanceMetrics,
    ShutdownReason, StepHeader, StepId, TestPlanId,
};
use paramodel_trials::TrialStatus;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Runtimes.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct OkRuntime;

#[derive(Debug, Default)]
struct FailingRuntime;

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
// Plan helpers.
// ---------------------------------------------------------------------------

fn svc() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn tid(n: u64) -> TrialId {
    TrialId::from_ulid(Ulid::from_parts(n, 1))
}

fn sid(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

fn header(id: &str, deps: &[&str]) -> StepHeader {
    StepHeader::builder()
        .id(sid(id))
        .depends_on(deps.iter().map(|d| sid(d)).collect())
        .reason("test".to_owned())
        .build()
}

fn trial_start(id: &str, trial_id: TrialId, deps: &[&str]) -> AtomicStep {
    AtomicStep::TrialStart {
        header:        header(id, deps),
        trial_id,
        element_names: Vec::new(),
    }
}

fn trial_end(id: &str, trial_id: TrialId, deps: &[&str]) -> AtomicStep {
    AtomicStep::TrialEnd {
        header:          header(id, deps),
        trial_id,
        element_names:   Vec::new(),
        shutdown_reason: ShutdownReason::Normal,
    }
}

fn save_output(
    id: &str,
    trial_id: TrialId,
    element: &str,
    deps: &[&str],
) -> AtomicStep {
    AtomicStep::SaveOutput {
        header:          header(id, deps),
        element:         ename(element),
        instance_number: 0,
        trial_id,
        output:          OutputSelector::ResultParameter { parameter: pname("latency") },
    }
}

fn deploy(id: &str, element: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header:                header(id, deps),
        element:               ename(element),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn plan_of(steps: Vec<AtomicStep>) -> ExecutionPlan {
    let _ = Element::builder()
        .name(ename("db"))
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

fn runtimes_ok() -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(ename("db"), Arc::new(OkRuntime) as Arc<dyn ElementRuntime>);
    m
}

/// Runtime that returns a single typed output parameter `latency = 42`.
#[derive(Debug, Default)]
struct LatencyRuntime;

#[async_trait]
impl ElementRuntime for LatencyRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        let mut outs = MaterializationOutputs::new();
        outs.insert(
            pname("latency"),
            Value::integer(pname("latency"), 42, None),
        );
        Ok(outs)
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

fn runtimes_latency() -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(ename("db"), Arc::new(LatencyRuntime) as Arc<dyn ElementRuntime>);
    m
}

fn save_volume(id: &str, trial_id: TrialId, element: &str, mount: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::SaveOutput {
        header:          header(id, deps),
        element:         ename(element),
        instance_number: 0,
        trial_id,
        output:          OutputSelector::Volume { mount: mount.to_owned() },
    }
}

fn runtimes_mixed() -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    // `good` uses the latency-emitting runtime so its SaveOutput
    // succeeds; `bad` is FailingRuntime but the bad trial's failure
    // comes from NoOutputsForInstance (no prior Deploy), not from
    // the runtime itself.
    m.insert(ename("good"), Arc::new(LatencyRuntime) as Arc<dyn ElementRuntime>);
    m.insert(ename("bad"), Arc::new(FailingRuntime) as Arc<dyn ElementRuntime>);
    m
}

fn runtimes_failing() -> BTreeMap<ElementName, Arc<dyn ElementRuntime>> {
    let mut m: BTreeMap<ElementName, Arc<dyn ElementRuntime>> = BTreeMap::new();
    m.insert(ename("db"), Arc::new(FailingRuntime) as Arc<dyn ElementRuntime>);
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

/// A two-step trial (`TrialStart → TrialEnd`) produces exactly one
/// `TrialResult` with status `Completed`.
#[tokio::test]
async fn trial_end_produces_one_completed_trial_result() {
    let t = tid(1);
    let plan = plan_of(vec![
        trial_start("ts", t.clone(), &[]),
        trial_end("te", t.clone(), &["ts"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_ok(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.trial_id, t);
    assert_eq!(r.status, TrialStatus::Completed);
    assert_eq!(r.attempt, 1);
    assert!(r.error.is_none());
    assert!(r.timing.completed_at >= r.timing.started_at);
}

/// A plan with two trials (two TrialStart/TrialEnd pairs over distinct
/// trial ids) produces one `TrialResult` per trial.
#[tokio::test]
async fn multiple_trials_produce_one_result_each() {
    let t1 = tid(1);
    let t2 = tid(2);
    let plan = plan_of(vec![
        trial_start("ts1", t1.clone(), &[]),
        trial_end("te1", t1.clone(), &["ts1"]),
        trial_start("ts2", t2.clone(), &["te1"]),
        trial_end("te2", t2.clone(), &["ts2"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_ok(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.trial_results.len(), 2);
    let ids: Vec<&TrialId> = results.trial_results.iter().map(|r| &r.trial_id).collect();
    assert!(ids.contains(&&t1) && ids.contains(&&t2));
    for r in &results.trial_results {
        assert_eq!(r.status, TrialStatus::Completed);
    }
}

/// A trial whose SaveOutput step fails (because the target element's
/// runtime reports `OperationalState::Failed`) yields a `TrialResult`
/// with status `Failed` and an `error` payload.
///
/// Structure: TrialStart → SaveOutput(on failing element) → TrialEnd.
/// The SaveOutput fails as `StepOutcome::Failed`, marking the trial
/// failed. TrialEnd completes (it has no direct dep on SaveOutput
/// here; see plan layout below) and finalises the aggregate.
#[tokio::test]
async fn failed_trial_step_yields_failed_trial_result() {
    let t = tid(1);
    // TrialStart → SaveOutput (fails) \
    //            →                     → TrialEnd
    // TrialEnd depends on TrialStart only — not SaveOutput — so the
    // SaveOutput failure marks trial_failed but does not cascade to
    // TrialEnd, allowing the aggregate to finalise as Failed.
    let plan = plan_of(vec![
        trial_start("ts", t.clone(), &[]),
        save_output("so", t.clone(), "db", &["ts"]),
        trial_end("te", t.clone(), &["ts"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_failing(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.status, ExecutionStatus::PartialFailure);
    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.trial_id, t);
    assert_eq!(r.status, TrialStatus::Failed);
    assert!(r.error.is_some(), "Failed trial must carry an ErrorInfo");
    assert_eq!(r.error.as_ref().unwrap().kind, "ElementFailed");
}

/// Two parallel trials, one fails and one succeeds. Each yields
/// exactly one TrialResult with the right status and error payload;
/// failure in one trial does not contaminate its sibling.
///
/// The "bad" trial's `SaveOutput` fires without a prior `Deploy` →
/// `NoOutputsForInstance`. The "good" trial's `SaveOutput` has its
/// Deploy prerequisite and completes cleanly.
#[tokio::test]
async fn failure_in_one_trial_does_not_contaminate_sibling() {
    let t_bad = tid(1);
    let t_good = tid(2);
    // Only the good trial has a Deploy; the bad trial's SaveOutput
    // will fail because no outputs have been recorded for "bad".
    let plan = plan_of(vec![
        deploy("d_good", "good", &[]),
        trial_start("ts_bad", t_bad.clone(), &[]),
        save_output("so_bad", t_bad.clone(), "bad", &["ts_bad"]),
        trial_end("te_bad", t_bad.clone(), &["ts_bad"]),
        trial_start("ts_good", t_good.clone(), &["d_good"]),
        save_output("so_good", t_good.clone(), "good", &["ts_good"]),
        trial_end("te_good", t_good.clone(), &["ts_good"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_mixed(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.status, ExecutionStatus::PartialFailure);
    assert_eq!(results.trial_results.len(), 2);

    let by_id: BTreeMap<TrialId, &paramodel_trials::TrialResult> = results
        .trial_results
        .iter()
        .map(|r| (r.trial_id.clone(), r))
        .collect();
    let bad_r = by_id.get(&t_bad).expect("bad trial result present");
    let good_r = by_id.get(&t_good).expect("good trial result present");

    assert_eq!(bad_r.status, TrialStatus::Failed);
    assert!(bad_r.error.is_some());
    assert_eq!(good_r.status, TrialStatus::Completed);
    assert!(
        good_r.error.is_none(),
        "sibling trial error must not carry the bad trial's error: {:?}",
        good_r.error,
    );
}

/// A `TrialStart` without any matching `TrialEnd` produces no
/// `TrialResult` — the trial never finalised.
#[tokio::test]
async fn trial_start_without_trial_end_produces_no_result() {
    let t = tid(1);
    let plan = plan_of(vec![trial_start("ts", t, &[])]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_ok(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.status, ExecutionStatus::Succeeded);
    assert_eq!(results.trial_results.len(), 0);
}

/// A `TrialEnd` without a prior `TrialStart` still finalises a
/// `TrialResult` — start and end collapse to the same instant (zero
/// duration) but aggregation is not blocked.
#[tokio::test]
async fn trial_end_without_trial_start_still_produces_result() {
    let t = tid(1);
    let plan = plan_of(vec![trial_end("te", t.clone(), &[])]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_ok(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.trial_id, t);
    assert_eq!(r.status, TrialStatus::Completed);
    assert!(r.timing.completed_at >= r.timing.started_at);
}

/// When an upstream step fails and cascade-skips a trial's TrialEnd,
/// no TrialResult is emitted for that trial — the trial never
/// completed.
#[tokio::test]
async fn cascade_skipped_trial_end_emits_no_trial_result() {
    let t = tid(1);
    // deploy(failing) → trial_start → trial_end.
    // The failing deploy cascade-skips trial_start and trial_end.
    let plan = plan_of(vec![
        deploy("d", "db", &[]), // `db` runtime is FailingRuntime below
        trial_start("ts", t.clone(), &["d"]),
        trial_end("te", t, &["ts"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_failing(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.status, ExecutionStatus::PartialFailure);
    // TrialEnd never completes → no TrialResult for its trial_id.
    assert_eq!(results.trial_results.len(), 0);
}

// ---------------------------------------------------------------------------
// SaveOutput metric capture (real-metrics scope).
// ---------------------------------------------------------------------------

/// Deploy an element whose runtime emits `latency=42`; `SaveOutput`
/// with `ResultParameter { latency }` captures that value into the
/// trial's metrics.
#[tokio::test]
async fn save_output_captures_result_parameter_into_trial_metrics() {
    let t = tid(1);
    let plan = plan_of(vec![
        deploy("d", "db", &[]),
        trial_start("ts", t.clone(), &["d"]),
        save_output("so", t.clone(), "db", &["ts"]),
        trial_end("te", t.clone(), &["so"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_latency(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.status, TrialStatus::Completed);
    let v = r
        .metrics
        .get(&ename("db"), &pname("latency"))
        .expect("metric present");
    assert_eq!(v.as_integer(), Some(42));
}

/// `SaveOutput` targeting a parameter the runtime did not produce
/// fails with `MissingResultParameter`; the trial's TrialResult
/// reflects the failure.
#[tokio::test]
async fn save_output_missing_result_parameter_fails_the_trial() {
    let t = tid(1);
    let plan = plan_of(vec![
        deploy("d", "db", &[]),
        trial_start("ts", t.clone(), &["d"]),
        save_output("so_missing", t.clone(), "db", &["ts"]), // ok, asks for `latency` which exists — override:
        trial_end("te", t.clone(), &["ts"]),
    ]);
    // Override: make the SaveOutput ask for a parameter the runtime
    // does NOT emit. Easier: use a fresh plan helper targeting an
    // unused parameter name.
    let plan2 = plan_of(vec![
        deploy("d", "db", &[]),
        trial_start("ts", t.clone(), &[]),
        AtomicStep::SaveOutput {
            header:          header("so", &["ts", "d"]),
            element:         ename("db"),
            instance_number: 0,
            trial_id:        t.clone(),
            output:          OutputSelector::ResultParameter {
                parameter: pname("not_produced"),
            },
        },
        trial_end("te", t.clone(), &["ts"]),
    ]);
    let _ = plan; // silence unused
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_latency(), journal);
    let results = exec.execute(&plan2).await.expect("runs");

    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.status, TrialStatus::Failed);
    assert_eq!(
        r.error.as_ref().unwrap().kind,
        "MissingResultParameter",
    );
}

/// `SaveOutput` without a prior `Deploy` fails with
/// `NoOutputsForInstance`.
#[tokio::test]
async fn save_output_without_prior_deploy_fails() {
    let t = tid(1);
    let plan = plan_of(vec![
        trial_start("ts", t.clone(), &[]),
        save_output("so", t.clone(), "db", &["ts"]),
        trial_end("te", t.clone(), &["ts"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_latency(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.status, TrialStatus::Failed);
    assert_eq!(
        r.error.as_ref().unwrap().kind,
        "NoOutputsForInstance",
    );
}

/// `OutputSelector::Volume` appends an ArtifactRef to the trial
/// (not to metrics).
#[tokio::test]
async fn save_output_volume_appends_artifact_to_trial() {
    let t = tid(1);
    let plan = plan_of(vec![
        deploy("d", "db", &[]),
        trial_start("ts", t.clone(), &["d"]),
        save_volume("sv", t.clone(), "db", "/var/log", &["ts"]),
        trial_end("te", t.clone(), &["sv"]),
    ]);
    let journal = Arc::new(InMemoryJournalWriter::new());
    let exec = fresh_executor(runtimes_latency(), journal);
    let results = exec.execute(&plan).await.expect("runs");

    assert_eq!(results.trial_results.len(), 1);
    let r = &results.trial_results[0];
    assert_eq!(r.artifacts.len(), 1);
    assert_eq!(r.artifacts[0].name, "/var/log");
    assert_eq!(r.artifacts[0].element, ename("db"));
    assert!(r.metrics.is_empty(), "volume capture must not produce metrics");
}
