// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `CheckpointStore` impl backed by the `checkpoints` table.

use async_trait::async_trait;
use paramodel_executor::{Checkpoint, CheckpointHandle, ExecutionId};
use paramodel_persistence::{
    CheckpointStore, CleanupReport, Page, PageRequest, PageToken, PersistenceError,
    PersistenceResult,
};
use paramodel_plan::CheckpointId;
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::{SqliteStore, json_err, rusqlite_err};

#[async_trait]
impl CheckpointStore for SqliteStore {
    async fn save(&self, checkpoint: &Checkpoint) -> PersistenceResult<()> {
        let id       = checkpoint.id.as_str().to_owned();
        let exec     = checkpoint.execution_id.to_string();
        let fp       = checkpoint.plan_fingerprint.to_string();
        let created  = checkpoint.created_at.to_string();
        let body = serde_json::to_string(checkpoint)
            .map_err(json_err("checkpoint.save"))?;
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                // Upsert on id so a retry with the same checkpoint is
                // idempotent.
                c.execute(
                    "INSERT INTO checkpoints \
                     (id, execution_id, plan_fingerprint, created_at, checkpoint_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(id) DO UPDATE SET \
                       execution_id = excluded.execution_id, \
                       plan_fingerprint = excluded.plan_fingerprint, \
                       created_at = excluded.created_at, \
                       checkpoint_json = excluded.checkpoint_json",
                    rusqlite::params![id, exec, fp, created, body],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("checkpoint.save"))?;
        Ok(())
    }

    async fn get(&self, id: &CheckpointId) -> PersistenceResult<Checkpoint> {
        let key = id.as_str().to_owned();
        let id_for_error = key.clone();
        let raw = self
            .conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT checkpoint_json FROM checkpoints WHERE id = ?1",
                    rusqlite::params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("checkpoint.get"))?;
        let body = raw.ok_or_else(|| PersistenceError::NotFound {
            kind: "checkpoint",
            id:   id_for_error,
        })?;
        serde_json::from_str::<Checkpoint>(&body).map_err(json_err("checkpoint.get"))
    }

    async fn delete(&self, id: &CheckpointId) -> PersistenceResult<()> {
        let key = id.as_str().to_owned();
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "DELETE FROM checkpoints WHERE id = ?1",
                    rusqlite::params![key],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("checkpoint.delete"))?;
        Ok(())
    }

    async fn latest(
        &self,
        execution: &ExecutionId,
    ) -> PersistenceResult<Option<Checkpoint>> {
        let exec = execution.to_string();
        let raw = self
            .conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT checkpoint_json FROM checkpoints \
                     WHERE execution_id = ?1 \
                     ORDER BY created_at DESC, id DESC LIMIT 1",
                    rusqlite::params![exec],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("checkpoint.latest"))?;
        match raw {
            None => Ok(None),
            Some(s) => serde_json::from_str(&s)
                .map(Some)
                .map_err(json_err("checkpoint.latest")),
        }
    }

    async fn list(
        &self,
        execution: &ExecutionId,
        page:      PageRequest,
    ) -> PersistenceResult<Page<CheckpointHandle>> {
        let exec = execution.to_string();
        let page = page.clamped();
        let size = page.size as i64;
        let cursor: Option<(String, String)> = page.token.as_ref().and_then(parse_token);

        let rows = self
            .conn
            .call(move |c| -> Result<Vec<(String, String, String)>, rusqlite::Error> {
                let limit = size + 1;
                match cursor {
                    None => {
                        let mut stmt = c.prepare(
                            "SELECT id, created_at, checkpoint_json FROM checkpoints \
                             WHERE execution_id = ?1 \
                             ORDER BY created_at DESC, id DESC LIMIT ?2",
                        )?;
                        let rows: Vec<(String, String, String)> = stmt
                            .query_map(rusqlite::params![exec, limit], |r| {
                                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                            })?
                            .collect::<Result<_, _>>()?;
                        Ok(rows)
                    }
                    Some((cursor_ts, cursor_id)) => {
                        let mut stmt = c.prepare(
                            "SELECT id, created_at, checkpoint_json FROM checkpoints \
                             WHERE execution_id = ?1 \
                             AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)) \
                             ORDER BY created_at DESC, id DESC LIMIT ?4",
                        )?;
                        let rows: Vec<(String, String, String)> = stmt
                            .query_map(
                                rusqlite::params![exec, cursor_ts, cursor_id, limit],
                                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                            )?
                            .collect::<Result<_, _>>()?;
                        Ok(rows)
                    }
                }
            })
            .await
            .map_err(rusqlite_err("checkpoint.list"))?;

        let has_next = rows.len() > size as usize;
        let kept: Vec<(String, String, String)> =
            rows.into_iter().take(size as usize).collect();

        let next_token = if has_next {
            kept.last().map(|(id, ts, _)| make_token(ts, id))
        } else {
            None
        };

        let mut items = Vec::with_capacity(kept.len());
        for (_id, _ts, body) in kept {
            let cp: Checkpoint = serde_json::from_str(&body)
                .map_err(json_err("checkpoint.list"))?;
            items.push(CheckpointHandle {
                id:         cp.id,
                created_at: cp.created_at,
                is_valid:   true,
            });
        }

        Ok(Page { items, next_token, total_hint: None })
    }

    async fn prune(
        &self,
        execution:   &ExecutionId,
        keep_newest: u32,
    ) -> PersistenceResult<CleanupReport> {
        let exec = execution.to_string();
        let keep = keep_newest as i64;
        let removed = self
            .conn
            .call(move |c| -> Result<i64, rusqlite::Error> {
                let affected = c.execute(
                    "DELETE FROM checkpoints WHERE id IN (\
                       SELECT id FROM checkpoints WHERE execution_id = ?1 \
                       ORDER BY created_at DESC, id DESC \
                       LIMIT -1 OFFSET ?2\
                     )",
                    rusqlite::params![exec, keep],
                )?;
                Ok(affected as i64)
            })
            .await
            .map_err(rusqlite_err("checkpoint.prune"))?;
        Ok(CleanupReport {
            artifacts_removed: removed.max(0) as u64,
            bytes_freed:       0,
        })
    }
}

/// `"{created_at}|{id}"` — opaque to callers.
fn make_token(created_at: &str, id: &str) -> PageToken {
    PageToken::new(format!("{created_at}|{id}"))
}

fn parse_token(t: &PageToken) -> Option<(String, String)> {
    let s = t.as_str();
    let (ts, id) = s.split_once('|')?;
    if ts.is_empty() { return None; }
    Some((ts.to_owned(), id.to_owned()))
}
