// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! File-backed persistence round-trip. Proves `SqliteStore::open(path)`
//! actually durably persists state across a process-lifecycle boundary
//! (store dropped + reopened against the same file).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures::StreamExt;
use jiff::Timestamp;
use paramodel_elements::Fingerprint;
use paramodel_executor::{
    Checkpoint, CheckpointState, ExecutionId, JournalEvent, JournalEventKind,
    JournalSequence, ResourceUsage, SchedulerState,
};
use paramodel_persistence::{CheckpointStore, JournalStore};
use paramodel_plan::CheckpointId;
use paramodel_store_sqlite::SqliteStore;
use tempfile::tempdir;
use ulid::Ulid;

fn journal_event(seq: u64, exec: ExecutionId, ts_secs: i64) -> JournalEvent {
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

fn checkpoint_of(id: &str, exec: ExecutionId, ts: i64) -> Checkpoint {
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
    let fp = Fingerprint::of(b"fp");
    Checkpoint {
        id:               CheckpointId::new(id).unwrap(),
        execution_id:     exec,
        plan_fingerprint: fp,
        created_at:       Timestamp::from_second(ts).unwrap(),
        state,
        fingerprint:      fp,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_survives_store_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("paramodel.db");
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));

    // First session: open, write 3 events, drop the store.
    {
        let store = SqliteStore::open(&path).await.unwrap();
        for i in 1..=3 {
            store
                .append(&journal_event(i, exec, 1_700_000_000 + i as i64))
                .await
                .unwrap();
        }
    }

    // Second session: reopen the same path. Events must still be there.
    let store = SqliteStore::open(&path).await.unwrap();
    let events: Vec<JournalEvent> = store
        .replay(&exec, None)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(events.len(), 3);
    let seqs: Vec<u64> = events.iter().map(|e| e.sequence.get()).collect();
    assert_eq!(seqs, vec![1, 2, 3]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_survives_store_reopen() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("paramodel.db");
    let exec = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
    let cp = checkpoint_of("resume-me", exec, 1_700_000_000);

    {
        let store = SqliteStore::open(&path).await.unwrap();
        store.save(&cp).await.unwrap();
    }

    let store = SqliteStore::open(&path).await.unwrap();
    let back = store.get(&cp.id).await.unwrap();
    assert_eq!(back, cp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrations_are_idempotent_across_reopens() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("paramodel.db");
    // Four separate opens on the same file — migrations must be
    // idempotent (all tables created with `IF NOT EXISTS`).
    for _ in 0..4 {
        let _store = SqliteStore::open(&path).await.unwrap();
    }
    // Fifth open still works.
    let _store = SqliteStore::open(&path).await.unwrap();
}
