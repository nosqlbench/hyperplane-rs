// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Adversarial / negative-path tests. Every assertion here proves
//! that a malformed input is rejected cleanly rather than
//! panicking, silently succeeding, or producing a corrupt graph.

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler, WarningCode};
use paramodel_elements::{
    Dependency, Element, ElementName, IntegerParameter, LabelValue, Labels, Parameter,
    ParameterName, ShutdownSemantics, TokenExpr, Value, attributes::label,
};
use paramodel_persistence::{
    ArtifactContext, ArtifactStore, PersistenceError, SaveMode,
};
use paramodel_plan::{
    Axis, AxisName, ElementParameterRef, PlanError, PlanName, TestPlan, TestPlanId,
    TestPlanMetadata, TrialOrdering,
};
use paramodel_executor::ArtifactContent;
use paramodel_trials::ArtifactRef;
use paramodel_tck::MockArtifactStoreProvider;
use paramodel_tck::ArtifactStoreProvider;
use ulid::Ulid;

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}
fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}
fn plain_service(name: &str) -> Element {
    Element::builder()
        .name(ename(name))
        .labels(svc_labels())
        .build()
}
fn plan(elements: Vec<Element>, axes: Vec<Axis>) -> TestPlan {
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("adv").unwrap())
        .elements(elements)
        .axes(axes)
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

// ---------------------------------------------------------------------------
// Compiler rejects invalid plan shapes.
// ---------------------------------------------------------------------------

#[test]
fn compiler_rejects_command_shutdown_element() {
    let mut cmd = plain_service("cmd");
    cmd.shutdown_semantics = ShutdownSemantics::Command;
    let p = plan(vec![cmd], vec![]);
    let err = DefaultCompiler::default().compile(&p).unwrap_err();
    assert!(
        err.diagnostics.iter().any(|d| d.code == WarningCode::E002),
        "expected E002, got {:?}",
        err.diagnostics
    );
}

#[test]
fn compiler_rejects_token_expression_in_config() {
    let mut element = plain_service("db");
    element.parameters.push(Parameter::Integer(
        IntegerParameter::range(pname("threads"), 1, 64).unwrap(),
    ));
    element.configuration.insert(
        pname("threads"),
        paramodel_elements::ConfigEntry::token(
            TokenExpr::new("${self.ip}").unwrap(),
        ),
    );
    let p = plan(vec![element], vec![]);
    let err = DefaultCompiler::default().compile(&p).unwrap_err();
    assert!(
        err.diagnostics
            .iter()
            .any(|d| d.code == WarningCode::E002 && d.message.contains("token"))
    );
}

#[test]
fn compiler_rejects_custom_trial_ordering() {
    let p = TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("p").unwrap())
        .elements(vec![plain_service("db")])
        .trial_ordering(TrialOrdering::Custom {
            name: "weird".to_owned(),
        })
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build();
    let err = DefaultCompiler::default().compile(&p).unwrap_err();
    assert!(
        err.diagnostics.iter().any(|d| d.code == WarningCode::E002),
    );
}

#[test]
fn compiler_rejects_two_prototypes_exclusive_on_same_target() {
    let mut a = plain_service("a");
    a.dependencies.push(Dependency::exclusive(ename("db")));
    let mut b = plain_service("b");
    b.dependencies.push(Dependency::exclusive(ename("db")));
    let p = plan(vec![plain_service("db"), a, b], vec![]);
    let err = DefaultCompiler::default().compile(&p).unwrap_err();
    assert!(err.diagnostics.iter().any(|d| d.code == WarningCode::W002));
}

// ---------------------------------------------------------------------------
// Plan-layer validation: cycles, duplicate step ids, dangling deps.
// ---------------------------------------------------------------------------

#[test]
fn execution_graph_rejects_cycle() {
    use paramodel_elements::ResolvedConfiguration;
    use paramodel_plan::{AtomicStep, ExecutionGraph, StepHeader, StepId};

    let header = |id: &str, deps: &[&str]| -> StepHeader {
        StepHeader::builder()
            .id(StepId::new(id).unwrap())
            .depends_on(
                deps.iter()
                    .map(|d| StepId::new(*d).unwrap())
                    .collect::<Vec<_>>(),
            )
            .reason("t".to_owned())
            .build()
    };
    let deploy = |id: &str, deps: &[&str]| -> AtomicStep {
        AtomicStep::Deploy {
            header:                header(id, deps),
            element:               ename("db"),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        }
    };
    let result = ExecutionGraph::new(vec![
        deploy("a", &["b"]),
        deploy("b", &["a"]),
    ]);
    assert!(matches!(result, Err(PlanError::GraphHasCycle)));
}

#[test]
fn execution_graph_rejects_duplicate_step_ids() {
    use paramodel_elements::ResolvedConfiguration;
    use paramodel_plan::{AtomicStep, ExecutionGraph, StepHeader, StepId};

    let deploy = |id: &str| -> AtomicStep {
        AtomicStep::Deploy {
            header:                StepHeader::builder()
                .id(StepId::new(id).unwrap())
                .reason("t".to_owned())
                .build(),
            element:               ename("db"),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        }
    };
    let result =
        ExecutionGraph::new(vec![deploy("a"), deploy("a")]);
    assert!(matches!(result, Err(PlanError::DuplicateStepId { .. })));
}

#[test]
fn execution_graph_rejects_dangling_dependency() {
    use paramodel_elements::ResolvedConfiguration;
    use paramodel_plan::{AtomicStep, ExecutionGraph, StepHeader, StepId};

    let result = ExecutionGraph::new(vec![AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new("a").unwrap())
            .depends_on(vec![StepId::new("ghost").unwrap()])
            .reason("t".to_owned())
            .build(),
        element:               ename("db"),
        instance_number:       0,
        configuration:         paramodel_elements::ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }]);
    assert!(matches!(result, Err(PlanError::DanglingDependency { .. })));
    let _ = ResolvedConfiguration::new();
}

// ---------------------------------------------------------------------------
// Parameter / Value validation.
// ---------------------------------------------------------------------------

#[test]
fn integer_parameter_rejects_inverted_range() {
    // max < min is a construction error.
    let err = IntegerParameter::range(pname("v"), 100, 1).unwrap_err();
    // Any error variant is fine; assertion here is that we got one.
    let _ = err;
}

#[test]
fn value_constraint_test_is_false_on_kind_mismatch() {
    use paramodel_elements::{Constraint, IntConstraint};

    let c = Constraint::Integer(IntConstraint::Max { n: 10 });
    // A `Value::Boolean` against an integer constraint cannot match.
    let v = Value::boolean(pname("b"), true, None);
    assert!(!c.test(&v));
}

// ---------------------------------------------------------------------------
// Persistence: error variants surface correctly.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn artifact_store_not_found_on_get() {
    use paramodel_persistence::ArtifactId;
    let provider = MockArtifactStoreProvider;
    let store = provider.fresh();
    let fake = ArtifactId::from_ulid(Ulid::from_parts(0, 0));
    let err = store.get(&fake).await.unwrap_err();
    assert!(matches!(err, PersistenceError::NotFound { .. }));
}

#[tokio::test]
async fn artifact_store_conflict_on_create_new_twice() {
    let provider = MockArtifactStoreProvider;
    let store = provider.fresh();
    let ctx = ArtifactContext::default();
    let a = ArtifactRef::builder()
        .element(ename("db"))
        .name("log".to_owned())
        .uri("inline://".to_owned())
        .content_type("text/plain".to_owned())
        .build();
    store
        .save(
            &ctx,
            &a,
            ArtifactContent::inline(b"x".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    let err = store
        .save(
            &ctx,
            &a,
            ArtifactContent::inline(b"y".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PersistenceError::Conflict { .. }));
}

// ---------------------------------------------------------------------------
// TrialResult invariants.
// ---------------------------------------------------------------------------

#[test]
fn trial_result_rejects_zero_attempt() {
    use paramodel_trials::{ExecutionTiming, TrialResult, TrialStatus};
    let r = TrialResult::builder()
        .trial_id(paramodel_elements::TrialId::from_ulid(Ulid::from_parts(1, 1)))
        .status(TrialStatus::Completed)
        .attempt(0)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1).unwrap(),
                Timestamp::from_second(2).unwrap(),
            )
            .unwrap(),
        )
        .build();
    assert!(r.validate().is_err());
}

#[test]
fn trial_result_rejects_failed_without_error() {
    use paramodel_trials::{ExecutionTiming, TrialResult, TrialStatus};
    let r = TrialResult::builder()
        .trial_id(paramodel_elements::TrialId::from_ulid(Ulid::from_parts(1, 1)))
        .status(TrialStatus::Failed)
        .attempt(1)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1).unwrap(),
                Timestamp::from_second(2).unwrap(),
            )
            .unwrap(),
        )
        .build();
    assert!(r.validate().is_err());
}

#[test]
fn trial_result_rejects_skipped_without_reason() {
    use paramodel_trials::{ExecutionTiming, TrialResult, TrialStatus};
    let r = TrialResult::builder()
        .trial_id(paramodel_elements::TrialId::from_ulid(Ulid::from_parts(1, 1)))
        .status(TrialStatus::Skipped)
        .attempt(1)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1).unwrap(),
                Timestamp::from_second(2).unwrap(),
            )
            .unwrap(),
        )
        .build();
    assert!(r.validate().is_err());
}

#[test]
fn execution_timing_rejects_negative_duration() {
    use paramodel_trials::ExecutionTiming;
    let err = ExecutionTiming::new(
        Timestamp::from_second(2000).unwrap(),
        Timestamp::from_second(1000).unwrap(),
    )
    .unwrap_err();
    let _ = err;
}

// ---------------------------------------------------------------------------
// Persistence: pagination clamping, idempotent saves.
// ---------------------------------------------------------------------------

#[test]
fn page_request_clamps_zero_size_to_one() {
    use paramodel_persistence::PageRequest;
    let p = PageRequest::first(0).clamped();
    assert_eq!(p.size, 1);
}

#[test]
fn page_request_clamps_huge_size_to_max() {
    use paramodel_persistence::{MAX_PAGE_SIZE, PageRequest};
    let p = PageRequest::first(100_000).clamped();
    assert_eq!(p.size, MAX_PAGE_SIZE);
}

#[tokio::test]
async fn journal_store_latest_sequence_is_zero_when_empty() {
    use paramodel_executor::{ExecutionId, JournalSequence};
    use paramodel_persistence::JournalStore;
    use paramodel_tck::{JournalStoreProvider, MockJournalStoreProvider};
    let p = MockJournalStoreProvider;
    let store = p.fresh();
    let exec = ExecutionId::from_ulid(Ulid::new());
    assert_eq!(
        store.latest_sequence(&exec).await.unwrap(),
        JournalSequence::ZERO,
    );
}

#[tokio::test]
async fn result_store_delete_of_missing_is_ok() {
    use paramodel_persistence::ResultStore;
    use paramodel_tck::{MockResultStoreProvider, ResultStoreProvider};
    let p = MockResultStoreProvider;
    let store = p.fresh();
    let tid = paramodel_elements::TrialId::from_ulid(Ulid::new());
    // Deleting a record that was never inserted should not error.
    store.delete(&tid).await.unwrap();
}

// ---------------------------------------------------------------------------
// Compiler: dependency on non-existent element surfaces cleanly.
// ---------------------------------------------------------------------------

#[test]
fn compiler_surfaces_dependency_on_missing_target() {
    let mut client = plain_service("client");
    client.dependencies.push(Dependency::shared(ename("ghost")));
    let p = plan(vec![client], vec![]);
    // The compiler either catches this at plan validation or as a
    // compiler invariant. Either way, an Err must surface.
    let result = DefaultCompiler::default().compile(&p);
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Misc silencing — keep unused-import warnings at bay across platforms.
// ---------------------------------------------------------------------------
#[allow(dead_code)]
fn _keep_imports_live() {
    let _: AxisName = AxisName::new("x").unwrap();
    let _: ElementParameterRef =
        ElementParameterRef::new(ename("db"), pname("threads"));
    let _: Value = Value::integer(pname("x"), 1, None);
}
