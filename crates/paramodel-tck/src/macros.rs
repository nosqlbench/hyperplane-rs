// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Declarative helpers that expand into `#[tokio::test]` wrappers
//! around the conformance-check functions.
//!
//! Per-area macros land a fixed set of tests into the caller's test
//! binary — no trait ceremony required:
//!
//! ```no_run
//! paramodel_tck::run_journal_store!(
//!     paramodel_tck::MockJournalStoreProvider
//! );
//! ```
//!
//! Callers wanting the full persistence sweep invoke every area
//! macro in turn; each one targets a separate store, so the
//! signatures are distinct.

/// Expand every journal-store conformance check as a
/// `#[tokio::test]`, driven by the supplied provider expression.
#[macro_export]
macro_rules! run_journal_store {
    ($provider:expr) => {
        #[::tokio::test]
        async fn journal_store_conformance_append_then_replay() {
            $crate::journal_store_tests::tck_journal_store_append_then_replay(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn journal_store_conformance_replay_after_sequence() {
            $crate::journal_store_tests::tck_journal_store_replay_after_sequence(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn journal_store_conformance_latest_sequence() {
            $crate::journal_store_tests::tck_journal_store_latest_sequence(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn journal_store_conformance_truncate_before() {
            $crate::journal_store_tests::tck_journal_store_truncate_before(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn journal_store_conformance_per_execution_isolation() {
            $crate::journal_store_tests::tck_journal_store_per_execution_isolation(
                &$provider,
            )
            .await;
        }
    };
}

/// Expand every checkpoint-store conformance check.
#[macro_export]
macro_rules! run_checkpoint_store {
    ($provider:expr) => {
        #[::tokio::test]
        async fn checkpoint_store_conformance_save_and_get() {
            $crate::checkpoint_store_tests::tck_checkpoint_save_and_get(&$provider)
                .await;
        }
        #[::tokio::test]
        async fn checkpoint_store_conformance_latest() {
            $crate::checkpoint_store_tests::tck_checkpoint_latest(&$provider).await;
        }
        #[::tokio::test]
        async fn checkpoint_store_conformance_prune() {
            $crate::checkpoint_store_tests::tck_checkpoint_prune(&$provider).await;
        }
        #[::tokio::test]
        async fn checkpoint_store_conformance_per_execution_isolation() {
            $crate::checkpoint_store_tests::tck_checkpoint_per_execution_isolation(
                &$provider,
            )
            .await;
        }
    };
}

/// Expand every metadata-store conformance check.
#[macro_export]
macro_rules! run_metadata_store {
    ($provider:expr) => {
        #[::tokio::test]
        async fn metadata_store_conformance_save_and_get() {
            $crate::metadata_store_tests::tck_metadata_save_and_get(&$provider).await;
        }
        #[::tokio::test]
        async fn metadata_store_conformance_list_plans() {
            $crate::metadata_store_tests::tck_metadata_list_plans(&$provider).await;
        }
        #[::tokio::test]
        async fn metadata_store_conformance_set_labels() {
            $crate::metadata_store_tests::tck_metadata_set_labels(&$provider).await;
        }
    };
}

/// Expand every result-store conformance check.
#[macro_export]
macro_rules! run_result_store {
    ($provider:expr) => {
        #[::tokio::test]
        async fn result_store_conformance_save_and_get() {
            $crate::result_store_tests::tck_result_save_and_get(&$provider).await;
        }
        #[::tokio::test]
        async fn result_store_conformance_query_by_status() {
            $crate::result_store_tests::tck_result_query_by_status(&$provider).await;
        }
        #[::tokio::test]
        async fn result_store_conformance_count() {
            $crate::result_store_tests::tck_result_count(&$provider).await;
        }
        #[::tokio::test]
        async fn result_store_conformance_stream() {
            $crate::result_store_tests::tck_result_stream(&$provider).await;
        }
        #[::tokio::test]
        async fn result_store_conformance_aggregate() {
            $crate::result_store_tests::tck_result_aggregate(&$provider).await;
        }
    };
}

/// Expand every artifact-store conformance check.
#[macro_export]
macro_rules! run_artifact_store {
    ($provider:expr) => {
        #[::tokio::test]
        async fn artifact_store_conformance_save_and_download() {
            $crate::artifact_store_tests::tck_artifact_save_and_download(&$provider)
                .await;
        }
        #[::tokio::test]
        async fn artifact_store_conformance_create_new_rejects_duplicate() {
            $crate::artifact_store_tests::tck_artifact_save_mode_create_new_rejects_duplicate(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn artifact_store_conformance_replace_overwrites() {
            $crate::artifact_store_tests::tck_artifact_save_mode_replace_overwrites(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn artifact_store_conformance_list_by_context() {
            $crate::artifact_store_tests::tck_artifact_list_by_context(&$provider)
                .await;
        }
    };
}

/// Expand every execution-repository conformance check.
#[macro_export]
macro_rules! run_execution_repository {
    ($provider:expr) => {
        #[::tokio::test]
        async fn execution_repo_conformance_save_and_get_plan() {
            $crate::execution_repository_tests::tck_execution_save_and_get_plan(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn execution_repo_conformance_save_and_get_execution() {
            $crate::execution_repository_tests::tck_execution_save_and_get_execution(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn execution_repo_conformance_list_plans_filter() {
            $crate::execution_repository_tests::tck_execution_list_plans_filter(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn execution_repo_conformance_filter_status() {
            $crate::execution_repository_tests::tck_execution_filter_status(
                &$provider,
            )
            .await;
        }
    };
}

/// Expand every compiler conformance check.
#[macro_export]
macro_rules! run_compiler {
    ($provider:expr) => {
        #[test]
        fn compiler_conformance_fingerprint_stable() {
            $crate::compiler_tests::tck_compiler_fingerprint_stable(&$provider);
        }
        #[test]
        fn compiler_conformance_graph_well_formed() {
            $crate::compiler_tests::tck_compiler_graph_well_formed(&$provider);
        }
        #[test]
        fn compiler_conformance_carries_source_fingerprint() {
            $crate::compiler_tests::tck_compiler_carries_source_fingerprint(&$provider);
        }
    };
}

/// Expand every journal-writer conformance check.
#[macro_export]
macro_rules! run_journal_writer {
    ($provider:expr) => {
        #[::tokio::test]
        async fn journal_writer_conformance_write_then_since() {
            $crate::journal_writer_tests::tck_journal_writer_write_then_since(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn journal_writer_conformance_since_gate() {
            $crate::journal_writer_tests::tck_journal_writer_since_gate(&$provider)
                .await;
        }
        #[::tokio::test]
        async fn journal_writer_conformance_last_event() {
            $crate::journal_writer_tests::tck_journal_writer_last_event(&$provider)
                .await;
        }
    };
}

/// Expand every executor conformance check.
#[macro_export]
macro_rules! run_executor {
    ($provider:expr) => {
        #[::tokio::test]
        async fn executor_conformance_dry_run_accepts_valid_plan() {
            $crate::executor_tests::tck_executor_dry_run_accepts_valid_plan(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn executor_conformance_execute_terminates() {
            $crate::executor_tests::tck_executor_execute_terminates(&$provider).await;
        }
        #[::tokio::test]
        async fn executor_conformance_checkpoints_idempotent() {
            $crate::executor_tests::tck_executor_checkpoints_idempotent(&$provider)
                .await;
        }
    };
}

/// Expand every orchestrator conformance check.
#[macro_export]
macro_rules! run_orchestrator {
    ($provider:expr) => {
        #[::tokio::test]
        async fn orchestrator_conformance_prepare_infallible() {
            $crate::orchestrator_tests::tck_orchestrator_prepare_is_infallible(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn orchestrator_conformance_missing_runtime_errors_cleanly() {
            $crate::orchestrator_tests::tck_orchestrator_missing_runtime_errors_cleanly(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn orchestrator_conformance_instance_state_none_for_unknown() {
            $crate::orchestrator_tests::tck_orchestrator_instance_state_none_for_unknown(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn orchestrator_conformance_shutdown_infallible() {
            $crate::orchestrator_tests::tck_orchestrator_shutdown_infallible(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn orchestrator_conformance_await_ready_noop() {
            $crate::orchestrator_tests::tck_orchestrator_await_ready_noop(&$provider)
                .await;
        }
    };
}

/// Expand every resource-manager conformance check.
#[macro_export]
macro_rules! run_resource_manager {
    ($provider:expr) => {
        #[::tokio::test]
        async fn resource_manager_conformance_allocates_simple() {
            $crate::resource_manager_tests::tck_resource_manager_allocates_simple_request(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn resource_manager_conformance_can_allocate_consistent() {
            $crate::resource_manager_tests::tck_resource_manager_can_allocate_consistent(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn resource_manager_conformance_release_decrements() {
            $crate::resource_manager_tests::tck_resource_manager_release_decrements_usage(
                &$provider,
            )
            .await;
        }
        #[test]
        fn resource_manager_conformance_available_stable() {
            $crate::resource_manager_tests::tck_resource_manager_available_snapshot_is_stable(
                &$provider,
            );
        }
        #[test]
        fn resource_manager_conformance_pools_safe() {
            $crate::resource_manager_tests::tck_resource_manager_pools_safe(
                &$provider,
            );
        }
    };
}

/// Expand every execution-state-manager conformance check.
#[macro_export]
macro_rules! run_state_manager {
    ($provider:expr) => {
        #[::tokio::test]
        async fn state_manager_conformance_record_and_checkpoint_infallible() {
            $crate::state_manager_tests::tck_state_manager_record_and_checkpoint_infallible(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn state_manager_conformance_recover_returns_default() {
            $crate::state_manager_tests::tck_state_manager_recover_returns_default(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn state_manager_conformance_step_not_completed() {
            $crate::state_manager_tests::tck_state_manager_step_not_completed(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn state_manager_conformance_lifecycle_infallible() {
            $crate::state_manager_tests::tck_state_manager_lifecycle_infallible(
                &$provider,
            )
            .await;
        }
        #[test]
        fn state_manager_conformance_idempotency_class_by_variant() {
            $crate::state_manager_tests::tck_state_manager_idempotency_class_by_variant(
                &$provider,
            );
        }
    };
}

/// Expand every artifact-collector conformance check.
#[macro_export]
macro_rules! run_artifact_collector {
    ($provider:expr) => {
        #[::tokio::test]
        async fn artifact_collector_conformance_collect_then_stop() {
            $crate::artifact_collector_tests::tck_collector_collect_then_stop(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn artifact_collector_conformance_stop_without_start_errors() {
            $crate::artifact_collector_tests::tck_collector_stop_without_start_errors(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn artifact_collector_conformance_artifacts_accessor() {
            $crate::artifact_collector_tests::tck_collector_artifacts_accessor(
                &$provider,
            )
            .await;
        }
        #[::tokio::test]
        async fn artifact_collector_conformance_cleanup_returns_report() {
            $crate::artifact_collector_tests::tck_collector_cleanup_returns_report(
                &$provider,
            )
            .await;
        }
    };
}

/// Expand every persistence conformance check at once. Takes one
/// provider per persistence store, each in its own module so the
/// expanded test function names don't collide.
///
/// ```no_run
/// paramodel_tck::run_all_persistence! {
///     artifact       = paramodel_tck::MockArtifactStoreProvider,
///     checkpoint     = paramodel_tck::MockCheckpointStoreProvider,
///     execution_repo = paramodel_tck::MockExecutionRepositoryProvider,
///     journal        = paramodel_tck::MockJournalStoreProvider,
///     metadata       = paramodel_tck::MockMetadataStoreProvider,
///     result         = paramodel_tck::MockResultStoreProvider,
/// }
/// ```
#[macro_export]
macro_rules! run_all_persistence {
    (
        artifact       = $artifact:expr,
        checkpoint     = $checkpoint:expr,
        execution_repo = $exec_repo:expr,
        journal        = $journal:expr,
        metadata       = $metadata:expr,
        result         = $result:expr $(,)?
    ) => {
        mod _tck_persistence_artifact { use super::*; $crate::run_artifact_store!($artifact); }
        mod _tck_persistence_checkpoint { use super::*; $crate::run_checkpoint_store!($checkpoint); }
        mod _tck_persistence_execution_repo { use super::*; $crate::run_execution_repository!($exec_repo); }
        mod _tck_persistence_journal { use super::*; $crate::run_journal_store!($journal); }
        mod _tck_persistence_metadata { use super::*; $crate::run_metadata_store!($metadata); }
        mod _tck_persistence_result { use super::*; $crate::run_result_store!($result); }
    };
}
