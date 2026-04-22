// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::CheckpointStore`].

use jiff::Timestamp;
use paramodel_elements::Fingerprint;
use paramodel_executor::{Checkpoint, CheckpointState, ExecutionId};
use paramodel_persistence::{CheckpointStore, PageRequest};
use paramodel_plan::CheckpointId;
use ulid::Ulid;

use crate::providers::CheckpointStoreProvider;

fn make_checkpoint(execution: ExecutionId, name: &str, at: Timestamp) -> Checkpoint {
    Checkpoint {
        id:               CheckpointId::new(name).expect("valid id"),
        execution_id:     execution,
        plan_fingerprint: Fingerprint::of(b"plan"),
        created_at:       at,
        state:            CheckpointState::default(),
        fingerprint:      Fingerprint::of(name.as_bytes()),
    }
}

fn fresh_execution_id() -> ExecutionId {
    ExecutionId::from_ulid(Ulid::new())
}

fn ts(n: i64) -> Timestamp {
    Timestamp::from_second(1_700_000_000 + n).unwrap()
}

/// Save + get round-trips the full checkpoint.
pub async fn tck_checkpoint_save_and_get<P: CheckpointStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    let cp = make_checkpoint(exec, "cp1", ts(1));
    store.save(&cp).await.unwrap();
    let got = store.get(&cp.id).await.unwrap();
    assert_eq!(got, cp);
}

/// `latest` returns the most-recent-by-timestamp checkpoint.
pub async fn tck_checkpoint_latest<P: CheckpointStoreProvider>(provider: &P) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    store.save(&make_checkpoint(exec, "cp1", ts(1))).await.unwrap();
    store.save(&make_checkpoint(exec, "cp2", ts(2))).await.unwrap();
    store.save(&make_checkpoint(exec, "cp3", ts(3))).await.unwrap();

    let latest = store.latest(&exec).await.unwrap().expect("has latest");
    assert_eq!(latest.id.as_str(), "cp3");
}

/// `prune` keeps the N newest checkpoints per execution.
pub async fn tck_checkpoint_prune<P: CheckpointStoreProvider>(provider: &P) {
    let store = provider.fresh();
    let exec = fresh_execution_id();
    for i in 1..=5i64 {
        store
            .save(&make_checkpoint(exec, &format!("cp{i}"), ts(i)))
            .await
            .unwrap();
    }
    let report = store.prune(&exec, 2).await.unwrap();
    assert_eq!(report.artifacts_removed, 3);

    let page = store.list(&exec, PageRequest::first(10)).await.unwrap();
    assert_eq!(page.items.len(), 2);
    let ids: Vec<String> = page
        .items
        .iter()
        .map(|h| h.id.as_str().to_owned())
        .collect();
    assert!(ids.contains(&"cp4".to_owned()));
    assert!(ids.contains(&"cp5".to_owned()));
}

/// Per-execution isolation: prune / latest only affect the target
/// execution.
pub async fn tck_checkpoint_per_execution_isolation<P: CheckpointStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let a = fresh_execution_id();
    let b = fresh_execution_id();
    store.save(&make_checkpoint(a, "a1", ts(1))).await.unwrap();
    store.save(&make_checkpoint(b, "b1", ts(1))).await.unwrap();

    assert_eq!(
        store.latest(&a).await.unwrap().map(|c| c.id),
        Some(CheckpointId::new("a1").unwrap()),
    );
    assert_eq!(
        store.latest(&b).await.unwrap().map(|c| c.id),
        Some(CheckpointId::new("b1").unwrap()),
    );

    store.delete(&CheckpointId::new("a1").unwrap()).await.unwrap();
    assert!(store.latest(&a).await.unwrap().is_none());
    assert!(store.latest(&b).await.unwrap().is_some());
}
