// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Implementation-provider trait family.
//!
//! Each paramodel trait the TCK targets has a tiny factory trait the
//! adopter implements. The TCK's conformance functions accept one or
//! more providers and use them to mint fresh instances for each
//! check. Adopters implement only the providers for the traits they
//! supply; the rest fall back to `paramodel-mock` equivalents.

use paramodel_compiler::{Compiler, CompilerOptions};
use std::collections::BTreeSet;

use paramodel_executor::{
    ArtifactCollector, BarrierHandle, ExecutionStateManager, Executor, JournalWriter,
    Orchestrator, ResourceManager,
};
use paramodel_plan::{BarrierId, StepId};
use paramodel_persistence::{
    ArtifactStore, CheckpointStore, ExecutionRepository, JournalStore, MetadataStore,
    ResultStore,
};

/// Provider for [`Compiler`] implementations under test.
pub trait CompilerProvider: Send + Sync + 'static {
    /// Concrete compiler type the provider mints.
    type Compiler: Compiler;

    /// Fresh instance constructed with `options`.
    fn compiler(&self, options: CompilerOptions) -> Self::Compiler;
}

/// Provider for [`Executor`] implementations under test.
pub trait ExecutorProvider: Send + Sync + 'static {
    /// Concrete executor type the provider mints.
    type Executor: Executor;

    /// Fresh executor. The provider is responsible for constructing
    /// and wiring its own orchestrator / scheduler / journal / … —
    /// unlike upstream we don't pass those in, because the trait's
    /// shape varies across implementations.
    fn executor(&self) -> Self::Executor;
}

/// Provider for [`ArtifactStore`] implementations under test.
pub trait ArtifactStoreProvider: Send + Sync + 'static {
    /// Concrete store type.
    type Store: ArtifactStore;
    /// Fresh, empty store.
    fn fresh(&self) -> Self::Store;
}

/// Provider for [`CheckpointStore`] implementations under test.
pub trait CheckpointStoreProvider: Send + Sync + 'static {
    /// Concrete store type.
    type Store: CheckpointStore;
    /// Fresh, empty store.
    fn fresh(&self) -> Self::Store;
}

/// Provider for [`ExecutionRepository`] implementations under test.
pub trait ExecutionRepositoryProvider: Send + Sync + 'static {
    /// Concrete repository type.
    type Repository: ExecutionRepository;
    /// Fresh, empty repository.
    fn fresh(&self) -> Self::Repository;
}

/// Provider for [`JournalStore`] implementations under test.
pub trait JournalStoreProvider: Send + Sync + 'static {
    /// Concrete store type.
    type Store: JournalStore;
    /// Fresh, empty store.
    fn fresh(&self) -> Self::Store;
}

/// Provider for [`MetadataStore`] implementations under test.
pub trait MetadataStoreProvider: Send + Sync + 'static {
    /// Concrete store type.
    type Store: MetadataStore;
    /// Fresh, empty store.
    fn fresh(&self) -> Self::Store;
}

/// Provider for [`ResultStore`] implementations under test.
pub trait ResultStoreProvider: Send + Sync + 'static {
    /// Concrete store type.
    type Store: ResultStore;
    /// Fresh, empty store.
    fn fresh(&self) -> Self::Store;
}

/// Provider for [`JournalWriter`] implementations under test
/// (executor-side ephemeral journal).
pub trait JournalWriterProvider: Send + Sync + 'static {
    /// Concrete writer type.
    type Writer: JournalWriter;
    /// Fresh, empty writer.
    fn fresh(&self) -> Self::Writer;
}

/// Provider for [`Orchestrator`] implementations under test.
pub trait OrchestratorProvider: Send + Sync + 'static {
    /// Concrete orchestrator type.
    type Orchestrator: Orchestrator;
    /// Fresh orchestrator (no runtimes registered).
    fn fresh(&self) -> Self::Orchestrator;
}

/// Provider for [`ResourceManager`] implementations under test.
pub trait ResourceManagerProvider: Send + Sync + 'static {
    /// Concrete resource-manager type.
    type Manager: ResourceManager;
    /// Fresh manager.
    fn fresh(&self) -> Self::Manager;
}

/// Provider for [`ArtifactCollector`] implementations under test
/// (executor-side ephemeral collector).
pub trait ArtifactCollectorProvider: Send + Sync + 'static {
    /// Concrete collector type.
    type Collector: ArtifactCollector;
    /// Fresh collector.
    fn fresh(&self) -> Self::Collector;
}

/// Provider for [`ExecutionStateManager`] implementations under test.
pub trait ExecutionStateManagerProvider: Send + Sync + 'static {
    /// Concrete manager type.
    type Manager: ExecutionStateManager;
    /// Fresh manager.
    fn fresh(&self) -> Self::Manager;
}

/// Provider for [`BarrierHandle`] implementations under test.
pub trait BarrierHandleProvider: Send + Sync + 'static {
    /// Concrete handle type.
    type Handle: BarrierHandle;
    /// Fresh handle with the given id and dependency set.
    fn fresh(&self, id: BarrierId, deps: BTreeSet<StepId>) -> Self::Handle;
}

// ---------------------------------------------------------------------------
// Default providers backed by paramodel-mock.
// ---------------------------------------------------------------------------

/// Default [`CompilerProvider`] backed by
/// [`paramodel_compiler::DefaultCompiler`].
#[derive(Debug, Default)]
pub struct MockCompilerProvider;

impl CompilerProvider for MockCompilerProvider {
    type Compiler = paramodel_compiler::DefaultCompiler;
    fn compiler(&self, options: CompilerOptions) -> Self::Compiler {
        paramodel_compiler::DefaultCompiler::new(options)
    }
}

/// Default [`ArtifactStoreProvider`] backed by
/// [`paramodel_mock::MockArtifactStore`].
#[derive(Debug, Default)]
pub struct MockArtifactStoreProvider;

impl ArtifactStoreProvider for MockArtifactStoreProvider {
    type Store = paramodel_mock::MockArtifactStore;
    fn fresh(&self) -> Self::Store {
        paramodel_mock::MockArtifactStore::new()
    }
}

/// Default [`JournalStoreProvider`] backed by
/// [`paramodel_mock::MockJournalStore`].
#[derive(Debug, Default)]
pub struct MockJournalStoreProvider;

impl JournalStoreProvider for MockJournalStoreProvider {
    type Store = paramodel_mock::MockJournalStore;
    fn fresh(&self) -> Self::Store {
        paramodel_mock::MockJournalStore::new()
    }
}

/// Default [`CheckpointStoreProvider`] backed by
/// [`paramodel_mock::MockCheckpointStore`].
#[derive(Debug, Default)]
pub struct MockCheckpointStoreProvider;

impl CheckpointStoreProvider for MockCheckpointStoreProvider {
    type Store = paramodel_mock::MockCheckpointStore;
    fn fresh(&self) -> Self::Store {
        paramodel_mock::MockCheckpointStore::new()
    }
}

/// Default [`ExecutionRepositoryProvider`] backed by
/// [`paramodel_mock::MockExecutionRepository`].
#[derive(Debug, Default)]
pub struct MockExecutionRepositoryProvider;

impl ExecutionRepositoryProvider for MockExecutionRepositoryProvider {
    type Repository = paramodel_mock::MockExecutionRepository;
    fn fresh(&self) -> Self::Repository {
        paramodel_mock::MockExecutionRepository::new()
    }
}

/// Default [`MetadataStoreProvider`] backed by
/// [`paramodel_mock::MockMetadataStore`].
#[derive(Debug, Default)]
pub struct MockMetadataStoreProvider;

impl MetadataStoreProvider for MockMetadataStoreProvider {
    type Store = paramodel_mock::MockMetadataStore;
    fn fresh(&self) -> Self::Store {
        paramodel_mock::MockMetadataStore::new()
    }
}

/// Default [`ResultStoreProvider`] backed by
/// [`paramodel_mock::MockResultStore`].
#[derive(Debug, Default)]
pub struct MockResultStoreProvider;

impl ResultStoreProvider for MockResultStoreProvider {
    type Store = paramodel_mock::MockResultStore;
    fn fresh(&self) -> Self::Store {
        paramodel_mock::MockResultStore::new()
    }
}

/// Default [`JournalWriterProvider`] backed by
/// [`paramodel_mock::MockJournalWriter`]
/// (== `paramodel_executor::InMemoryJournalWriter`).
#[derive(Debug, Default)]
pub struct MockJournalWriterProvider;

impl JournalWriterProvider for MockJournalWriterProvider {
    type Writer = paramodel_mock::MockJournalWriter;
    fn fresh(&self) -> Self::Writer {
        paramodel_mock::MockJournalWriter::new()
    }
}

/// Default [`OrchestratorProvider`] backed by
/// [`paramodel_mock::MockOrchestrator`].
#[derive(Debug, Default)]
pub struct MockOrchestratorProvider;

impl OrchestratorProvider for MockOrchestratorProvider {
    type Orchestrator = paramodel_mock::MockOrchestrator;
    fn fresh(&self) -> Self::Orchestrator {
        paramodel_mock::MockOrchestrator::empty()
    }
}

/// Default [`ResourceManagerProvider`] backed by
/// [`paramodel_mock::MockResourceManager`].
#[derive(Debug, Default)]
pub struct MockResourceManagerProvider;

impl ResourceManagerProvider for MockResourceManagerProvider {
    type Manager = paramodel_mock::MockResourceManager;
    fn fresh(&self) -> Self::Manager {
        paramodel_mock::MockResourceManager::new()
    }
}

/// Default [`ArtifactCollectorProvider`] backed by
/// [`paramodel_mock::MockArtifactCollector`].
#[derive(Debug, Default)]
pub struct MockArtifactCollectorProvider;

impl ArtifactCollectorProvider for MockArtifactCollectorProvider {
    type Collector = paramodel_mock::MockArtifactCollector;
    fn fresh(&self) -> Self::Collector {
        paramodel_mock::MockArtifactCollector::new()
    }
}

/// Default [`ExecutionStateManagerProvider`] backed by
/// [`paramodel_mock::MockStateManager`] (the `NoopStateManager`).
#[derive(Debug, Default)]
pub struct MockStateManagerProvider;

impl ExecutionStateManagerProvider for MockStateManagerProvider {
    type Manager = paramodel_mock::MockStateManager;
    fn fresh(&self) -> Self::Manager {
        paramodel_mock::MockStateManager::new()
    }
}

/// Default [`BarrierHandleProvider`] backed by
/// [`paramodel_mock::MockBarrierHandle`].
#[derive(Debug, Default)]
pub struct MockBarrierHandleProvider;

impl BarrierHandleProvider for MockBarrierHandleProvider {
    type Handle = paramodel_mock::MockBarrierHandle;
    fn fresh(&self, id: BarrierId, deps: BTreeSet<StepId>) -> Self::Handle {
        paramodel_mock::MockBarrierHandle::new(id, deps)
    }
}

/// Default [`ExecutorProvider`] backed by [`paramodel_executor::DefaultExecutor`].
///
/// Each call to [`ExecutorProvider::executor`] builds a fresh
/// orchestrator / scheduler / journal / state-manager / observer
/// wired in-memory from [`paramodel_mock`], so tests don't share
/// state.
#[derive(Debug, Default)]
pub struct MockExecutorProvider;

impl ExecutorProvider for MockExecutorProvider {
    type Executor = paramodel_executor::DefaultExecutor;
    fn executor(&self) -> Self::Executor {
        use std::sync::Arc;
        paramodel_executor::DefaultExecutor::new(
            Arc::new(paramodel_mock::MockOrchestrator::empty()),
            Arc::new(paramodel_mock::MockScheduler::new()),
            Arc::new(paramodel_mock::MockStateManager::new()),
            Arc::new(paramodel_mock::MockJournalWriter::new()),
            Arc::new(paramodel_executor::NoopObserver),
        )
    }
}
