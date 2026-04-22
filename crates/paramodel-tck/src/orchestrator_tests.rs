// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::Orchestrator`].

use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, Fingerprint, HealthCheckSpec, LabelValue, Labels,
    ResolvedConfiguration, attributes::label,
};
use paramodel_executor::{Orchestrator, OrchestratorError};
use paramodel_plan::{
    AtomicStep, ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, InstanceId, OptimizationStrategy, PerformanceMetrics,
    StepHeader, StepId, TestPlanId,
};
use ulid::Ulid;

use crate::providers::OrchestratorProvider;

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn deploy_step(element: &str) -> AtomicStep {
    AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new("d1").unwrap())
            .reason("t".to_owned())
            .build(),
        element:               ElementName::new(element).unwrap(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn plan_with(steps: Vec<AtomicStep>) -> ExecutionPlan {
    let _ = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc_labels())
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
                .compiler_version("tck".to_owned())
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

/// `prepare` on an empty-runtime orchestrator succeeds even with
/// steps in the plan — resolution happens per-step.
pub async fn tck_orchestrator_prepare_is_infallible<P: OrchestratorProvider>(
    provider: &P,
) {
    let orch = provider.fresh();
    let plan = plan_with(vec![deploy_step("db")]);
    orch.prepare(&plan).await.expect("prepare ok");
}

/// Executing a Deploy step that targets an element with no
/// registered runtime surfaces [`OrchestratorError::NoRuntimeForElement`]
/// rather than panicking.
pub async fn tck_orchestrator_missing_runtime_errors_cleanly<P>(provider: &P)
where
    P: OrchestratorProvider,
{
    let orch = provider.fresh();
    let plan = plan_with(vec![deploy_step("db")]);
    let step = plan.steps()[0].clone();
    let err = orch.execute_step(&plan, &step).await.unwrap_err();
    assert!(
        matches!(err, OrchestratorError::NoRuntimeForElement { .. }),
        "unexpected: {err:?}"
    );
}

/// `instance_state` returns `None` for instances the orchestrator
/// has never seen.
pub async fn tck_orchestrator_instance_state_none_for_unknown<P>(provider: &P)
where
    P: OrchestratorProvider,
{
    let orch = provider.fresh();
    let fake = InstanceId::from_parts(&ElementName::new("ghost").unwrap(), 0);
    assert!(orch.instance_state(&fake).await.is_none());
}

/// `shutdown` is infallible on a freshly-constructed orchestrator.
pub async fn tck_orchestrator_shutdown_infallible<P: OrchestratorProvider>(
    provider: &P,
) {
    let orch = provider.fresh();
    orch.shutdown().await.expect("shutdown ok");
}

/// `await_ready` on a fresh orchestrator is a no-op (no instance
/// registered) — returns `Ok`.
pub async fn tck_orchestrator_await_ready_noop<P: OrchestratorProvider>(provider: &P) {
    let orch = provider.fresh();
    let id = InstanceId::from_parts(&ElementName::new("ghost").unwrap(), 0);
    let spec = HealthCheckSpec::new(
        Duration::from_millis(10),
        1,
        Duration::from_millis(10),
    );
    orch.await_ready(&id, &spec).await.expect("await ok");
}
