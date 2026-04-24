// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Exhaustive paramodel-tck conformance against the `SqliteStore`
//! backend. Every `tck_*` entrypoint in the six persistence test
//! modules runs here via the Sqlite provider shims.
//!
//! Multi-thread runtime is required because `provider.fresh()`
//! synchronously constructs an in-memory SQLite connection using
//! `tokio::task::block_in_place`.

use paramodel_store_sqlite::{
    SqliteArtifactStoreProvider, SqliteCheckpointStoreProvider,
    SqliteExecutionRepositoryProvider, SqliteJournalStoreProvider,
    SqliteMetadataStoreProvider, SqliteResultStoreProvider,
};
use paramodel_tck::artifact_store_tests::{
    tck_artifact_list_by_context, tck_artifact_save_and_download,
    tck_artifact_save_mode_create_new_rejects_duplicate,
    tck_artifact_save_mode_replace_overwrites,
};
use paramodel_tck::checkpoint_store_tests::{
    tck_checkpoint_latest, tck_checkpoint_per_execution_isolation, tck_checkpoint_prune,
    tck_checkpoint_save_and_get,
};
use paramodel_tck::execution_repository_tests::{
    tck_execution_filter_status, tck_execution_list_plans_filter,
    tck_execution_save_and_get_execution, tck_execution_save_and_get_plan,
};
use paramodel_tck::journal_store_tests::{
    tck_journal_store_append_then_replay, tck_journal_store_latest_sequence,
    tck_journal_store_per_execution_isolation, tck_journal_store_replay_after_sequence,
    tck_journal_store_truncate_before,
};
use paramodel_tck::metadata_store_tests::{
    tck_metadata_list_plans, tck_metadata_save_and_get, tck_metadata_set_labels,
};
use paramodel_tck::result_store_tests::{
    tck_result_aggregate, tck_result_count, tck_result_query_by_status,
    tck_result_save_and_get, tck_result_stream,
};

// ---------------------------------------------------------------------------
// JournalStore conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_journal_append_then_replay() {
    tck_journal_store_append_then_replay(&SqliteJournalStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_journal_replay_after_sequence() {
    tck_journal_store_replay_after_sequence(&SqliteJournalStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_journal_latest_sequence() {
    tck_journal_store_latest_sequence(&SqliteJournalStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_journal_truncate_before() {
    tck_journal_store_truncate_before(&SqliteJournalStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_journal_per_execution_isolation() {
    tck_journal_store_per_execution_isolation(&SqliteJournalStoreProvider).await;
}

// ---------------------------------------------------------------------------
// CheckpointStore conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_checkpoint_save_get() {
    tck_checkpoint_save_and_get(&SqliteCheckpointStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_checkpoint_latest_return() {
    tck_checkpoint_latest(&SqliteCheckpointStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_checkpoint_prune_keep_n() {
    tck_checkpoint_prune(&SqliteCheckpointStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_checkpoint_isolation_by_execution() {
    tck_checkpoint_per_execution_isolation(&SqliteCheckpointStoreProvider).await;
}

// ---------------------------------------------------------------------------
// ResultStore conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_result_save_get() {
    tck_result_save_and_get(&SqliteResultStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_result_filter_by_status() {
    tck_result_query_by_status(&SqliteResultStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_result_count_filter() {
    tck_result_count(&SqliteResultStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_result_stream_filter() {
    tck_result_stream(&SqliteResultStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_result_aggregate_scalar() {
    tck_result_aggregate(&SqliteResultStoreProvider).await;
}

// ---------------------------------------------------------------------------
// ArtifactStore conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_artifact_save_download() {
    tck_artifact_save_and_download(&SqliteArtifactStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_artifact_create_new_conflicts() {
    tck_artifact_save_mode_create_new_rejects_duplicate(&SqliteArtifactStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_artifact_replace_overwrites() {
    tck_artifact_save_mode_replace_overwrites(&SqliteArtifactStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_artifact_list() {
    tck_artifact_list_by_context(&SqliteArtifactStoreProvider).await;
}

// ---------------------------------------------------------------------------
// ExecutionRepository conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_repo_save_get_plan() {
    tck_execution_save_and_get_plan(&SqliteExecutionRepositoryProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_repo_save_get_execution() {
    tck_execution_save_and_get_execution(&SqliteExecutionRepositoryProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_repo_list_plans_filter() {
    tck_execution_list_plans_filter(&SqliteExecutionRepositoryProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_repo_filter_status() {
    tck_execution_filter_status(&SqliteExecutionRepositoryProvider).await;
}

// ---------------------------------------------------------------------------
// MetadataStore conformance.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_metadata_save_get() {
    tck_metadata_save_and_get(&SqliteMetadataStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_metadata_list() {
    tck_metadata_list_plans(&SqliteMetadataStoreProvider).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tck_metadata_labels() {
    tck_metadata_set_labels(&SqliteMetadataStoreProvider).await;
}
