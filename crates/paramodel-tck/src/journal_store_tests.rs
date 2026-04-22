// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::JournalStore`].
//!
//! Each function is `async` and takes a provider; downstream test
//! suites call them from `#[tokio::test]` bodies (a future slice
//! wires up a `run_all!` macro that does this automatically).

use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use paramodel_elements::Fingerprint;
use paramodel_executor::{ExecutionId, JournalEvent, JournalEventKind, JournalSequence};
use paramodel_persistence::JournalStore;
use ulid::Ulid;

use crate::providers::JournalStoreProvider;

fn make_event(execution: ExecutionId, seq: u64) -> JournalEvent {
    JournalEvent {
        sequence:     JournalSequence::new(seq),
        execution_id: execution,
        timestamp:    Timestamp::now(),
        kind:         JournalEventKind::ExecutionCompleted {
            success:  true,
            duration: Duration::from_millis(seq),
        },
    }
}

fn fresh_execution_id() -> ExecutionId {
    ExecutionId::from_ulid(Ulid::new())
}

/// Append, then replay from scratch, recovers every event in order.
pub async fn tck_journal_store_append_then_replay<P: JournalStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=5 {
        store.append(&make_event(exec, i)).await.expect("append");
    }
    let events: Vec<JournalEvent> = store
        .replay(&exec, None)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events.len(), 5, "expected 5 replayed events");
    for (i, e) in events.iter().enumerate() {
        assert_eq!(
            e.sequence.get(),
            u64::try_from(i + 1).unwrap(),
            "sequence should match append order"
        );
    }
}

/// Replay with `after_sequence` skips everything at or before the
/// supplied sequence.
pub async fn tck_journal_store_replay_after_sequence<P: JournalStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=4 {
        store.append(&make_event(exec, i)).await.unwrap();
    }
    let events: Vec<JournalEvent> = store
        .replay(&exec, Some(JournalSequence::new(2)))
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.sequence.get() > 2));
}

/// `latest_sequence` returns the highest sequence appended; `ZERO`
/// for an execution with no events.
pub async fn tck_journal_store_latest_sequence<P: JournalStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    let other = fresh_execution_id();
    assert_eq!(
        store.latest_sequence(&exec).await.unwrap(),
        JournalSequence::ZERO,
        "empty execution should report ZERO"
    );
    for i in 1..=3 {
        store.append(&make_event(exec, i)).await.unwrap();
    }
    store
        .append(&make_event(other, 99))
        .await
        .expect("cross-execution append");
    assert_eq!(
        store.latest_sequence(&exec).await.unwrap(),
        JournalSequence::new(3),
        "sibling execution must not bleed into latest_sequence"
    );
}

/// `truncate_before` drops events with sequence strictly below the
/// supplied bound.
pub async fn tck_journal_store_truncate_before<P: JournalStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=5 {
        store.append(&make_event(exec, i)).await.unwrap();
    }
    store
        .truncate_before(&exec, JournalSequence::new(3))
        .await
        .unwrap();
    let survivors: Vec<u64> = store
        .replay(&exec, None)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .map(|e| e.sequence.get())
        .collect();
    assert_eq!(survivors, vec![3, 4, 5]);
}

/// Per-execution isolation: events appended to one id don't appear
/// in another's replay.
pub async fn tck_journal_store_per_execution_isolation<P: JournalStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let a = fresh_execution_id();
    let b = fresh_execution_id();
    store.append(&make_event(a, 1)).await.unwrap();
    store.append(&make_event(b, 1)).await.unwrap();
    let a_count = store
        .replay(&a, None)
        .await
        .collect::<Vec<_>>()
        .await
        .len();
    let b_count = store
        .replay(&b, None)
        .await
        .collect::<Vec<_>>()
        .await
        .len();
    assert_eq!(a_count, 1);
    assert_eq!(b_count, 1);
}

// Silence unused-import warning in lib builds — Fingerprint is used
// in the tests module below.
#[allow(dead_code)]
fn _fingerprint_keeps_import_alive() -> Fingerprint {
    Fingerprint::of(b"tck")
}
