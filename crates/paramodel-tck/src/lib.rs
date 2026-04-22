// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::needless_collect,
    reason = "test helpers collect to drain futures streams for assertions"
)]

//! Paramodel Technology Compatibility Kit (TCK).
//!
//! Adopters of any paramodel trait — compiler, executor, or one of
//! the six persistence stores — plug their implementation into a
//! [`providers`] factory, invoke the relevant conformance check, and
//! get back either a pass or a targeted assertion failure.
//!
//! v0.1 scope ships:
//!
//! - The provider trait family ([`providers`]) with default
//!   `Mock*Provider`s backed by [`paramodel_mock`].
//! - Conformance checks for all six persistence traits:
//!   [`ArtifactStore`][paramodel_persistence::ArtifactStore],
//!   [`CheckpointStore`][paramodel_persistence::CheckpointStore],
//!   [`ExecutionRepository`][paramodel_persistence::ExecutionRepository],
//!   [`JournalStore`][paramodel_persistence::JournalStore],
//!   [`MetadataStore`][paramodel_persistence::MetadataStore],
//!   [`ResultStore`][paramodel_persistence::ResultStore].
//!
//! Each check is an `async` free function taking a provider; a later
//! slice wires them up through `run_all!` and per-area macros.

pub mod artifact_collector_tests;
pub mod artifact_store_tests;
pub mod barrier_tests;
pub mod checkpoint_store_tests;
pub mod compiler_tests;
pub mod execution_repository_tests;
pub mod executor_tests;
pub mod golden;
pub mod journal_store_tests;
pub mod journal_writer_tests;
pub mod macros;
pub mod metadata_store_tests;
pub mod orchestrator_tests;
pub mod providers;
pub mod resource_manager_tests;
pub mod result_store_tests;
pub mod state_manager_tests;

pub use providers::{
    ArtifactCollectorProvider, ArtifactStoreProvider, BarrierHandleProvider,
    CheckpointStoreProvider, CompilerProvider, ExecutionRepositoryProvider,
    ExecutionStateManagerProvider, ExecutorProvider, JournalStoreProvider,
    JournalWriterProvider, MetadataStoreProvider, MockArtifactCollectorProvider,
    MockArtifactStoreProvider, MockBarrierHandleProvider, MockCheckpointStoreProvider,
    MockCompilerProvider, MockExecutionRepositoryProvider, MockExecutorProvider,
    MockJournalStoreProvider, MockJournalWriterProvider, MockMetadataStoreProvider,
    MockOrchestratorProvider, MockResourceManagerProvider, MockResultStoreProvider,
    MockStateManagerProvider, OrchestratorProvider, ResourceManagerProvider,
    ResultStoreProvider,
};

#[cfg(test)]
mod mock_self_tests {
    //! Validate the TCK runs green against `paramodel-mock`. Any
    //! adopter's impl that breaks these checks has violated the
    //! trait's contract.

    use super::*;

    // ---- JournalStore ----
    #[tokio::test]
    async fn journal_store_mock_passes_append_then_replay() {
        let p = MockJournalStoreProvider;
        journal_store_tests::tck_journal_store_append_then_replay(&p).await;
    }
    #[tokio::test]
    async fn journal_store_mock_passes_replay_after_sequence() {
        let p = MockJournalStoreProvider;
        journal_store_tests::tck_journal_store_replay_after_sequence(&p).await;
    }
    #[tokio::test]
    async fn journal_store_mock_passes_latest_sequence() {
        let p = MockJournalStoreProvider;
        journal_store_tests::tck_journal_store_latest_sequence(&p).await;
    }
    #[tokio::test]
    async fn journal_store_mock_passes_truncate_before() {
        let p = MockJournalStoreProvider;
        journal_store_tests::tck_journal_store_truncate_before(&p).await;
    }
    #[tokio::test]
    async fn journal_store_mock_passes_per_execution_isolation() {
        let p = MockJournalStoreProvider;
        journal_store_tests::tck_journal_store_per_execution_isolation(&p).await;
    }

    // ---- CheckpointStore ----
    #[tokio::test]
    async fn checkpoint_mock_passes_save_and_get() {
        let p = MockCheckpointStoreProvider;
        checkpoint_store_tests::tck_checkpoint_save_and_get(&p).await;
    }
    #[tokio::test]
    async fn checkpoint_mock_passes_latest() {
        let p = MockCheckpointStoreProvider;
        checkpoint_store_tests::tck_checkpoint_latest(&p).await;
    }
    #[tokio::test]
    async fn checkpoint_mock_passes_prune() {
        let p = MockCheckpointStoreProvider;
        checkpoint_store_tests::tck_checkpoint_prune(&p).await;
    }
    #[tokio::test]
    async fn checkpoint_mock_passes_isolation() {
        let p = MockCheckpointStoreProvider;
        checkpoint_store_tests::tck_checkpoint_per_execution_isolation(&p).await;
    }

    // ---- MetadataStore ----
    #[tokio::test]
    async fn metadata_mock_passes_save_and_get() {
        let p = MockMetadataStoreProvider;
        metadata_store_tests::tck_metadata_save_and_get(&p).await;
    }
    #[tokio::test]
    async fn metadata_mock_passes_list_plans() {
        let p = MockMetadataStoreProvider;
        metadata_store_tests::tck_metadata_list_plans(&p).await;
    }
    #[tokio::test]
    async fn metadata_mock_passes_set_labels() {
        let p = MockMetadataStoreProvider;
        metadata_store_tests::tck_metadata_set_labels(&p).await;
    }

    // ---- ResultStore ----
    #[tokio::test]
    async fn result_mock_passes_save_and_get() {
        let p = MockResultStoreProvider;
        result_store_tests::tck_result_save_and_get(&p).await;
    }
    #[tokio::test]
    async fn result_mock_passes_query_by_status() {
        let p = MockResultStoreProvider;
        result_store_tests::tck_result_query_by_status(&p).await;
    }
    #[tokio::test]
    async fn result_mock_passes_count() {
        let p = MockResultStoreProvider;
        result_store_tests::tck_result_count(&p).await;
    }
    #[tokio::test]
    async fn result_mock_passes_stream() {
        let p = MockResultStoreProvider;
        result_store_tests::tck_result_stream(&p).await;
    }
    #[tokio::test]
    async fn result_mock_passes_aggregate() {
        let p = MockResultStoreProvider;
        result_store_tests::tck_result_aggregate(&p).await;
    }

    // ---- ArtifactStore ----
    #[tokio::test]
    async fn artifact_mock_passes_save_and_download() {
        let p = MockArtifactStoreProvider;
        artifact_store_tests::tck_artifact_save_and_download(&p).await;
    }
    #[tokio::test]
    async fn artifact_mock_passes_create_new_rejects_duplicate() {
        let p = MockArtifactStoreProvider;
        artifact_store_tests::tck_artifact_save_mode_create_new_rejects_duplicate(&p)
            .await;
    }
    #[tokio::test]
    async fn artifact_mock_passes_replace_overwrites() {
        let p = MockArtifactStoreProvider;
        artifact_store_tests::tck_artifact_save_mode_replace_overwrites(&p).await;
    }
    #[tokio::test]
    async fn artifact_mock_passes_list_by_context() {
        let p = MockArtifactStoreProvider;
        artifact_store_tests::tck_artifact_list_by_context(&p).await;
    }

    // ---- Compiler ----
    #[test]
    fn compiler_mock_passes_fingerprint_stable() {
        compiler_tests::tck_compiler_fingerprint_stable(&MockCompilerProvider);
    }
    #[test]
    fn compiler_mock_passes_graph_well_formed() {
        compiler_tests::tck_compiler_graph_well_formed(&MockCompilerProvider);
    }
    #[test]
    fn compiler_mock_passes_carries_source_fingerprint() {
        compiler_tests::tck_compiler_carries_source_fingerprint(&MockCompilerProvider);
    }

    // ---- JournalWriter ----
    #[tokio::test]
    async fn journal_writer_mock_passes_write_then_since() {
        let p = MockJournalWriterProvider;
        journal_writer_tests::tck_journal_writer_write_then_since(&p).await;
    }
    #[tokio::test]
    async fn journal_writer_mock_passes_since_gate() {
        let p = MockJournalWriterProvider;
        journal_writer_tests::tck_journal_writer_since_gate(&p).await;
    }
    #[tokio::test]
    async fn journal_writer_mock_passes_last_event() {
        let p = MockJournalWriterProvider;
        journal_writer_tests::tck_journal_writer_last_event(&p).await;
    }

    // ---- Executor ----
    #[tokio::test]
    async fn executor_mock_passes_dry_run_accepts_valid_plan() {
        let p = MockExecutorProvider;
        executor_tests::tck_executor_dry_run_accepts_valid_plan(&p).await;
    }
    #[tokio::test]
    async fn executor_mock_passes_execute_terminates() {
        let p = MockExecutorProvider;
        executor_tests::tck_executor_execute_terminates(&p).await;
    }
    #[tokio::test]
    async fn executor_mock_passes_checkpoints_idempotent() {
        let p = MockExecutorProvider;
        executor_tests::tck_executor_checkpoints_idempotent(&p).await;
    }

    // ---- Orchestrator ----
    #[tokio::test]
    async fn orchestrator_mock_passes_prepare_infallible() {
        let p = MockOrchestratorProvider;
        orchestrator_tests::tck_orchestrator_prepare_is_infallible(&p).await;
    }
    #[tokio::test]
    async fn orchestrator_mock_passes_missing_runtime_errors_cleanly() {
        let p = MockOrchestratorProvider;
        orchestrator_tests::tck_orchestrator_missing_runtime_errors_cleanly(&p).await;
    }
    #[tokio::test]
    async fn orchestrator_mock_passes_instance_state_none_for_unknown() {
        let p = MockOrchestratorProvider;
        orchestrator_tests::tck_orchestrator_instance_state_none_for_unknown(&p)
            .await;
    }
    #[tokio::test]
    async fn orchestrator_mock_passes_shutdown_infallible() {
        let p = MockOrchestratorProvider;
        orchestrator_tests::tck_orchestrator_shutdown_infallible(&p).await;
    }
    #[tokio::test]
    async fn orchestrator_mock_passes_await_ready_noop() {
        let p = MockOrchestratorProvider;
        orchestrator_tests::tck_orchestrator_await_ready_noop(&p).await;
    }

    // ---- ResourceManager ----
    #[tokio::test]
    async fn resource_manager_mock_passes_allocates_simple() {
        let p = MockResourceManagerProvider;
        resource_manager_tests::tck_resource_manager_allocates_simple_request(&p)
            .await;
    }
    #[tokio::test]
    async fn resource_manager_mock_passes_can_allocate_consistent() {
        let p = MockResourceManagerProvider;
        resource_manager_tests::tck_resource_manager_can_allocate_consistent(&p)
            .await;
    }
    #[tokio::test]
    async fn resource_manager_mock_passes_release_decrements() {
        let p = MockResourceManagerProvider;
        resource_manager_tests::tck_resource_manager_release_decrements_usage(&p)
            .await;
    }
    #[test]
    fn resource_manager_mock_passes_available_stable() {
        let p = MockResourceManagerProvider;
        resource_manager_tests::tck_resource_manager_available_snapshot_is_stable(&p);
    }
    #[test]
    fn resource_manager_mock_passes_pools_safe() {
        let p = MockResourceManagerProvider;
        resource_manager_tests::tck_resource_manager_pools_safe(&p);
    }

    // ---- BarrierHandle ----
    #[tokio::test]
    async fn barrier_mock_passes_empty_deps_satisfied() {
        let p = MockBarrierHandleProvider;
        barrier_tests::tck_barrier_empty_deps_is_immediately_satisfied(&p).await;
    }
    #[tokio::test]
    async fn barrier_mock_passes_marks_and_satisfies() {
        let p = MockBarrierHandleProvider;
        barrier_tests::tck_barrier_marks_and_satisfies(&p).await;
    }
    #[tokio::test]
    async fn barrier_mock_passes_fail_errors() {
        let p = MockBarrierHandleProvider;
        barrier_tests::tck_barrier_fail_errors_out(&p).await;
    }
    #[tokio::test]
    async fn barrier_mock_passes_release_forces_satisfied() {
        let p = MockBarrierHandleProvider;
        barrier_tests::tck_barrier_release_forces_satisfied(&p).await;
    }
    #[tokio::test]
    async fn barrier_mock_passes_await_with_timeout_times_out() {
        let p = MockBarrierHandleProvider;
        barrier_tests::tck_barrier_await_with_timeout_times_out(&p).await;
    }

    // ---- ExecutionStateManager ----
    #[tokio::test]
    async fn state_manager_mock_passes_record_and_checkpoint_infallible() {
        let p = MockStateManagerProvider;
        state_manager_tests::tck_state_manager_record_and_checkpoint_infallible(&p)
            .await;
    }
    #[tokio::test]
    async fn state_manager_mock_passes_recover_returns_default() {
        let p = MockStateManagerProvider;
        state_manager_tests::tck_state_manager_recover_returns_default(&p).await;
    }
    #[tokio::test]
    async fn state_manager_mock_passes_step_not_completed() {
        let p = MockStateManagerProvider;
        state_manager_tests::tck_state_manager_step_not_completed(&p).await;
    }
    #[tokio::test]
    async fn state_manager_mock_passes_lifecycle_infallible() {
        let p = MockStateManagerProvider;
        state_manager_tests::tck_state_manager_lifecycle_infallible(&p).await;
    }
    #[test]
    fn state_manager_mock_passes_idempotency_class_by_variant() {
        let p = MockStateManagerProvider;
        state_manager_tests::tck_state_manager_idempotency_class_by_variant(&p);
    }

    // ---- ArtifactCollector ----
    #[tokio::test]
    async fn collector_mock_passes_collect_then_stop() {
        let p = MockArtifactCollectorProvider;
        artifact_collector_tests::tck_collector_collect_then_stop(&p).await;
    }
    #[tokio::test]
    async fn collector_mock_passes_stop_without_start_errors() {
        let p = MockArtifactCollectorProvider;
        artifact_collector_tests::tck_collector_stop_without_start_errors(&p).await;
    }
    #[tokio::test]
    async fn collector_mock_passes_artifacts_accessor() {
        let p = MockArtifactCollectorProvider;
        artifact_collector_tests::tck_collector_artifacts_accessor(&p).await;
    }
    #[tokio::test]
    async fn collector_mock_passes_cleanup_returns_report() {
        let p = MockArtifactCollectorProvider;
        artifact_collector_tests::tck_collector_cleanup_returns_report(&p).await;
    }

    // ---- ExecutionRepository ----
    #[tokio::test]
    async fn execution_repo_mock_passes_save_and_get_plan() {
        let p = MockExecutionRepositoryProvider;
        execution_repository_tests::tck_execution_save_and_get_plan(&p).await;
    }
    #[tokio::test]
    async fn execution_repo_mock_passes_save_and_get_execution() {
        let p = MockExecutionRepositoryProvider;
        execution_repository_tests::tck_execution_save_and_get_execution(&p).await;
    }
    #[tokio::test]
    async fn execution_repo_mock_passes_list_plans_filter() {
        let p = MockExecutionRepositoryProvider;
        execution_repository_tests::tck_execution_list_plans_filter(&p).await;
    }
    #[tokio::test]
    async fn execution_repo_mock_passes_filter_status() {
        let p = MockExecutionRepositoryProvider;
        execution_repository_tests::tck_execution_filter_status(&p).await;
    }
}
