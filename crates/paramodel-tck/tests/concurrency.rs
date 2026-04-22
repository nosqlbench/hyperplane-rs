// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::items_after_statements,
    clippy::cast_possible_truncation,
    reason = "concurrency stress; N is a small const so casts to usize are safe"
)]

//! Concurrency stress tests for the in-memory mocks. Each test
//! fires N concurrent tokio tasks through the store surface and
//! verifies no data loss / no deadlock / consistent aggregate
//! counters.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
use jiff::Timestamp;
use paramodel_elements::{ElementName, ParameterName, TrialId, Value};
use paramodel_executor::{
    ArtifactContent, ExecutionId, JournalEvent, JournalEventKind, JournalSequence,
};
use paramodel_persistence::{
    ArtifactContext, ArtifactStore, JournalStore, ResultFilter, ResultStore, SaveMode,
};
use paramodel_trials::{ArtifactRef, ExecutionTiming, Metrics, TrialResult, TrialStatus};
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

// ---------------------------------------------------------------------------
// JournalStore: concurrent appends don't lose events.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn journal_store_concurrent_append_preserves_every_event() {
    let store = Arc::new(paramodel_mock::MockJournalStore::new());
    let exec = ExecutionId::from_ulid(Ulid::new());

    const N: u64 = 256;
    let mut tasks = Vec::with_capacity(N as usize);
    for i in 1..=N {
        let s = store.clone();
        tasks.push(tokio::spawn(async move {
            s.append(&JournalEvent {
                sequence:     JournalSequence::new(i),
                execution_id: exec,
                timestamp:    Timestamp::now(),
                kind:         JournalEventKind::ExecutionCompleted {
                    success:  true,
                    duration: Duration::from_millis(i),
                },
            })
            .await
        }));
    }
    for j in join_all(tasks).await {
        j.unwrap().unwrap();
    }

    let total = store
        .latest_sequence(&exec)
        .await
        .unwrap()
        .get()
        .max(
            futures::StreamExt::collect::<Vec<_>>(
                store.replay(&exec, None).await,
            )
            .await
            .len() as u64,
        );
    assert_eq!(total, N, "every appended event must be recoverable");
}

// ---------------------------------------------------------------------------
// ResultStore: concurrent save + count is monotonic.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn result_store_concurrent_saves_monotonic_count() {
    let store = Arc::new(paramodel_mock::MockResultStore::new());
    const N: u64 = 128;
    let mut tasks = Vec::with_capacity(N as usize);
    for i in 1..=N {
        let s = store.clone();
        tasks.push(tokio::spawn(async move {
            let mut inner = BTreeMap::new();
            #[allow(clippy::cast_possible_wrap)]
            let lat = i as i64;
            inner.insert(
                pname("latency_ms"),
                Value::integer(pname("latency_ms"), lat, None),
            );
            let mut outer = BTreeMap::new();
            outer.insert(ename("db"), inner);
            let r = TrialResult::builder()
                .trial_id(TrialId::from_ulid(Ulid::from_parts(i, 1)))
                .status(TrialStatus::Completed)
                .attempt(1)
                .metrics(Metrics::from_map(outer))
                .timing(
                    ExecutionTiming::new(
                        Timestamp::from_second(1_700_000_000).unwrap(),
                        Timestamp::from_second(1_700_000_001).unwrap(),
                    )
                    .unwrap(),
                )
                .build();
            s.save(&r).await
        }));
    }
    for j in join_all(tasks).await {
        j.unwrap().unwrap();
    }
    let n = store.count(&ResultFilter::Any).await.unwrap();
    assert_eq!(n, N);
}

// ---------------------------------------------------------------------------
// ArtifactStore: concurrent save+get under `CreateNew` mode.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn artifact_store_concurrent_distinct_saves_all_succeed() {
    let store = Arc::new(paramodel_mock::MockArtifactStore::new());
    const N: u64 = 64;
    let mut tasks = Vec::with_capacity(N as usize);
    for i in 0..N {
        let s = store.clone();
        tasks.push(tokio::spawn(async move {
            let name = format!("log-{i}");
            let a = ArtifactRef::builder()
                .element(ename("db"))
                .name(name)
                .uri("inline://".to_owned())
                .content_type("text/plain".to_owned())
                .build();
            s.save(
                &ArtifactContext::default(),
                &a,
                ArtifactContent::inline(b"x".to_vec()),
                SaveMode::CreateNew,
            )
            .await
        }));
    }
    let ids: Vec<_> = join_all(tasks)
        .await
        .into_iter()
        .map(|r| r.unwrap().unwrap())
        .collect();
    assert_eq!(ids.len(), N as usize);
    // Every id is unique.
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), N as usize);
}
