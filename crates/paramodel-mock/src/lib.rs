// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::significant_drop_tightening,
    reason = "in-memory mock; short-lived guards don't warrant scope surgery"
)]
#![allow(
    clippy::needless_collect,
    reason = "we collect to drop the Mutex guard before handing the iterator to futures::stream::iter"
)]

//! In-memory reference implementations for every paramodel runtime
//! and persistence trait.
//!
//! Runtime-side mocks alias the reference impls that already live in
//! [`paramodel_executor`]:
//!
//! - [`MockOrchestrator`]
//! - [`MockScheduler`]
//! - [`MockResourceManager`]
//! - [`MockArtifactCollector`]
//! - [`MockStateManager`]
//! - [`MockJournalWriter`]
//!
//! Persistence-side mocks live in this crate since
//! [`paramodel_persistence`] ships trait surfaces only:
//!
//! - [`MockArtifactStore`]
//! - [`MockJournalStore`]
//!
//! Additional persistence mocks (`MockCheckpointStore`,
//! `MockExecutionRepository`, `MockMetadataStore`, `MockResultStore`)
//! land alongside the full TCK conformance suite.

pub mod artifact_store;
pub mod barrier;
pub mod checkpoint_store;
pub mod execution_repository;
pub mod journal_store;
pub mod metadata_store;
pub mod result_store;

pub use artifact_store::MockArtifactStore;
pub use barrier::MockBarrierHandle;
pub use checkpoint_store::MockCheckpointStore;
pub use execution_repository::MockExecutionRepository;
pub use journal_store::MockJournalStore;
pub use metadata_store::MockMetadataStore;
pub use result_store::MockResultStore;

/// Alias for [`paramodel_executor::DefaultOrchestrator`].
pub type MockOrchestrator = paramodel_executor::DefaultOrchestrator;

/// Alias for [`paramodel_executor::DefaultScheduler`].
pub type MockScheduler = paramodel_executor::DefaultScheduler;

/// Alias for [`paramodel_executor::DefaultResourceManager`].
pub type MockResourceManager = paramodel_executor::DefaultResourceManager;

/// Alias for [`paramodel_executor::InMemoryArtifactCollector`].
pub type MockArtifactCollector = paramodel_executor::InMemoryArtifactCollector;

/// Alias for [`paramodel_executor::InMemoryStateManager`].
pub type MockStateManager = paramodel_executor::InMemoryStateManager;

/// Alias for [`paramodel_executor::InMemoryJournalWriter`].
pub type MockJournalWriter = paramodel_executor::InMemoryJournalWriter;
