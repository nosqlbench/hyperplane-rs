// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! TCK provider shims for [`SqliteStore`].
//!
//! The paramodel-tck provider traits use a synchronous
//! `fresh() -> Self::Store`. `SqliteStore::in_memory()` is `async`,
//! so these providers bridge via `tokio::task::block_in_place` +
//! `Handle::block_on`. Tests that use them must therefore run under
//! `#[tokio::test(flavor = "multi_thread")]`.

use paramodel_tck::{
    ArtifactStoreProvider, CheckpointStoreProvider, ExecutionRepositoryProvider,
    JournalStoreProvider, MetadataStoreProvider, ResultStoreProvider,
};

use crate::SqliteStore;

fn spawn_sqlite() -> SqliteStore {
    let handle = tokio::runtime::Handle::current();
    tokio::task::block_in_place(|| {
        handle.block_on(async { SqliteStore::in_memory().await })
    })
    .expect("construct in-memory SqliteStore for TCK provider")
}

/// Provider that hands out fresh in-memory SqliteStore instances,
/// each a brand-new empty database.
#[derive(Debug, Default)]
pub struct SqliteJournalStoreProvider;
impl JournalStoreProvider for SqliteJournalStoreProvider {
    type Store = SqliteStore;
    fn fresh(&self) -> Self::Store {
        spawn_sqlite()
    }
}

/// Provider for `CheckpointStore` conformance tests.
#[derive(Debug, Default)]
pub struct SqliteCheckpointStoreProvider;
impl CheckpointStoreProvider for SqliteCheckpointStoreProvider {
    type Store = SqliteStore;
    fn fresh(&self) -> Self::Store {
        spawn_sqlite()
    }
}

/// Provider for `ExecutionRepository` conformance tests.
#[derive(Debug, Default)]
pub struct SqliteExecutionRepositoryProvider;
impl ExecutionRepositoryProvider for SqliteExecutionRepositoryProvider {
    type Repository = SqliteStore;
    fn fresh(&self) -> Self::Repository {
        spawn_sqlite()
    }
}

/// Provider for `MetadataStore` conformance tests.
#[derive(Debug, Default)]
pub struct SqliteMetadataStoreProvider;
impl MetadataStoreProvider for SqliteMetadataStoreProvider {
    type Store = SqliteStore;
    fn fresh(&self) -> Self::Store {
        spawn_sqlite()
    }
}

/// Provider for `ArtifactStore` conformance tests.
#[derive(Debug, Default)]
pub struct SqliteArtifactStoreProvider;
impl ArtifactStoreProvider for SqliteArtifactStoreProvider {
    type Store = SqliteStore;
    fn fresh(&self) -> Self::Store {
        spawn_sqlite()
    }
}

/// Provider for `ResultStore` conformance tests.
#[derive(Debug, Default)]
pub struct SqliteResultStoreProvider;
impl ResultStoreProvider for SqliteResultStoreProvider {
    type Store = SqliteStore;
    fn fresh(&self) -> Self::Store {
        spawn_sqlite()
    }
}
