// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `JournalStore` impl backed by the `journal_events` table.
//!
//! Events serialise as JSON in `event_json`. Sequence numbers are the
//! primary key; callers supply them (the executor's `JournalWriter`
//! already allocates monotonic sequences). Replay filters by
//! `(execution_id, sequence > after)` ordered by `sequence ASC`.

use async_trait::async_trait;
use futures::stream;
use paramodel_executor::{ExecutionId, JournalEvent, JournalSequence};
use paramodel_persistence::{
    JournalStore, PersistenceError, PersistenceResult, PersistenceStream,
};
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::{SqliteStore, json_err, rusqlite_err};

#[async_trait]
impl JournalStore for SqliteStore {
    async fn append(&self, event: &JournalEvent) -> PersistenceResult<()> {
        let seq = event.sequence.get() as i64;
        let exec_str = event.execution_id.to_string();
        let ts   = event.timestamp.to_string();
        let body = serde_json::to_string(event).map_err(json_err("journal.append"))?;
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                // `INSERT OR IGNORE` makes `(execution_id, sequence)`
                // writes idempotent — fine when the same store is
                // wired as both `JournalWriter` (executor) and
                // `ExecutionStateManager` (which also calls
                // `record_event` → `append`).
                c.execute(
                    "INSERT OR IGNORE INTO journal_events \
                     (execution_id, sequence, timestamp, event_json) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![exec_str, seq, ts, body],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("journal.append"))?;
        // Broadcast to live subscribers. Errors mean no active
        // subscribers — not a failure.
        let _ = self.publisher_for(event.execution_id).send(event.clone());
        Ok(())
    }

    async fn append_batch(&self, events: &[JournalEvent]) -> PersistenceResult<()> {
        let mut rows: Vec<(i64, String, String, String)> = Vec::with_capacity(events.len());
        for e in events {
            let body = serde_json::to_string(e).map_err(json_err("journal.append_batch"))?;
            rows.push((
                e.sequence.get() as i64,
                e.execution_id.to_string(),
                e.timestamp.to_string(),
                body,
            ));
        }
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                let tx = c.transaction()?;
                {
                    let mut stmt = tx.prepare(
                        "INSERT OR IGNORE INTO journal_events \
                         (execution_id, sequence, timestamp, event_json) \
                         VALUES (?1, ?2, ?3, ?4)",
                    )?;
                    for (seq, exec, ts, body) in &rows {
                        stmt.execute(rusqlite::params![exec, seq, ts, body])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("journal.append_batch"))?;
        // Publish each event to live subscribers after the DB commit
        // succeeds.
        for e in events {
            let _ = self.publisher_for(e.execution_id).send(e.clone());
        }
        Ok(())
    }

    async fn replay(
        &self,
        execution:      &ExecutionId,
        after_sequence: Option<JournalSequence>,
    ) -> PersistenceStream<JournalEvent> {
        let exec = execution.to_string();
        let after = after_sequence.map(|s| s.get() as i64).unwrap_or(-1);
        let raw = self
            .conn
            .call(move |c| -> Result<Vec<String>, rusqlite::Error> {
                let mut stmt = c.prepare(
                    "SELECT event_json FROM journal_events \
                     WHERE execution_id = ?1 AND sequence > ?2 \
                     ORDER BY sequence ASC",
                )?;
                let out: Vec<String> = stmt
                    .query_map(rusqlite::params![exec, after], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok(out)
            })
            .await;
        stream_of(raw, "journal.replay")
    }

    async fn latest_event(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<JournalEvent>> {
        let exec = execution.to_string();
        let raw = self
            .conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                let mut stmt = c.prepare(
                    "SELECT event_json FROM journal_events \
                     WHERE execution_id = ?1 ORDER BY sequence DESC LIMIT 1",
                )?;
                let out = stmt
                    .query_row(rusqlite::params![exec], |r| r.get(0))
                    .optional()?;
                Ok(out)
            })
            .await
            .map_err(rusqlite_err("journal.latest_event"))?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(json_err("journal.latest_event")),
        }
    }

    async fn latest_sequence(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<JournalSequence> {
        let exec = execution.to_string();
        let seq = self
            .conn
            .call(move |c| -> Result<Option<i64>, rusqlite::Error> {
                let mut stmt = c.prepare(
                    "SELECT MAX(sequence) FROM journal_events WHERE execution_id = ?1",
                )?;
                let out: Option<i64> = stmt
                    .query_row(rusqlite::params![exec], |r| r.get(0))
                    .optional()?
                    .flatten();
                Ok(out)
            })
            .await
            .map_err(rusqlite_err("journal.latest_sequence"))?;
        Ok(match seq {
            Some(v) if v >= 0 => JournalSequence::new(v as u64),
            _ => JournalSequence::ZERO,
        })
    }

    async fn truncate_before(
        &self,
        execution: &ExecutionId,
        before:    JournalSequence,
    ) -> PersistenceResult<()> {
        let exec = execution.to_string();
        let b = before.get() as i64;
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "DELETE FROM journal_events WHERE execution_id = ?1 AND sequence < ?2",
                    rusqlite::params![exec, b],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("journal.truncate_before"))?;
        Ok(())
    }

    async fn delete_all(&self, execution: &ExecutionId) -> PersistenceResult<()> {
        let exec = execution.to_string();
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "DELETE FROM journal_events WHERE execution_id = ?1",
                    rusqlite::params![exec],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("journal.delete_all"))?;
        Ok(())
    }

    async fn subscribe(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceStream<JournalEvent> {
        // Live tail: attach a broadcast receiver and fold recv()
        // calls into a Stream. Existing rows are *not* replayed —
        // callers that want history should use `replay` first.
        let rx = self.publisher_for(*execution).subscribe();
        let stream = stream::unfold(rx, |mut rx| async move {
            match rx.recv().await {
                Ok(event) => Some((Ok(event), rx)),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    // Surface as a Serialization-ish backend error.
                    Some((
                        Err(PersistenceError::Backend(Box::new(
                            crate::store::BackendMsg(format!(
                                "subscribe lagged: dropped {skipped} event(s)"
                            )),
                        ))),
                        rx,
                    ))
                }
            }
        });
        Box::pin(stream)
    }
}

fn stream_of(
    rows: Result<Vec<String>, tokio_rusqlite::Error<rusqlite::Error>>,
    ctx:  &'static str,
) -> PersistenceStream<JournalEvent> {
    let rows: Result<Vec<String>, PersistenceError> =
        rows.map_err(rusqlite_err::<tokio_rusqlite::Error<rusqlite::Error>>(ctx));
    match rows {
        Err(e)   => Box::pin(stream::iter(vec![Err(e)])),
        Ok(rows) => {
            let items: Vec<PersistenceResult<JournalEvent>> = rows
                .into_iter()
                .map(|s| serde_json::from_str(&s).map_err(json_err(ctx)))
                .collect();
            Box::pin(stream::iter(items))
        }
    }
}
