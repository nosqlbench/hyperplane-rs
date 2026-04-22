// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::BarrierHandle`].

use std::collections::BTreeSet;
use std::time::Duration;

use paramodel_executor::{BarrierError, BarrierHandle, BarrierState};
use paramodel_plan::{BarrierId, StepId};

use crate::providers::BarrierHandleProvider;

fn bid(s: &str) -> BarrierId {
    BarrierId::new(s).unwrap()
}
fn sid(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

fn deps(ids: &[&str]) -> BTreeSet<StepId> {
    ids.iter().copied().map(sid).collect()
}

/// A handle with no dependencies is immediately `Satisfied`.
pub async fn tck_barrier_empty_deps_is_immediately_satisfied<P>(provider: &P)
where
    P: BarrierHandleProvider,
{
    let h = provider.fresh(bid("empty"), BTreeSet::new());
    assert_eq!(h.state(), BarrierState::Satisfied);
    h.await_satisfied().await.expect("must resolve");
}

/// `mark_dependency_satisfied` drains the pending set; reaching
/// zero pending flips state to `Satisfied` and releases waiters.
pub async fn tck_barrier_marks_and_satisfies<P>(provider: &P)
where
    P: BarrierHandleProvider,
{
    let h = provider.fresh(bid("b"), deps(&["a", "b"]));
    assert_eq!(h.state(), BarrierState::Pending);
    assert_eq!(h.pending_dependencies().len(), 2);
    h.mark_dependency_satisfied(&sid("a")).await;
    assert_eq!(h.state(), BarrierState::Pending);
    h.mark_dependency_satisfied(&sid("b")).await;
    assert_eq!(h.state(), BarrierState::Satisfied);
    h.await_satisfied().await.expect("resolves");
    assert_eq!(h.pending_dependencies().len(), 0);
    assert_eq!(h.satisfied_dependencies().len(), 2);
    assert!(h.satisfied_at().is_some());
}

/// `fail` pushes state to `Failed` and surfaces `BarrierError::Failed`.
pub async fn tck_barrier_fail_errors_out<P: BarrierHandleProvider>(provider: &P) {
    let h = provider.fresh(bid("b"), deps(&["a"]));
    h.fail("user abort").await;
    assert_eq!(h.state(), BarrierState::Failed);
    let err = h.await_satisfied().await.unwrap_err();
    assert!(matches!(err, BarrierError::Failed { .. }));
}

/// `release` forces `Satisfied` regardless of pending deps.
pub async fn tck_barrier_release_forces_satisfied<P: BarrierHandleProvider>(
    provider: &P,
) {
    let h = provider.fresh(bid("b"), deps(&["a", "b"]));
    assert_eq!(h.state(), BarrierState::Pending);
    h.release().await;
    assert_eq!(h.state(), BarrierState::Satisfied);
    h.await_satisfied().await.unwrap();
}

/// `await_with_timeout(...)` returns `Ok(false)` and leaves state
/// `TimedOut` when deps don't come in.
pub async fn tck_barrier_await_with_timeout_times_out<P>(provider: &P)
where
    P: BarrierHandleProvider,
{
    let h = provider.fresh(bid("b"), deps(&["never"]));
    let got = h
        .await_with_timeout(Duration::from_millis(50))
        .await
        .unwrap();
    assert!(!got, "timeout → false");
    assert_eq!(h.state(), BarrierState::TimedOut);
}
