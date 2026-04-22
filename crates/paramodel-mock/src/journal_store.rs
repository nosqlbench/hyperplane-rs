// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockJournalStore` — in-memory implementation of
//! [`paramodel_persistence::JournalStore`].
//!
//! `subscribe` returns a snapshot stream (no live tailing). Adopters
//! that need live subscription should use a real backend; the mock is
//! here to validate trait semantics and back tests.

use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use paramodel_executor::{ExecutionId, JournalEvent, JournalSequence};
use paramodel_persistence::{JournalStore, PersistenceResult, PersistenceStream};

/// Simple in-memory `JournalStore` used by the TCK and by adopters.
#[derive(Debug, Default)]
pub struct MockJournalStore {
    events: Mutex<Vec<JournalEvent>>,
}

impl MockJournalStore {
    /// Construct an empty store.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Convenience: snapshot the whole log (useful in tests).
    #[must_use]
    pub fn snapshot(&self) -> Vec<JournalEvent> {
        self.events.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl JournalStore for MockJournalStore {
    async fn append(&self, event: &JournalEvent) -> PersistenceResult<()> {
        self.events.lock().expect("poisoned").push(event.clone());
        Ok(())
    }

    async fn append_batch(
        &self,
        events: &[JournalEvent],
    ) -> PersistenceResult<()> {
        self.events
            .lock()
            .expect("poisoned")
            .extend(events.iter().cloned());
        Ok(())
    }

    async fn replay(
        &self,
        execution:      &ExecutionId,
        after_sequence: Option<JournalSequence>,
    ) -> PersistenceStream<JournalEvent> {
        let gate = after_sequence.map_or(0u64, |s| s.get());
        let eid = *execution;
        let snapshot: Vec<JournalEvent> = self
            .events
            .lock()
            .expect("poisoned")
            .iter()
            .filter(|e| e.execution_id == eid && e.sequence.get() > gate)
            .cloned()
            .collect();
        stream::iter(snapshot.into_iter().map(Ok)).boxed()
    }

    async fn latest_event(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<JournalEvent>> {
        Ok(self
            .events
            .lock()
            .expect("poisoned")
            .iter()
            .rev()
            .find(|e| &e.execution_id == execution)
            .cloned())
    }

    async fn latest_sequence(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<JournalSequence> {
        Ok(self
            .events
            .lock()
            .expect("poisoned")
            .iter()
            .filter(|e| &e.execution_id == execution)
            .map(|e| e.sequence)
            .max()
            .unwrap_or(JournalSequence::ZERO))
    }

    async fn truncate_before(
        &self,
        execution: &ExecutionId,
        before:    JournalSequence,
    ) -> PersistenceResult<()> {
        self.events
            .lock()
            .expect("poisoned")
            .retain(|e| &e.execution_id != execution || e.sequence >= before);
        Ok(())
    }

    async fn delete_all(&self, execution: &ExecutionId) -> PersistenceResult<()> {
        self.events
            .lock()
            .expect("poisoned")
            .retain(|e| &e.execution_id != execution);
        Ok(())
    }

    async fn subscribe(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceStream<JournalEvent> {
        // Snapshot-only: return whatever's currently recorded.
        // Real live-tailing belongs to a richer backend.
        self.replay(execution, None).await
    }
}
