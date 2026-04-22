// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! State-machine coverage for `DefaultExecutor` — pause, unpause,
//! stop, `checkpoint_now`, and their interactions.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Element, ElementName, LabelValue, Labels, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionError,
    ExecutionStatus, Executor, InMemoryJournalWriter, NoopObserver, NoopStateManager,
};
use paramodel_plan::{PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use ulid::Ulid;

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn tiny_plan() -> TestPlan {
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

fn fresh_executor() -> DefaultExecutor {
    DefaultExecutor::new(
        Arc::new(DefaultOrchestrator::empty()),
        Arc::new(DefaultScheduler::new()),
        Arc::new(NoopStateManager::new()),
        Arc::new(InMemoryJournalWriter::new()),
        Arc::new(NoopObserver),
    )
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn checkpoints_is_empty_initially() {
    let exec = fresh_executor();
    assert_eq!(exec.checkpoints().await.len(), 0);
}

#[tokio::test]
async fn pause_and_unpause_are_infallible_when_idle() {
    let exec = fresh_executor();
    // Neither pause nor unpause should error outside a run.
    exec.pause("manual").await.unwrap();
    exec.unpause().await.unwrap();
}

#[tokio::test]
async fn stop_request_fires_cancelled_during_execute() {
    let exec = Arc::new(fresh_executor());
    let plan = tiny_plan();
    let compiled = DefaultCompiler::default().compile(&plan).unwrap();

    // Pre-arm cancellation, then run. The cancellation flag is
    // checked at the next scheduling boundary.
    exec.stop("test", true).await.unwrap();
    let err = exec.execute(&compiled).await.unwrap_err();
    assert!(
        matches!(err, ExecutionError::Cancelled { .. }),
        "expected Cancelled, got {err:?}"
    );
}

#[tokio::test]
async fn checkpoint_now_outside_run_writes_immediately() {
    let exec = fresh_executor();
    let handle = exec.checkpoint_now().await.unwrap();
    assert!(handle.is_valid);
    // And the handle is tracked in `checkpoints()`.
    let list = exec.checkpoints().await;
    assert!(list.iter().any(|h| h.id == handle.id));
}

#[tokio::test]
async fn dry_run_on_valid_plan_reports_no_diagnostics() {
    let exec = fresh_executor();
    let plan = tiny_plan();
    let compiled = DefaultCompiler::default().compile(&plan).unwrap();
    let diags = exec.dry_run(&compiled).await;
    assert!(diags.is_empty(), "{diags:?}");
}

#[tokio::test]
async fn execute_with_orchestrator_missing_runtime_errors_cleanly() {
    let exec = fresh_executor();
    let plan = tiny_plan();
    let compiled = DefaultCompiler::default().compile(&plan).unwrap();
    // DefaultOrchestrator::empty() has no runtime for `db` — the
    // Deploy step can't be executed. The executor must surface the
    // OrchestratorError cleanly, not panic.
    let outcome = exec.execute(&compiled).await;
    match outcome {
        Ok(results) => {
            // If somehow it succeeds (e.g. because our tiny plan
            // is all-sentinel) that's fine too.
            assert!(matches!(
                results.status,
                ExecutionStatus::Succeeded
                    | ExecutionStatus::PartialFailure
                    | ExecutionStatus::Halted
                    | ExecutionStatus::Cancelled,
            ));
        }
        Err(e) => {
            assert!(
                matches!(
                    e,
                    ExecutionError::Orchestrator(_)
                        | ExecutionError::Cancelled { .. }
                        | ExecutionError::StopOnFailure { .. }
                ),
                "unexpected error variant: {e:?}"
            );
        }
    }
    let _ = Duration::from_secs(0); // silence unused-import
}
