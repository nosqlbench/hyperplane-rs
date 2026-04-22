// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `JournalStore` trait (SRD-0012).
//!
//! Durable append-only log of `JournalEvent`s (defined in
//! `paramodel-executor`). Append is monotonic per execution; replay
//! and live subscription return `PersistenceStream<JournalEvent>`.

use async_trait::async_trait;
use paramodel_executor::{ExecutionId, JournalEvent, JournalSequence};

use crate::error::PersistenceResult;
use crate::page::PersistenceStream;

/// Durable store for executor journal events.
#[async_trait]
pub trait JournalStore: Send + Sync + 'static {
    /// Append one event. Backend commits the sequence before
    /// returning; concurrent `replay` / `subscribe` callers see the
    /// event once `append` resolves.
    async fn append(&self, event: &JournalEvent) -> PersistenceResult<()>;

    /// Append a batch of events. Backend writes atomically where
    /// possible.
    async fn append_batch(&self, events: &[JournalEvent]) -> PersistenceResult<()>;

    /// Replay events strictly after `after_sequence`. `None` starts
    /// at the beginning.
    async fn replay(
        &self,
        execution:      &ExecutionId,
        after_sequence: Option<JournalSequence>,
    ) -> PersistenceStream<JournalEvent>;

    /// Most recent event for an execution, or `None` if empty.
    async fn latest_event(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<JournalEvent>>;

    /// Highest sequence seen for an execution. Returns
    /// `JournalSequence::ZERO` when no events exist.
    async fn latest_sequence(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<JournalSequence>;

    /// Drop events with `sequence < before`. Callers must ensure the
    /// referenced checkpoint captures everything they prune.
    async fn truncate_before(
        &self,
        execution: &ExecutionId,
        before:    JournalSequence,
    ) -> PersistenceResult<()>;

    /// Drop every event for an execution.
    async fn delete_all(&self, execution: &ExecutionId) -> PersistenceResult<()>;

    /// Live-tail new events as they land. Stream closes when the
    /// caller drops it.
    async fn subscribe(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceStream<JournalEvent>;
}
