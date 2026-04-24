// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ArtifactStore` impl backed by the `artifacts` table.
//!
//! `ArtifactContent::Inline(bytes)` goes into the `content_blob`
//! column; `ArtifactContent::Uri(s)` into `uri`. Small inline
//! payloads therefore round-trip without touching external storage —
//! the common case for test runs.

use async_trait::async_trait;
use futures::stream;
use jiff::Timestamp;
use paramodel_elements::{ElementName, TrialId};
use paramodel_executor::{ArtifactContent, ExecutionId};
use paramodel_persistence::{
    ArtifactContext, ArtifactId, ArtifactRecord, ArtifactStore, CleanupReport, Page,
    PageRequest, PageToken, PersistenceError, PersistenceResult, PersistenceStream,
    RetentionPolicy, SaveMode,
};
use paramodel_trials::ArtifactRef;
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;
use ulid::Ulid;

use crate::store::{SqliteStore, json_err, rusqlite_err};

#[async_trait]
impl ArtifactStore for SqliteStore {
    async fn save(
        &self,
        context:  &ArtifactContext,
        artifact: &ArtifactRef,
        content:  ArtifactContent,
        mode:     SaveMode,
    ) -> PersistenceResult<ArtifactId> {
        let exec_id = context
            .execution
            .map(|e| e.to_string())
            .unwrap_or_default();
        let trial_id = context.trial.map(|t| t.to_string());
        let element  = artifact.element.as_str().to_owned();
        let name     = artifact.name.clone();
        let content_type = artifact.content_type.clone();

        let (uri, blob, size): (Option<String>, Option<Vec<u8>>, u64) = match content {
            ArtifactContent::Inline(bytes) => {
                let len = bytes.len() as u64;
                (None, Some(bytes), len)
            }
            ArtifactContent::Uri(u)        => {
                (Some(u), None, artifact.size_bytes.unwrap_or(0))
            }
        };

        // Duplicate detection keyed on (execution, trial, element, name).
        if matches!(mode, SaveMode::CreateNew) {
            let exists = {
                let e = exec_id.clone();
                let t = trial_id.clone();
                let el = element.clone();
                let n  = name.clone();
                self.conn
                    .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                        c.query_row(
                            "SELECT id FROM artifacts \
                             WHERE execution_id = ?1 \
                               AND IFNULL(trial_id, '') = IFNULL(?2, '') \
                               AND element = ?3 AND name = ?4 \
                             LIMIT 1",
                            rusqlite::params![e, t, el, n],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                    })
                    .await
                    .map_err(rusqlite_err("artifact.save"))?
            };
            if let Some(existing) = exists {
                return Err(PersistenceError::Conflict {
                    kind: "artifact",
                    id:   existing,
                });
            }
        }
        if matches!(mode, SaveMode::Replace) {
            let e = exec_id.clone();
            let t = trial_id.clone();
            let el = element.clone();
            let n  = name.clone();
            self.conn
                .call(move |c| -> Result<(), rusqlite::Error> {
                    c.execute(
                        "DELETE FROM artifacts \
                         WHERE execution_id = ?1 \
                           AND IFNULL(trial_id, '') = IFNULL(?2, '') \
                           AND element = ?3 AND name = ?4",
                        rusqlite::params![e, t, el, n],
                    )?;
                    Ok(())
                })
                .await
                .map_err(rusqlite_err("artifact.save"))?;
        }

        let id = ArtifactId::from_ulid(Ulid::new());
        let id_str = id.to_string();
        let created = Timestamp::now().to_string();
        let size_i = size as i64;
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "INSERT INTO artifacts \
                     (id, execution_id, trial_id, element, name, content_type, \
                      size_bytes, uri, content_blob, created_at, expires_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, NULL)",
                    rusqlite::params![
                        id_str, exec_id, trial_id, element, name, content_type,
                        size_i, uri, blob, created
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("artifact.save"))?;
        Ok(id)
    }

    async fn get(&self, id: &ArtifactId) -> PersistenceResult<ArtifactRecord> {
        let key = id.to_string();
        let id_for_error = key.clone();
        let row = self.conn
            .call(move |c| -> Result<Option<ArtifactRow>, rusqlite::Error> {
                c.query_row(
                    "SELECT id, execution_id, trial_id, element, name, content_type, \
                            size_bytes, uri, created_at \
                     FROM artifacts WHERE id = ?1",
                    rusqlite::params![key],
                    ArtifactRow::from_row,
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("artifact.get"))?;
        let row = row.ok_or_else(|| PersistenceError::NotFound {
            kind: "artifact",
            id:   id_for_error,
        })?;
        row.into_record().map_err(json_err("artifact.get"))
    }

    async fn download(&self, id: &ArtifactId) -> PersistenceResult<ArtifactContent> {
        let key = id.to_string();
        let id_for_error = key.clone();
        let row = self.conn
            .call(move |c| -> Result<Option<(Option<Vec<u8>>, Option<String>)>, rusqlite::Error> {
                c.query_row(
                    "SELECT content_blob, uri FROM artifacts WHERE id = ?1",
                    rusqlite::params![key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("artifact.download"))?;
        let (blob, uri) = row.ok_or_else(|| PersistenceError::NotFound {
            kind: "artifact",
            id:   id_for_error,
        })?;
        match (blob, uri) {
            (Some(b), _) => Ok(ArtifactContent::Inline(b)),
            (None, Some(u)) => Ok(ArtifactContent::Uri(u)),
            (None, None) => Ok(ArtifactContent::Inline(Vec::new())),
        }
    }

    async fn list(
        &self,
        context: &ArtifactContext,
        page:    PageRequest,
    ) -> PersistenceResult<Page<ArtifactRecord>> {
        let page = page.clamped();
        let size = page.size as i64;
        let limit = size + 1;
        let cursor = page.token.as_ref().map(|t| t.as_str().to_owned());
        let exec = context.execution.map(|e| e.to_string());
        let trial = context.trial.map(|t| t.to_string());
        let element = context.element.as_ref().map(|e| e.as_str().to_owned());

        let rows = self.conn
            .call(move |c| -> Result<Vec<ArtifactRow>, rusqlite::Error> {
                let mut sql = String::from(
                    "SELECT id, execution_id, trial_id, element, name, content_type, \
                            size_bytes, uri, created_at FROM artifacts WHERE 1=1",
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(e) = exec.as_ref() {
                    sql.push_str(" AND execution_id = ?");
                    params.push(Box::new(e.clone()));
                }
                if let Some(t) = trial.as_ref() {
                    sql.push_str(" AND trial_id = ?");
                    params.push(Box::new(t.clone()));
                }
                if let Some(el) = element.as_ref() {
                    sql.push_str(" AND element = ?");
                    params.push(Box::new(el.clone()));
                }
                if let Some(cur) = cursor.as_ref() {
                    sql.push_str(" AND id > ?");
                    params.push(Box::new(cur.clone()));
                }
                sql.push_str(" ORDER BY id ASC LIMIT ?");
                params.push(Box::new(limit));
                let mut stmt = c.prepare(&sql)?;
                let rows: Vec<ArtifactRow> = stmt
                    .query_map(rusqlite::params_from_iter(params), ArtifactRow::from_row)?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(rusqlite_err("artifact.list"))?;

        let has_next = rows.len() > size as usize;
        let kept: Vec<ArtifactRow> = rows.into_iter().take(size as usize).collect();
        let next_token = if has_next {
            kept.last().map(|r| PageToken::new(r.id.clone()))
        } else {
            None
        };
        let mut items = Vec::with_capacity(kept.len());
        for row in kept {
            items.push(row.into_record().map_err(json_err("artifact.list"))?);
        }
        Ok(Page { items, next_token, total_hint: None })
    }

    async fn stream(&self, context: &ArtifactContext) -> PersistenceStream<ArtifactRecord> {
        // V1: materialise a one-shot page of a generous size; a
        // true chunked stream can wait for a multi-page helper.
        let page = self
            .list(context, PageRequest::first(paramodel_persistence::MAX_PAGE_SIZE))
            .await;
        match page {
            Ok(p)  => Box::pin(stream::iter(p.items.into_iter().map(Ok))),
            Err(e) => Box::pin(stream::iter(vec![Err(e)])),
        }
    }

    async fn delete(&self, id: &ArtifactId) -> PersistenceResult<()> {
        let key = id.to_string();
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "DELETE FROM artifacts WHERE id = ?1",
                    rusqlite::params![key],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("artifact.delete"))?;
        Ok(())
    }

    async fn run_retention(
        &self,
        policy: &RetentionPolicy,
    ) -> PersistenceResult<CleanupReport> {
        let keep_newest = policy.keep_newest;
        let max_age = policy.max_age;
        let max_size_bytes = policy.max_size_bytes;

        // `max_age` translates to a timestamp bound; compute up front
        // so the SQL closure only uses owned data.
        let age_cutoff: Option<String> = max_age.map(|d| {
            let secs = d.as_secs() as i64;
            let cutoff = jiff::Timestamp::now().as_second() - secs;
            jiff::Timestamp::from_second(cutoff)
                .unwrap_or_else(|_| jiff::Timestamp::UNIX_EPOCH)
                .to_string()
        });

        let (removed, bytes_freed) = self.conn
            .call(move |c| -> Result<(u64, u64), rusqlite::Error> {
                let tx = c.transaction()?;
                let mut removed: u64 = 0;
                let mut bytes_freed: u64 = 0;

                // Age-based pass.
                if let Some(cutoff) = age_cutoff.as_ref() {
                    let freed: i64 = tx.query_row(
                        "SELECT IFNULL(SUM(size_bytes), 0) FROM artifacts \
                         WHERE created_at < ?1",
                        rusqlite::params![cutoff],
                        |r| r.get(0),
                    )?;
                    let n = tx.execute(
                        "DELETE FROM artifacts WHERE created_at < ?1",
                        rusqlite::params![cutoff],
                    )?;
                    removed = removed.saturating_add(n as u64);
                    bytes_freed = bytes_freed.saturating_add(freed.max(0) as u64);
                }

                // keep_newest per-context pass. Context key =
                // (execution_id, IFNULL(trial_id,''), element).
                if let Some(k) = keep_newest {
                    let ctxs: Vec<(String, String, String)> = {
                        let mut stmt = tx.prepare(
                            "SELECT DISTINCT execution_id, IFNULL(trial_id, ''), element \
                             FROM artifacts",
                        )?;
                        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    for (exec, trial, element) in ctxs {
                        let freed: i64 = tx.query_row(
                            "SELECT IFNULL(SUM(size_bytes), 0) FROM artifacts \
                             WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                               AND element = ?3 AND id IN ( \
                                 SELECT id FROM artifacts \
                                 WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                                   AND element = ?3 \
                                 ORDER BY created_at DESC LIMIT -1 OFFSET ?4)",
                            rusqlite::params![exec, trial, element, k as i64],
                            |r| r.get(0),
                        )?;
                        let n = tx.execute(
                            "DELETE FROM artifacts \
                             WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                               AND element = ?3 AND id IN ( \
                                 SELECT id FROM artifacts \
                                 WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                                   AND element = ?3 \
                                 ORDER BY created_at DESC LIMIT -1 OFFSET ?4)",
                            rusqlite::params![exec, trial, element, k as i64],
                        )?;
                        removed = removed.saturating_add(n as u64);
                        bytes_freed = bytes_freed.saturating_add(freed.max(0) as u64);
                    }
                }

                // max_size_bytes per-context pass — evict oldest
                // until the context's total size is within budget.
                if let Some(cap) = max_size_bytes {
                    let ctxs: Vec<(String, String, String)> = {
                        let mut stmt = tx.prepare(
                            "SELECT DISTINCT execution_id, IFNULL(trial_id, ''), element \
                             FROM artifacts",
                        )?;
                        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                            .collect::<Result<Vec<_>, _>>()?
                    };
                    for (exec, trial, element) in ctxs {
                        let total: i64 = tx.query_row(
                            "SELECT IFNULL(SUM(size_bytes), 0) FROM artifacts \
                             WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                               AND element = ?3",
                            rusqlite::params![exec, trial, element],
                            |r| r.get(0),
                        )?;
                        if (total as u64) <= cap {
                            continue;
                        }
                        // Iterate oldest-first and evict until within cap.
                        let victims: Vec<(String, i64)> = {
                            let mut stmt = tx.prepare(
                                "SELECT id, size_bytes FROM artifacts \
                                 WHERE execution_id = ?1 AND IFNULL(trial_id, '') = ?2 \
                                   AND element = ?3 \
                                 ORDER BY created_at ASC",
                            )?;
                            stmt.query_map(
                                rusqlite::params![exec, trial, element],
                                |r| Ok((r.get(0)?, r.get(1)?)),
                            )?
                            .collect::<Result<Vec<_>, _>>()?
                        };
                        let mut remaining = total as u64;
                        for (id, sz) in victims {
                            if remaining <= cap { break; }
                            tx.execute(
                                "DELETE FROM artifacts WHERE id = ?1",
                                rusqlite::params![id],
                            )?;
                            let sz = sz.max(0) as u64;
                            remaining = remaining.saturating_sub(sz);
                            removed = removed.saturating_add(1);
                            bytes_freed = bytes_freed.saturating_add(sz);
                        }
                    }
                }

                tx.commit()?;
                Ok((removed, bytes_freed))
            })
            .await
            .map_err(rusqlite_err("artifact.run_retention"))?;

        Ok(CleanupReport {
            artifacts_removed: removed,
            bytes_freed,
        })
    }
}

// ---------------------------------------------------------------------------
// Row-shape helper.
// ---------------------------------------------------------------------------

struct ArtifactRow {
    id:           String,
    execution_id: String,
    trial_id:     Option<String>,
    element:      String,
    name:         String,
    content_type: String,
    size_bytes:   Option<i64>,
    uri:          Option<String>,
    created_at:   String,
}

impl ArtifactRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(ArtifactRow {
            id:           r.get(0)?,
            execution_id: r.get(1)?,
            trial_id:     r.get(2)?,
            element:      r.get(3)?,
            name:         r.get(4)?,
            content_type: r.get(5)?,
            size_bytes:   r.get(6)?,
            uri:          r.get(7)?,
            created_at:   r.get(8)?,
        })
    }

    fn into_record(self) -> Result<ArtifactRecord, serde_json::Error> {
        let id: ArtifactId = serde_json::from_str(&format!("\"{}\"", self.id))?;
        let execution: Option<ExecutionId> = if self.execution_id.is_empty() {
            None
        } else {
            Some(serde_json::from_str(&format!("\"{}\"", self.execution_id))?)
        };
        let trial: Option<TrialId> = match self.trial_id {
            Some(s) if !s.is_empty() => {
                Some(serde_json::from_str(&format!("\"{s}\""))?)
            }
            _ => None,
        };
        let element: ElementName = serde_json::from_str(&format!("\"{}\"", self.element))?;
        let stored_at: Timestamp =
            serde_json::from_str(&format!("\"{}\"", self.created_at))?;
        let size_bytes = self.size_bytes.unwrap_or(0).max(0) as u64;
        let artifact = ArtifactRef::builder()
            .element(element.clone())
            .name(self.name)
            .uri(self.uri.clone().unwrap_or_default())
            .content_type(self.content_type)
            .build();
        Ok(ArtifactRecord {
            id,
            context: ArtifactContext { execution, trial, element: Some(element) },
            artifact,
            stored_at,
            size_bytes,
            sha256: None,
        })
    }
}
