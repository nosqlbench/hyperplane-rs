// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Verify `JournalStore::subscribe` on SqliteStore is a real live tail.

use std::sync::Arc;
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

/// A subscriber receives events that land *after* the subscribe call.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_receives_live_events() {
    let store = Arc::new(SqliteStore::in_memory().await.unwrap());
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));

    // Event #1 is appended *before* the subscribe — live subscribers
    // do not see it (that's what `replay` is for).
    store.append(&ev(1, exec, 1_700_000_000)).await.unwrap();

    let subscriber_store = store.clone();
    let subscriber = tokio::spawn(async move {
        let mut stream = subscriber_store.subscribe(&exec).await;
        let mut collected = Vec::new();
        // Pull the next two events — they come from the appends
        // scheduled below.
        for _ in 0..2 {
            let next = stream.next().await.expect("stream open");
            collected.push(next.unwrap());
        }
        collected
    });

    // Let the subscriber park on `recv` before we append.
    tokio::time::sleep(Duration::from_millis(50)).await;

    store.append(&ev(2, exec, 1_700_000_001)).await.unwrap();
    store.append(&ev(3, exec, 1_700_000_002)).await.unwrap();

    let collected = tokio::time::timeout(Duration::from_secs(2), subscriber)
        .await
        .unwrap()
        .unwrap();
    let seqs: Vec<u64> = collected.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(seqs, vec![2, 3]);
}

/// Multiple concurrent subscribers all see every event once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multiple_subscribers_each_get_full_fanout() {
    let store = Arc::new(SqliteStore::in_memory().await.unwrap());
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));

    // Two subscribers.
    let s1 = store.clone();
    let a = tokio::spawn(async move {
        let mut s = s1.subscribe(&exec).await;
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(s.next().await.unwrap().unwrap().sequence.get());
        }
        out
    });
    let s2 = store.clone();
    let b = tokio::spawn(async move {
        let mut s = s2.subscribe(&exec).await;
        let mut out = Vec::new();
        for _ in 0..3 {
            out.push(s.next().await.unwrap().unwrap().sequence.get());
        }
        out
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    for i in 1..=3u64 {
        store.append(&ev(i, exec, 1_700_000_000 + i as i64)).await.unwrap();
    }

    let (a, b) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(2), a),
        tokio::time::timeout(Duration::from_secs(2), b),
    );
    assert_eq!(a.unwrap().unwrap(), vec![1, 2, 3]);
    assert_eq!(b.unwrap().unwrap(), vec![1, 2, 3]);
}

/// Executions are isolated — a subscriber to execution A does not
/// see events appended to execution B.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_is_execution_scoped() {
    let store = Arc::new(SqliteStore::in_memory().await.unwrap());
    let a = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    let b = ExecutionId::from_ulid(Ulid::from_parts(2, 1));

    let store_a = store.clone();
    let sub_a = tokio::spawn(async move {
        let mut s = store_a.subscribe(&a).await;
        // Expect exactly one event — for execution a.
        let first = tokio::time::timeout(Duration::from_millis(300), s.next())
            .await
            .ok()
            .flatten();
        let second = tokio::time::timeout(Duration::from_millis(300), s.next())
            .await
            .ok()
            .flatten();
        (first, second)
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    store.append(&ev(1, b, 1_700_000_000)).await.unwrap();
    store.append(&ev(2, b, 1_700_000_001)).await.unwrap();
    store.append(&ev(7, a, 1_700_000_100)).await.unwrap();

    let (first, second) = sub_a.await.unwrap();
    let first = first.expect("got first").unwrap();
    assert_eq!(first.execution_id, a);
    assert_eq!(first.sequence.get(), 7);
    assert!(second.is_none(), "no further events for execution a");
}
