// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Smoke tests for `SqliteStore::CheckpointStore`.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use paramodel_elements::Fingerprint;
use paramodel_executor::{
    Checkpoint, CheckpointState, ExecutionId, JournalSequence, ResourceUsage,
    SchedulerState,
};
use paramodel_persistence::{CheckpointStore, PageRequest, PersistenceError};
use paramodel_plan::CheckpointId;
use paramodel_store_sqlite::SqliteStore;
use ulid::Ulid;

fn mk_checkpoint(id: &str, exec: ExecutionId, ts_secs: i64) -> Checkpoint {
    let state = CheckpointState {
        completed_step_ids: BTreeSet::new(),
        failed_step_ids:    BTreeSet::new(),
        skipped_step_ids:   BTreeSet::new(),
        in_flight_step_ids: BTreeSet::new(),
        completed_trials:   BTreeMap::new(),
        in_flight_trials:   BTreeSet::new(),
        instance_states:    BTreeMap::new(),
        scheduler_state:    SchedulerState::default(),
        resource_usage:     ResourceUsage::default(),
        journal_sequence:   JournalSequence::ZERO,
    };
    let fp = Fingerprint::of(b"test");
    Checkpoint {
        id:               CheckpointId::new(id).unwrap(),
        execution_id:     exec,
        plan_fingerprint: fp,
        created_at:       Timestamp::from_second(ts_secs).unwrap(),
        state,
        fingerprint:      fp,
    }
}

#[tokio::test]
async fn save_get_roundtrip() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    let cp = mk_checkpoint("cp-1", exec, 1_700_000_000);
    store.save(&cp).await.unwrap();
    let back = store.get(&cp.id).await.unwrap();
    assert_eq!(back, cp);
}

#[tokio::test]
async fn get_missing_returns_not_found() {
    let store = SqliteStore::in_memory().await.unwrap();
    let id = CheckpointId::new("nope").unwrap();
    let err = store.get(&id).await.unwrap_err();
    assert!(matches!(err, PersistenceError::NotFound { kind: "checkpoint", .. }));
}

#[tokio::test]
async fn latest_returns_most_recent_checkpoint_for_execution() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    store.save(&mk_checkpoint("old",  exec, 1_700_000_000)).await.unwrap();
    store.save(&mk_checkpoint("new",  exec, 1_700_000_100)).await.unwrap();
    let latest = store.latest(&exec).await.unwrap().unwrap();
    assert_eq!(latest.id.as_str(), "new");
}

#[tokio::test]
async fn prune_keeps_only_newest_n() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    for i in 0..5 {
        store
            .save(&mk_checkpoint(&format!("cp-{i}"), exec, 1_700_000_000 + i))
            .await
            .unwrap();
    }
    let report = store.prune(&exec, 2).await.unwrap();
    assert_eq!(report.artifacts_removed, 3);
    // Remaining should be the two newest.
    let page = store
        .list(&exec, PageRequest::first(10))
        .await
        .unwrap();
    let ids: Vec<_> = page.items.iter().map(|h| h.id.as_str().to_owned()).collect();
    assert_eq!(ids, vec!["cp-4".to_owned(), "cp-3".to_owned()]);
}

#[tokio::test]
async fn list_paginates() {
    let store = SqliteStore::in_memory().await.unwrap();
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    for i in 0..5 {
        store
            .save(&mk_checkpoint(&format!("cp-{i}"), exec, 1_700_000_000 + i))
            .await
            .unwrap();
    }
    let page1 = store.list(&exec, PageRequest::first(2)).await.unwrap();
    assert_eq!(page1.items.len(), 2);
    assert!(page1.next_token.is_some());
    let page2 = store
        .list(&exec, PageRequest::after(2, page1.next_token.unwrap()))
        .await
        .unwrap();
    assert_eq!(page2.items.len(), 2);
    assert!(page2.next_token.is_some());
    let page3 = store
        .list(&exec, PageRequest::after(2, page2.next_token.unwrap()))
        .await
        .unwrap();
    assert_eq!(page3.items.len(), 1);
    assert!(page3.next_token.is_none());
}
