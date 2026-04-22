// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockCheckpointStore` — in-memory implementation of
//! [`paramodel_persistence::CheckpointStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use paramodel_executor::{Checkpoint, CheckpointHandle, ExecutionId};
use paramodel_persistence::{
    CheckpointStore, CleanupReport, Page, PageRequest, PersistenceError,
    PersistenceResult,
};
use paramodel_plan::CheckpointId;

/// In-memory checkpoint store. Keyed by `CheckpointId`.
#[derive(Debug, Default)]
pub struct MockCheckpointStore {
    records: Mutex<BTreeMap<CheckpointId, Checkpoint>>,
}

impl MockCheckpointStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn handle(cp: &Checkpoint) -> CheckpointHandle {
        CheckpointHandle {
            id:         cp.id.clone(),
            created_at: cp.created_at,
            is_valid:   true,
        }
    }
}

#[async_trait]
impl CheckpointStore for MockCheckpointStore {
    async fn save(&self, checkpoint: &Checkpoint) -> PersistenceResult<()> {
        self.records
            .lock()
            .expect("poisoned")
            .insert(checkpoint.id.clone(), checkpoint.clone());
        Ok(())
    }

    async fn get(&self, id: &CheckpointId) -> PersistenceResult<Checkpoint> {
        self.records
            .lock()
            .expect("poisoned")
            .get(id)
            .cloned()
            .ok_or_else(|| PersistenceError::not_found("checkpoint", id.as_str()))
    }

    async fn delete(&self, id: &CheckpointId) -> PersistenceResult<()> {
        self.records.lock().expect("poisoned").remove(id);
        Ok(())
    }

    async fn latest(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<Checkpoint>> {
        Ok(self
            .records
            .lock()
            .expect("poisoned")
            .values()
            .filter(|c| &c.execution_id == execution)
            .max_by_key(|c| c.created_at)
            .cloned())
    }

    async fn list(
        &self,
        execution: &ExecutionId,
        page:      PageRequest,
    ) -> PersistenceResult<Page<CheckpointHandle>> {
        let page = page.clamped();
        let records = self.records.lock().expect("poisoned");
        let mut matches: Vec<&Checkpoint> = records
            .values()
            .filter(|c| &c.execution_id == execution)
            .collect();
        matches.sort_by_key(|c| std::cmp::Reverse(c.created_at));
        let take = page.size as usize;
        let items: Vec<CheckpointHandle> =
            matches.into_iter().take(take).map(Self::handle).collect();
        Ok(Page::terminal(items))
    }

    async fn prune(
        &self,
        execution:   &ExecutionId,
        keep_newest: u32,
    ) -> PersistenceResult<CleanupReport> {
        let mut records = self.records.lock().expect("poisoned");
        let mut by_ts: Vec<(CheckpointId, jiff::Timestamp)> = records
            .iter()
            .filter(|(_, c)| &c.execution_id == execution)
            .map(|(id, c)| (id.clone(), c.created_at))
            .collect();
        by_ts.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));

        let keep = keep_newest as usize;
        let to_drop: Vec<CheckpointId> =
            by_ts.into_iter().skip(keep).map(|(id, _)| id).collect();
        let removed = u64::try_from(to_drop.len()).unwrap_or(u64::MAX);
        for id in to_drop {
            records.remove(&id);
        }
        Ok(CleanupReport {
            artifacts_removed: removed,
            bytes_freed:       0,
        })
    }
}
