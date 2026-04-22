// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! End-to-end exercise of the TCK's declarative macros.
//!
//! This is exactly what an adopter would write: point each macro at
//! a provider, get every conformance check expanded as a
//! `#[tokio::test]`. Each macro lives in its own module so the
//! expanded test function names don't collide.

use paramodel_tck::{
    MockArtifactCollectorProvider, MockArtifactStoreProvider,
    MockCheckpointStoreProvider, MockCompilerProvider,
    MockExecutionRepositoryProvider, MockExecutorProvider, MockJournalStoreProvider,
    MockJournalWriterProvider, MockMetadataStoreProvider, MockOrchestratorProvider,
    MockResourceManagerProvider, MockResultStoreProvider, MockStateManagerProvider,
};

mod journal {
    use super::MockJournalStoreProvider;
    paramodel_tck::run_journal_store!(MockJournalStoreProvider);
}

mod checkpoint {
    use super::MockCheckpointStoreProvider;
    paramodel_tck::run_checkpoint_store!(MockCheckpointStoreProvider);
}

mod metadata {
    use super::MockMetadataStoreProvider;
    paramodel_tck::run_metadata_store!(MockMetadataStoreProvider);
}

mod result {
    use super::MockResultStoreProvider;
    paramodel_tck::run_result_store!(MockResultStoreProvider);
}

mod artifact {
    use super::MockArtifactStoreProvider;
    paramodel_tck::run_artifact_store!(MockArtifactStoreProvider);
}

mod execution_repo {
    use super::MockExecutionRepositoryProvider;
    paramodel_tck::run_execution_repository!(MockExecutionRepositoryProvider);
}

mod compiler_area {
    use super::MockCompilerProvider;
    paramodel_tck::run_compiler!(MockCompilerProvider);
}

mod journal_writer_area {
    use super::MockJournalWriterProvider;
    paramodel_tck::run_journal_writer!(MockJournalWriterProvider);
}

mod executor_area {
    use super::MockExecutorProvider;
    paramodel_tck::run_executor!(MockExecutorProvider);
}

mod orchestrator_area {
    use super::MockOrchestratorProvider;
    paramodel_tck::run_orchestrator!(MockOrchestratorProvider);
}

mod resource_manager_area {
    use super::MockResourceManagerProvider;
    paramodel_tck::run_resource_manager!(MockResourceManagerProvider);
}

mod artifact_collector_area {
    use super::MockArtifactCollectorProvider;
    paramodel_tck::run_artifact_collector!(MockArtifactCollectorProvider);
}

mod state_manager_area {
    use super::MockStateManagerProvider;
    paramodel_tck::run_state_manager!(MockStateManagerProvider);
}

// And the all-in-one dispatcher.
paramodel_tck::run_all_persistence! {
    artifact       = MockArtifactStoreProvider,
    checkpoint     = MockCheckpointStoreProvider,
    execution_repo = MockExecutionRepositoryProvider,
    journal        = MockJournalStoreProvider,
    metadata       = MockMetadataStoreProvider,
    result         = MockResultStoreProvider,
}
