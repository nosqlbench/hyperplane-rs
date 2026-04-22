// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ArtifactStore` trait + supporting types (SRD-0012).
//!
//! Artifacts are saved under a `(execution, trial, element)` context
//! and addressed by an opaque [`ArtifactId`]. The store is
//! content-aware enough to expose a `RetentionPolicy` hook but otherwise
//! treats payloads as opaque bytes or URIs.

use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{ElementName, TrialId};
use paramodel_executor::{ArtifactContent, ExecutionId};
use paramodel_trials::ArtifactRef;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::PersistenceResult;
use crate::page::{Page, PageRequest, PersistenceStream};

// ---------------------------------------------------------------------------
// ArtifactId.
// ---------------------------------------------------------------------------

/// Storage-assigned identifier for one artifact record.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ArtifactId(Ulid);

impl ArtifactId {
    /// From a pre-generated ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }

    /// Borrow the inner ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> &Ulid {
        &self.0
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ---------------------------------------------------------------------------
// Context + record + save mode.
// ---------------------------------------------------------------------------

/// Coordinate under which an artifact is saved and looked up. Any
/// combination of the three optional fields may be `Some` — the
/// backend filters accordingly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactContext {
    /// Execution the artifact belongs to.
    pub execution: Option<ExecutionId>,
    /// Trial the artifact belongs to.
    pub trial:     Option<TrialId>,
    /// Element that produced the artifact.
    pub element:   Option<ElementName>,
}

/// What a `get` / `list` call returns — metadata only, not the bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// Storage id.
    pub id:         ArtifactId,
    /// Coordinate the artifact was saved under.
    pub context:    ArtifactContext,
    /// Logical descriptor (element, name, uri, content-type, size).
    pub artifact:   ArtifactRef,
    /// When the backend recorded the save.
    pub stored_at:  Timestamp,
    /// Bytes the backend actually stored.
    pub size_bytes: u64,
    /// Optional SHA-256 the backend computed for integrity (SRD-0012 D10).
    pub sha256:     Option<[u8; 32]>,
}

/// How the store should handle a second save at the same logical key.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SaveMode {
    /// Fail with `PersistenceError::Conflict` on duplicate.
    #[default]
    CreateNew,
    /// Replace the existing record in place.
    Replace,
    /// Keep the old record; append a versioned copy.
    Version,
}

// ---------------------------------------------------------------------------
// Retention policy + cleanup report.
// ---------------------------------------------------------------------------

/// Retention bounds the backend should enforce on demand.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Keep at most N newest artifacts per `ArtifactContext`.
    pub keep_newest:    Option<u32>,
    /// Drop artifacts older than this.
    pub max_age:        Option<Duration>,
    /// Cap on total bytes per `ArtifactContext`.
    pub max_size_bytes: Option<u64>,
}

/// Summary of a retention pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Number of artifact records removed.
    pub artifacts_removed: u64,
    /// Total bytes freed.
    pub bytes_freed:       u64,
}

// ---------------------------------------------------------------------------
// Trait.
// ---------------------------------------------------------------------------

/// Durable store for artifact blobs.
#[async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    /// Save an artifact and return its storage id.
    async fn save(
        &self,
        context:  &ArtifactContext,
        artifact: &ArtifactRef,
        content:  ArtifactContent,
        mode:     SaveMode,
    ) -> PersistenceResult<ArtifactId>;

    /// Metadata for one artifact.
    async fn get(&self, id: &ArtifactId) -> PersistenceResult<ArtifactRecord>;

    /// Payload for one artifact.
    async fn download(&self, id: &ArtifactId) -> PersistenceResult<ArtifactContent>;

    /// List artifacts matching a context.
    async fn list(
        &self,
        context: &ArtifactContext,
        page:    PageRequest,
    ) -> PersistenceResult<Page<ArtifactRecord>>;

    /// Stream artifacts matching a context (no page bound).
    async fn stream(&self, context: &ArtifactContext) -> PersistenceStream<ArtifactRecord>;

    /// Delete one artifact.
    async fn delete(&self, id: &ArtifactId) -> PersistenceResult<()>;

    /// Enforce a retention policy right now. Scheduling is the
    /// caller's concern (SRD-0012 D13).
    async fn run_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> PersistenceResult<CleanupReport>;
}
