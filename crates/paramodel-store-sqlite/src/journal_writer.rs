// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `JournalWriter` impl on `SqliteStore` — lets the executor persist
//! events directly to the backend instead of an in-memory buffer.
//!
//! Uses the same `journal_events` table that `JournalStore` reads
//! from; the two trait impls meet in the middle of one durable log.

use async_trait::async_trait;
use paramodel_executor::{
    ExecutionId, JournalError, JournalEvent, JournalSequence, JournalWriter,
};
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::SqliteStore;

#[async_trait]
impl JournalWriter for SqliteStore {
    async fn write(&self, event: JournalEvent) -> Result<(), JournalError> {
        let seq  = event.sequence.get() as i64;
        let exec = event.execution_id.to_string();
        let ts   = event.timestamp.to_string();
        let body = serde_json::to_string(&event)
            .map_err(|e| JournalError::WriteFailed { reason: format!("{e}") })?;
        let exec_id_for_publish = event.execution_id;
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                // Idempotent on `(execution_id, sequence)` so the
                // same store may serve as both `JournalWriter` and
                // `ExecutionStateManager::record_event` without
                // double-insert conflicts.
                c.execute(
                    "INSERT OR IGNORE INTO journal_events \
                     (execution_id, sequence, timestamp, event_json) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![exec, seq, ts, body],
                )?;
                Ok(())
            })
            .await
            .map_err(|e| JournalError::WriteFailed { reason: format!("{e}") })?;
        // Fire live subscribers if any. No active listeners → no-op.
        let _ = self.publisher_for(exec_id_for_publish).send(event);
        Ok(())
    }

    async fn flush(&self) -> Result<(), JournalError> {
        // `tokio_rusqlite` commits every call synchronously in WAL
        // mode. There's no buffered write to flush.
        Ok(())
    }

    async fn since(
        &self,
        after: Option<JournalSequence>,
    ) -> Result<Vec<JournalEvent>, JournalError> {
        let gate = after.map(|s| s.get() as i64).unwrap_or(-1);
        let rows = self
            .conn
            .call(move |c| -> Result<Vec<String>, rusqlite::Error> {
                let mut stmt = c.prepare(
                    "SELECT event_json FROM journal_events \
                     WHERE sequence > ?1 ORDER BY sequence ASC, execution_id ASC",
                )?;
                stmt.query_map(rusqlite::params![gate], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .await
            .map_err(|e| JournalError::ReadFailed { reason: format!("{e}") })?;
        rows.into_iter()
            .map(|s| {
                serde_json::from_str::<JournalEvent>(&s)
                    .map_err(|e| JournalError::ReadFailed { reason: format!("{e}") })
            })
            .collect()
    }

    async fn last_event(&self, execution: &ExecutionId) -> Option<JournalEvent> {
        let exec = execution.to_string();
        let raw: Option<String> = self
            .conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT event_json FROM journal_events \
                     WHERE execution_id = ?1 ORDER BY sequence DESC LIMIT 1",
                    rusqlite::params![exec],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .ok()?;
        raw.and_then(|s| serde_json::from_str(&s).ok())
    }
}
