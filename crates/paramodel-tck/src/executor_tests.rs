// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::Executor`].
//!
//! Executors are deeply coupled with their collaborators
//! (orchestrator, scheduler, …). The TCK therefore targets the
//! trait-surface guarantees the executor must uphold without
//! assuming a particular backend.

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Element, ElementName, LabelValue, Labels, attributes::label,
};
use paramodel_executor::{ExecutionStatus, Executor};
use paramodel_plan::{PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use ulid::Ulid;

use crate::providers::ExecutorProvider;

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
        .name(PlanName::new("p").unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

/// `dry_run` on a well-formed plan reports no diagnostics.
pub async fn tck_executor_dry_run_accepts_valid_plan<P: ExecutorProvider>(
    provider: &P,
) {
    let exec = provider.executor();
    let plan = trivial_plan();
    let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");
    let diagnostics = exec.dry_run(&compiled).await;
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

/// `execute` against a trivial plan terminates.
///
/// Returns either `Ok(results)` with a terminal status, or `Err` —
/// e.g. `NoRuntimeForElement` when the orchestrator isn't wired to
/// the plan's elements. The contract is "no hang / panic /
/// non-termination".
pub async fn tck_executor_execute_terminates<P: ExecutorProvider>(
    provider: &P,
) {
    let exec = provider.executor();
    let plan = trivial_plan();
    let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");
    if let Ok(results) = exec.execute(&compiled).await {
        assert!(matches!(
            results.status,
            ExecutionStatus::Succeeded
                | ExecutionStatus::PartialFailure
                | ExecutionStatus::Halted
                | ExecutionStatus::Cancelled,
        ));
    }
}

/// `checkpoints()` returns a stable list — idempotent across calls.
pub async fn tck_executor_checkpoints_idempotent<P: ExecutorProvider>(
    provider: &P,
) {
    let exec = provider.executor();
    let a = exec.checkpoints().await;
    let b = exec.checkpoints().await;
    assert_eq!(a, b);
}
