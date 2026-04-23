// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Concurrent-waiter stress tests for `MockBarrierHandle`.
//!
//! Existing barrier tests exercise single-threaded state transitions.
//! These tests spawn multiple tokio tasks that wait on the same
//! handle simultaneously and verify the fan-out primitive wakes all
//! of them correctly under each terminal state (satisfied / failed /
//! timed out / released). They also verify idempotence of
//! `mark_dependency_satisfied` under concurrent calls.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use paramodel_executor::{BarrierError, BarrierHandle, BarrierState};
use paramodel_mock::MockBarrierHandle;
use paramodel_plan::{BarrierId, StepId};

fn bid(s: &str) -> BarrierId {
    BarrierId::new(s).unwrap()
}

fn sid(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

fn deps(names: &[&str]) -> BTreeSet<StepId> {
    names.iter().copied().map(sid).collect()
}

// ---------------------------------------------------------------------------

/// 16 tasks await the same handle. After marking its single dep
/// satisfied, every task's `await_satisfied` resolves to `Ok(())`.
#[tokio::test]
async fn many_waiters_all_wake_on_satisfy() {
    let handle = Arc::new(MockBarrierHandle::new(bid("b"), deps(&["dep"])));
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.await_satisfied().await
        }));
    }
    // Yield to let all waiters park on the notify.
    tokio::task::yield_now().await;
    handle.mark_dependency_satisfied(&sid("dep")).await;

    for t in tasks {
        t.await.expect("task joins").expect("await_satisfied must return Ok");
    }
    assert_eq!(handle.state(), BarrierState::Satisfied);
}

/// Same fan-out pattern but the terminal signal is `release()` —
/// every waiter still wakes with `Ok(())`.
#[tokio::test]
async fn many_waiters_all_wake_on_release() {
    let handle = Arc::new(MockBarrierHandle::new(
        bid("b"),
        deps(&["never-arrives"]),
    ));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.await_satisfied().await
        }));
    }
    tokio::task::yield_now().await;
    handle.release().await;
    for t in tasks {
        t.await.unwrap().expect("release must satisfy all waiters");
    }
    assert_eq!(handle.state(), BarrierState::Satisfied);
}

/// On `fail()`, every concurrent waiter gets
/// `BarrierError::Failed { reason }` with the same reason string.
#[tokio::test]
async fn many_waiters_all_wake_on_fail_with_reason() {
    let handle = Arc::new(MockBarrierHandle::new(bid("b"), deps(&["d"])));
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.await_satisfied().await
        }));
    }
    tokio::task::yield_now().await;
    handle.fail("upstream exploded").await;
    for t in tasks {
        match t.await.unwrap() {
            Err(BarrierError::Failed { reason, .. }) => {
                assert_eq!(reason, "upstream exploded");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
    assert_eq!(handle.state(), BarrierState::Failed);
}

/// Concurrent `await_with_timeout` on a never-satisfied handle: every
/// waiter returns `Ok(false)` after the timeout elapses, and the
/// handle's state moves to `TimedOut`.
#[tokio::test]
async fn concurrent_timeouts_all_report_false() {
    let handle = Arc::new(MockBarrierHandle::new(
        bid("b"),
        deps(&["d"]),
    ));
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.await_with_timeout(Duration::from_millis(60)).await
        }));
    }
    for t in tasks {
        let got = t.await.unwrap().expect("no hard error");
        assert!(!got, "all waiters must time out");
    }
    assert_eq!(handle.state(), BarrierState::TimedOut);
}

/// Half the waiters use `await_satisfied`, half use
/// `await_with_timeout` with a generous budget. All wake on the
/// satisfy — the mixed API surface shares the same notify.
#[tokio::test]
async fn mixed_waiter_apis_all_wake_on_satisfy() {
    let handle = Arc::new(MockBarrierHandle::new(bid("b"), deps(&["d"])));
    let mut await_tasks = Vec::new();
    let mut timeout_tasks = Vec::new();
    for _ in 0..4 {
        let h = handle.clone();
        await_tasks.push(tokio::spawn(async move {
            h.await_satisfied().await
        }));
    }
    for _ in 0..4 {
        let h = handle.clone();
        timeout_tasks.push(tokio::spawn(async move {
            h.await_with_timeout(Duration::from_secs(1)).await
        }));
    }
    tokio::task::yield_now().await;
    handle.mark_dependency_satisfied(&sid("d")).await;

    for t in await_tasks {
        t.await.unwrap().expect("await_satisfied returns Ok");
    }
    for t in timeout_tasks {
        assert!(t.await.unwrap().expect("no error") );
    }
}

/// Concurrent `mark_dependency_satisfied` calls for the same dep are
/// idempotent — the satisfied set ends with exactly one entry, and
/// the handle transitions to `Satisfied` exactly when the last
/// distinct dep arrives.
#[tokio::test]
async fn mark_dependency_satisfied_is_idempotent_under_concurrent_calls() {
    let handle = Arc::new(MockBarrierHandle::new(
        bid("b"),
        deps(&["d1", "d2"]),
    ));

    // Spawn many tasks all marking the same two deps. Even though
    // multiple calls per dep occur, each dep ends in `satisfied`
    // exactly once and the handle transitions cleanly.
    let mut tasks = Vec::new();
    for _ in 0..16 {
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.mark_dependency_satisfied(&sid("d1")).await;
        }));
        let h = handle.clone();
        tasks.push(tokio::spawn(async move {
            h.mark_dependency_satisfied(&sid("d2")).await;
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    assert_eq!(handle.state(), BarrierState::Satisfied);
    let sat = handle.satisfied_dependencies();
    assert_eq!(sat.len(), 2);
    assert!(sat.contains(&sid("d1")));
    assert!(sat.contains(&sid("d2")));
    assert!(handle.pending_dependencies().is_empty());
}

/// Once timed out, subsequent `mark_dependency_satisfied` calls do
/// not flip the state back to `Satisfied`. Terminal states are
/// terminal.
#[tokio::test]
async fn timed_out_handle_does_not_revert_on_late_dep_arrival() {
    let handle = Arc::new(MockBarrierHandle::new(bid("b"), deps(&["d"])));
    let _ = handle.await_with_timeout(Duration::from_millis(30)).await;
    assert_eq!(handle.state(), BarrierState::TimedOut);

    handle.mark_dependency_satisfied(&sid("d")).await;
    // Even though the pending set is drained, the outer state stays
    // TimedOut because the timeout path already closed the handle.
    assert_eq!(
        handle.state(),
        BarrierState::TimedOut,
        "terminal state must be sticky",
    );
}
