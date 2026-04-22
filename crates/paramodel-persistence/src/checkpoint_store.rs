// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `CheckpointStore` trait (SRD-0012).
//!
//! `Checkpoint` and `CheckpointHandle` live in
//! [`paramodel_executor`]. The store persists full snapshots, and
//! exposes lightweight handles for catalog browsing.

use async_trait::async_trait;
use paramodel_executor::{Checkpoint, CheckpointHandle, ExecutionId};
use paramodel_plan::CheckpointId;

use crate::artifact_store::CleanupReport;
use crate::error::PersistenceResult;
use crate::page::{Page, PageRequest};

/// Durable store for executor checkpoints.
#[async_trait]
pub trait CheckpointStore: Send + Sync + 'static {
    /// Persist a full snapshot. Idempotent on `checkpoint.id`.
    async fn save(&self, checkpoint: &Checkpoint) -> PersistenceResult<()>;

    /// Load a full snapshot by id.
    async fn get(&self, id: &CheckpointId) -> PersistenceResult<Checkpoint>;

    /// Remove one checkpoint.
    async fn delete(&self, id: &CheckpointId) -> PersistenceResult<()>;

    /// Most recent checkpoint for an execution, or `None` if none
    /// exist.
    async fn latest(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<Checkpoint>>;

    /// List checkpoints (handles only) for an execution.
    async fn list(
        &self,
        execution: &ExecutionId,
        page:      PageRequest,
    ) -> PersistenceResult<Page<CheckpointHandle>>;

    /// Keep the `keep_newest` most recent checkpoints for the
    /// execution; drop the rest.
    async fn prune(
        &self,
        execution:   &ExecutionId,
        keep_newest: u32,
    ) -> PersistenceResult<CleanupReport>;
}
