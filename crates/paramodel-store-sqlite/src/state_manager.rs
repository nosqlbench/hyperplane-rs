// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionStateManager` impl on `SqliteStore`.
//!
//! Unifies the three storage concerns the executor uses during a run:
//! - `record_event` / `is_step_completed` → `journal_events` table.
//! - `checkpoint` / `load_checkpoint` → `checkpoints` table.
//! - `save/get/get_trial_results` → `trial_results` table.
//! - `record_suspension` / `cleanup` → dedicated `suspensions` table
//!   (lazily created inside this module — the main schema doesn't
//!   know about it since the persistence SRD treats it as internal).
//!
//! `recover` scans the journal for this execution and synthesises a
//! `RecoveryResult`, mirroring what `InMemoryStateManager` does.

use async_trait::async_trait;
use paramodel_elements::TrialId;
use paramodel_executor::{
    Checkpoint, ExecutionId, ExecutionStateManager, JournalEvent, JournalEventKind,
    RecoveryResult,
};
use paramodel_plan::ExecutionPlan;
use paramodel_persistence::{CheckpointStore, JournalStore, ResultStore};
use paramodel_plan::{CheckpointId, StepId};
use paramodel_trials::TrialResult;
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::SqliteStore;

/// Create the internal `suspensions` table on demand. Kept out of
/// the main schema because it's not part of SRD-0012; used only by
/// `record_suspension`.
const SUSPENSIONS_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS suspensions (
    execution_id TEXT PRIMARY KEY,
    reason       TEXT NOT NULL,
    recorded_at  TEXT NOT NULL
);
";

#[async_trait]
impl ExecutionStateManager for SqliteStore {
    async fn record_event(&self, event: JournalEvent) {
        // We own a JournalStore impl; route through it. Swallow
        // errors — the trait signature is infallible.
        let _ = <Self as JournalStore>::append(self, &event).await;
    }

    async fn checkpoint(&self, checkpoint: &Checkpoint) {
        let _ = <Self as CheckpointStore>::save(self, checkpoint).await;
    }

    async fn load_checkpoint(&self, id: &CheckpointId) -> Option<Checkpoint> {
        <Self as CheckpointStore>::get(self, id).await.ok()
    }

    async fn recover(
        &self,
        execution: &ExecutionId,
        _plan:     &ExecutionPlan,
    ) -> RecoveryResult {
        use futures::StreamExt;
        let mut result = RecoveryResult::default();
        let events: Vec<JournalEvent> = <Self as JournalStore>::replay(
            self, execution, None,
        )
        .await
        .filter_map(|r| async move { r.ok() })
        .collect()
        .await;
        for e in events {
            match e.kind {
                JournalEventKind::StepStarted { step_id, .. } => {
                    result.in_flight_step_ids.insert(step_id);
                }
                JournalEventKind::StepCompleted { step_id, .. } => {
                    result.in_flight_step_ids.remove(&step_id);
                    result.completed_step_ids.insert(step_id);
                }
                JournalEventKind::StepFailed { step_id, .. } => {
                    result.in_flight_step_ids.remove(&step_id);
                    result.failed_step_ids.insert(step_id);
                }
                JournalEventKind::StepSkipped { step_id, .. } => {
                    result.in_flight_step_ids.remove(&step_id);
                    result.skipped_step_ids.insert(step_id);
                }
                JournalEventKind::ExecutionCompleted { .. } => {
                    result.was_clean_shutdown = true;
                }
                _ => {}
            }
        }
        result
    }

    async fn is_step_completed(
        &self,
        execution: &ExecutionId,
        step:      &StepId,
    ) -> bool {
        let exec = execution.to_string();
        let step_id = step.as_str().to_owned();
        let hit = self
            .conn
            .call(move |c| -> Result<Option<i64>, rusqlite::Error> {
                // Completed events serialise as
                // {"kind":"step_completed","step_id":"...","attempt":_,"duration":_}.
                // Use LIKE on the JSON — rough but correct for this
                // shape. A follow-up can index a discriminator column.
                c.query_row(
                    "SELECT 1 FROM journal_events \
                     WHERE execution_id = ?1 \
                       AND event_json LIKE '%\"kind\":\"step_completed\"%' \
                       AND event_json LIKE ?2 \
                     LIMIT 1",
                    rusqlite::params![
                        exec,
                        format!("%\"step_id\":\"{step_id}\"%"),
                    ],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
            })
            .await;
        matches!(hit, Ok(Some(_)))
    }

    async fn record_suspension(&self, execution: &ExecutionId, reason: &str) {
        let exec = execution.to_string();
        let reason = reason.to_owned();
        let now = jiff::Timestamp::now().to_string();
        let _ = self
            .conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                // Ensure the table exists — cheap IF NOT EXISTS each
                // call.
                c.execute_batch(SUSPENSIONS_SCHEMA)?;
                c.execute(
                    "INSERT INTO suspensions (execution_id, reason, recorded_at) \
                     VALUES (?1, ?2, ?3) \
                     ON CONFLICT(execution_id) DO UPDATE SET \
                       reason = excluded.reason, \
                       recorded_at = excluded.recorded_at",
                    rusqlite::params![exec, reason, now],
                )?;
                Ok(())
            })
            .await;
    }

    async fn save_trial_result(
        &self,
        _execution: &ExecutionId,
        result:     &TrialResult,
    ) {
        let _ = <Self as ResultStore>::save(self, result).await;
    }

    async fn get_trial_result(&self, trial: &TrialId) -> Option<TrialResult> {
        <Self as ResultStore>::get(self, trial).await.ok().flatten()
    }

    async fn get_trial_results(
        &self,
        _execution: &ExecutionId,
    ) -> Vec<TrialResult> {
        // ResultStore's query doesn't filter by ExecutionId directly
        // in v0.2 (TrialResult doesn't carry one). Return everything
        // — tests should scope via a fresh store.
        let page = <Self as ResultStore>::query(
            self,
            &paramodel_persistence::ResultFilter::Any,
            paramodel_persistence::PageRequest::first(
                paramodel_persistence::MAX_PAGE_SIZE,
            ),
        )
        .await;
        page.map(|p| p.items).unwrap_or_default()
    }

    async fn cleanup(&self, execution: &ExecutionId) {
        let exec = execution.to_string();
        let _ = self
            .conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "DELETE FROM journal_events WHERE execution_id = ?1",
                    rusqlite::params![exec],
                )?;
                c.execute(
                    "DELETE FROM checkpoints WHERE execution_id = ?1",
                    rusqlite::params![exec],
                )?;
                // Best-effort: the suspensions table may not exist
                // yet if record_suspension never fired.
                let _ = c.execute_batch(SUSPENSIONS_SCHEMA);
                c.execute(
                    "DELETE FROM suspensions WHERE execution_id = ?1",
                    rusqlite::params![exec],
                )?;
                Ok(())
            })
            .await;
    }
}
