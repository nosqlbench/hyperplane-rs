// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel persistence — SQLite backend.
//!
//! [`SqliteStore`] implements every trait in
//! [`paramodel_persistence`] against a single file- or memory-backed
//! SQLite database. The schema favours JSON blobs for full domain
//! objects plus dedicated columns for indexable filter fields (ids,
//! fingerprints, status, timestamps). Callers typically construct
//! one `SqliteStore` and share it as `Arc<SqliteStore>`.

pub mod artifact_store;
pub mod checkpoint_store;
pub mod execution_repository;
pub mod journal_store;
pub mod journal_writer;
pub mod metadata_store;
pub mod providers;
pub mod result_store;
pub mod state_manager;
pub mod store;

pub use providers::{
    SqliteArtifactStoreProvider, SqliteCheckpointStoreProvider,
    SqliteExecutionRepositoryProvider, SqliteJournalStoreProvider,
    SqliteMetadataStoreProvider, SqliteResultStoreProvider,
};
pub use store::SqliteStore;
pub use paramodel_persistence as persistence;
