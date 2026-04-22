// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Serde round-trip coverage for every paramodel type that crosses
//! the wire. Each test constructs a non-trivial instance, serialises
//! to JSON, deserialises back, and asserts structural equality.

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{
    ElementName, Fingerprint, LabelKey, LabelValue, Labels, ParameterName,
    ResolvedConfiguration, TagKey, TagValue, Tags, TrialId, Value,
};
use paramodel_plan::{
    AtomicStep, Axis, AxisName, BarrierId, BarrierKind, CheckpointId,
    ElementInstanceGraph, ElementParameterRef, ExecutionGraph, ExecutionPlan,
    ExecutionPlanId, ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics,
    ShutdownReason, StepHeader, StepId, TestPlanId, TimeoutAction,
};
use paramodel_trials::{
    ArtifactRef, ErrorInfo, ExecutionTiming, Metrics, TrialResult, TrialStatus,
};
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn header(id: &str) -> StepHeader {
    StepHeader::builder()
        .id(StepId::new(id).unwrap())
        .reason("t".to_owned())
        .build()
}

// ---------------------------------------------------------------------------
// AtomicStep — all 8 variants.
// ---------------------------------------------------------------------------

fn check_roundtrip(step: &AtomicStep) {
    let json = serde_json::to_string(step).unwrap();
    let back: AtomicStep = serde_json::from_str(&json).unwrap();
    assert_eq!(step, &back);
}

#[test]
fn atomic_step_deploy_roundtrip() {
    let step = AtomicStep::Deploy {
        header:                header("d"),
        element:               ename("db"),
        instance_number:       7,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       Some(4),
        max_group_concurrency: Some(2),
        dedicated_to:          Some(ename("owner")),
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_teardown_roundtrip() {
    let step = AtomicStep::Teardown {
        header:            header("t"),
        element:           ename("db"),
        instance_number:   3,
        collect_artifacts: true,
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_trial_start_roundtrip() {
    let step = AtomicStep::TrialStart {
        header:        header("ts"),
        trial_id:      TrialId::from_ulid(Ulid::from_parts(1, 1)),
        element_names: vec![ename("a"), ename("b")],
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_trial_end_roundtrip() {
    let step = AtomicStep::TrialEnd {
        header:          header("te"),
        trial_id:        TrialId::from_ulid(Ulid::from_parts(2, 1)),
        element_names:   vec![ename("db")],
        shutdown_reason: ShutdownReason::Normal,
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_await_roundtrip() {
    let step = AtomicStep::Await {
        header:          header("aw"),
        element:         ename("db"),
        instance_number: 0,
        trial_id:        TrialId::from_ulid(Ulid::from_parts(3, 1)),
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_save_output_roundtrip() {
    use paramodel_plan::OutputSelector;
    let step = AtomicStep::SaveOutput {
        header:          header("so"),
        element:         ename("db"),
        instance_number: 0,
        trial_id:        TrialId::from_ulid(Ulid::from_parts(4, 1)),
        output:          OutputSelector::ResultParameter {
            parameter: pname("p50"),
        },
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_barrier_roundtrip() {
    let step = AtomicStep::Barrier {
        header:         header("b"),
        barrier_id:     BarrierId::new("b1").unwrap(),
        barrier_kind:   BarrierKind::ElementReady,
        timeout:        Some(Duration::from_secs(30)),
        timeout_action: TimeoutAction::FailFast,
    };
    check_roundtrip(&step);
}

#[test]
fn atomic_step_checkpoint_roundtrip() {
    let step = AtomicStep::Checkpoint {
        header:        header("cp"),
        checkpoint_id: CheckpointId::new("cp1").unwrap(),
    };
    check_roundtrip(&step);
}

// ---------------------------------------------------------------------------
// Axis / ExecutionPlan / TrialResult / ArtifactRef.
// ---------------------------------------------------------------------------

#[test]
fn axis_roundtrip() {
    let a = Axis::builder()
        .name(AxisName::new("threads_axis").unwrap())
        .target(ElementParameterRef::new(ename("db"), pname("threads")))
        .values(vec![
            Value::integer(pname("threads"), 1, None),
            Value::integer(pname("threads"), 2, None),
            Value::integer(pname("threads"), 4, None),
        ])
        .build();
    let json = serde_json::to_string(&a).unwrap();
    let back: Axis = serde_json::from_str(&json).unwrap();
    assert_eq!(a.target, back.target);
    assert_eq!(a.values.len(), back.values.len());
}

#[test]
fn execution_plan_roundtrip() {
    let p = ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .source_plan_fingerprint(Fingerprint::of(b"src"))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .execution_graph(ExecutionGraph::default())
        .element_instance_graph(ElementInstanceGraph::default())
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(1))
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
        .build();
    let json = serde_json::to_string(&p).unwrap();
    let back: ExecutionPlan = serde_json::from_str(&json).unwrap();
    assert_eq!(p.id, back.id);
    assert_eq!(p.source_plan_fingerprint, back.source_plan_fingerprint);
    assert_eq!(p.fingerprint(), back.fingerprint());
}

#[test]
fn trial_result_roundtrip() {
    let mut inner = BTreeMap::new();
    inner.insert(pname("qps"), Value::double(pname("qps"), 1234.5, None));
    let mut outer = BTreeMap::new();
    outer.insert(ename("db"), inner);
    let mut labels = Labels::new();
    labels.insert(
        LabelKey::new("owner").unwrap(),
        LabelValue::new("ops").unwrap(),
    );
    let mut tags = Tags::new();
    tags.insert(TagKey::new("env").unwrap(), TagValue::new("ci").unwrap());
    let r = TrialResult::builder()
        .trial_id(TrialId::from_ulid(Ulid::from_parts(42, 1)))
        .status(TrialStatus::Failed)
        .attempt(2)
        .metrics(Metrics::from_map(outer))
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1_700_000_000).unwrap(),
                Timestamp::from_second(1_700_000_001).unwrap(),
            )
            .unwrap(),
        )
        .error(
            ErrorInfo::builder()
                .kind("TimeoutError".to_owned())
                .message("deadline exceeded".to_owned())
                .is_retryable(true)
                .build(),
        )
        .labels(labels)
        .tags(tags)
        .build();
    let json = serde_json::to_string(&r).unwrap();
    let back: TrialResult = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
}

#[test]
fn artifact_ref_roundtrip() {
    let a = ArtifactRef::builder()
        .element(ename("db"))
        .name("log".to_owned())
        .uri("s3://b/log".to_owned())
        .content_type("text/plain".to_owned())
        .size_bytes(1024u64)
        .build();
    let json = serde_json::to_string(&a).unwrap();
    let back: ArtifactRef = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
}
