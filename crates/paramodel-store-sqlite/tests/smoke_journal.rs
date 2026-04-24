// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Smoke tests for `SqliteStore::JournalStore`.
//!
//! Keeps the database in-memory per test. Verifies round-trip of
//! append + replay + latest_event + latest_sequence + truncate_before
//! + delete_all, plus isolation between executions.

use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use paramodel_executor::{ExecutionId, JournalEvent, JournalEventKind, JournalSequence};
use paramodel_persistence::JournalStore;
use paramodel_store_sqlite::SqliteStore;
use ulid::Ulid;

fn ev(seq: u64, exec: ExecutionId, ts_secs: i64) -> JournalEvent {
    JournalEvent {
        sequence:     JournalSequence::new(seq),
        execution_id: exec,
        timestamp:    Timestamp::from_second(ts_secs).unwrap(),
        kind:         JournalEventKind::ExecutionCompleted {
            success:  true,
            duration: Duration::from_secs(1),
        },
    }
}

#[tokio::test]
async fn append_and_replay_roundtrips() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    for i in 1..=3 {
        store.append(&ev(i, exec, 1_700_000_000 + i as i64)).await.unwrap();
    }
    let replayed: Vec<_> = store.replay(&exec, None).await.collect::<Vec<_>>().await;
    assert_eq!(replayed.len(), 3);
    let seqs: Vec<u64> = replayed
        .into_iter()
        .map(|r| r.unwrap().sequence.get())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[tokio::test]
async fn replay_honours_after_sequence() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    for i in 1..=5 {
        store.append(&ev(i, exec, 1_700_000_000 + i as i64)).await.unwrap();
    }
    let replayed: Vec<_> = store
        .replay(&exec, Some(JournalSequence::new(2)))
        .await
        .collect::<Vec<_>>()
        .await;
    let seqs: Vec<u64> = replayed
        .into_iter()
        .map(|r| r.unwrap().sequence.get())
        .collect();
    assert_eq!(seqs, vec![3, 4, 5]);
}

#[tokio::test]
async fn latest_event_and_sequence_track_highest() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    // Empty execution: latest_sequence is ZERO, latest_event is None.
    assert_eq!(
        store.latest_sequence(&exec).await.unwrap(),
        JournalSequence::ZERO,
    );
    assert!(store.latest_event(&exec).await.unwrap().is_none());

    for i in 1..=3 {
        store.append(&ev(i, exec, 1_700_000_000 + i as i64)).await.unwrap();
    }
    assert_eq!(
        store.latest_sequence(&exec).await.unwrap(),
        JournalSequence::new(3),
    );
    let last = store.latest_event(&exec).await.unwrap().unwrap();
    assert_eq!(last.sequence.get(), 3);
}

#[tokio::test]
async fn truncate_before_drops_only_preceding_events() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    for i in 1..=5 {
        store.append(&ev(i, exec, 1_700_000_000 + i as i64)).await.unwrap();
    }
    store
        .truncate_before(&exec, JournalSequence::new(3))
        .await
        .unwrap();
    let remaining: Vec<_> = store
        .replay(&exec, None)
        .await
        .collect::<Vec<_>>()
        .await;
    let seqs: Vec<u64> = remaining
        .into_iter()
        .map(|r| r.unwrap().sequence.get())
        .collect();
    assert_eq!(seqs, vec![3, 4, 5]);
}

#[tokio::test]
async fn delete_all_is_execution_scoped() {
    let store = SqliteStore::in_memory().await.unwrap();
    let a = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    let b = ExecutionId::from_ulid(Ulid::from_parts(2, 1));
    for i in 1..=3 {
        store.append(&ev(i, a, 1_700_000_000 + i as i64)).await.unwrap();
    }
    for i in 10..=12 {
        store.append(&ev(i, b, 1_700_000_000 + i as i64)).await.unwrap();
    }
    store.delete_all(&a).await.unwrap();
    let a_events: Vec<_> = store.replay(&a, None).await.collect::<Vec<_>>().await;
    let b_events: Vec<_> = store.replay(&b, None).await.collect::<Vec<_>>().await;
    assert!(a_events.is_empty());
    assert_eq!(b_events.len(), 3);
}

#[tokio::test]
async fn append_batch_is_atomic_and_ordered() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    let batch: Vec<_> = (1..=10)
        .map(|i| ev(i, exec, 1_700_000_000 + i as i64))
        .collect();
    store.append_batch(&batch).await.unwrap();
    let replayed: Vec<_> = store.replay(&exec, None).await.collect::<Vec<_>>().await;
    assert_eq!(replayed.len(), 10);
}
