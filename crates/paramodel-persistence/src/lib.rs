// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel persistence layer (SRD-0012).
//!
//! Six trait surfaces cover every durable artefact paramodel produces:
//!
//! - [`ArtifactStore`] — artifact blobs from `SaveOutput` steps.
//! - [`CheckpointStore`] — executor checkpoint snapshots.
//! - [`ExecutionRepository`] — compiled plans + aggregate results.
//! - [`JournalStore`] — append-only stream of `JournalEvent`s.
//! - [`MetadataStore`] — catalog metadata and scoped label / tag
//!   updates.
//! - [`ResultStore`] — per-trial `TrialResult` records with a typed
//!   filter + aggregation surface.
//!
//! The traits commit to *shape* only. Backends live in separate
//! crates (e.g. `paramodel-store-sqlite`); this crate never links
//! against a database driver.

pub mod artifact_store;
pub mod checkpoint_store;
pub mod error;
pub mod execution_repository;
pub mod journal_store;
pub mod metadata_store;
pub mod page;
pub mod result_store;
mod result_store_serde;

pub use artifact_store::{
    ArtifactContext, ArtifactId, ArtifactRecord, ArtifactStore, CleanupReport,
    RetentionPolicy, SaveMode,
};
pub use checkpoint_store::CheckpointStore;
pub use error::{PersistenceError, PersistenceResult};
pub use execution_repository::{
    ExecutionFilter, ExecutionPlanSummary, ExecutionRepository, ExecutionSummary,
    PlanFilter,
};
pub use journal_store::JournalStore;
pub use metadata_store::{MetadataScope, MetadataStore};
pub use page::{
    MAX_PAGE_SIZE, Page, PageRequest, PageToken, PersistenceStream,
};
pub use result_store::{
    AggregateResult, Aggregation, Comparison, GroupDimension, ResultFilter, ResultStore,
    TrialCodePattern,
};
