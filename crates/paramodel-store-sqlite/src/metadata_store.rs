// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MetadataStore` impl backed by `plan_metadata`.
//!
//! The `scope` column discriminates between test-plan + execution-
//! plan metadata; both share the same table because their access
//! patterns (save-by-id, get-by-id, list-with-pagination) are
//! identical. Execution / Trial / Artifact label and tag scopes land
//! in dedicated tables via `set_labels` / `set_tags`.

use async_trait::async_trait;
use paramodel_elements::{Fingerprint, Labels, Tags};
use paramodel_persistence::{
    MetadataScope, MetadataStore, Page, PageRequest, PageToken, PersistenceResult,
};
use paramodel_plan::{ExecutionPlanId, ExecutionPlanMetadata, TestPlanMetadata};
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::{SqliteStore, json_err, rusqlite_err};

const SCOPE_TEST_PLAN:      &str = "test_plan";
const SCOPE_EXECUTION_PLAN: &str = "execution_plan";
const SCOPE_EXECUTION:      &str = "execution";
const SCOPE_TRIAL:          &str = "trial";
const SCOPE_ARTIFACT:       &str = "artifact";

#[async_trait]
impl MetadataStore for SqliteStore {
    async fn save_test_plan_metadata(
        &self,
        fp:   &Fingerprint,
        meta: &TestPlanMetadata,
    ) -> PersistenceResult<()> {
        upsert_metadata(self, SCOPE_TEST_PLAN, &fp.to_string(), meta, "metadata.save_test_plan").await
    }

    async fn get_test_plan_metadata(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<TestPlanMetadata>> {
        fetch_metadata(self, SCOPE_TEST_PLAN, &fp.to_string(), "metadata.get_test_plan").await
    }

    async fn list_test_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(Fingerprint, TestPlanMetadata)>> {
        let rows = list_scope(self, SCOPE_TEST_PLAN, page, "metadata.list_test_plans").await?;
        let mut items = Vec::with_capacity(rows.items.len());
        for (id, body) in rows.items {
            let fp: Fingerprint = serde_json::from_str(&format!("\"{id}\""))
                .map_err(json_err("metadata.list_test_plans"))?;
            let m: TestPlanMetadata = serde_json::from_str(&body)
                .map_err(json_err("metadata.list_test_plans"))?;
            items.push((fp, m));
        }
        Ok(Page { items, next_token: rows.next_token, total_hint: rows.total_hint })
    }

    async fn save_execution_plan_metadata(
        &self,
        id:   &ExecutionPlanId,
        meta: &ExecutionPlanMetadata,
    ) -> PersistenceResult<()> {
        upsert_metadata(self, SCOPE_EXECUTION_PLAN, &id.to_string(), meta, "metadata.save_execution_plan").await
    }

    async fn get_execution_plan_metadata(
        &self,
        id: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionPlanMetadata>> {
        fetch_metadata(self, SCOPE_EXECUTION_PLAN, &id.to_string(), "metadata.get_execution_plan").await
    }

    async fn list_execution_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(ExecutionPlanId, ExecutionPlanMetadata)>> {
        let rows = list_scope(self, SCOPE_EXECUTION_PLAN, page, "metadata.list_execution_plans").await?;
        let mut items = Vec::with_capacity(rows.items.len());
        for (id, body) in rows.items {
            let eid: ExecutionPlanId = serde_json::from_str(&format!("\"{id}\""))
                .map_err(json_err("metadata.list_execution_plans"))?;
            let m: ExecutionPlanMetadata = serde_json::from_str(&body)
                .map_err(json_err("metadata.list_execution_plans"))?;
            items.push((eid, m));
        }
        Ok(Page { items, next_token: rows.next_token, total_hint: rows.total_hint })
    }

    async fn set_labels(
        &self,
        scope:  &MetadataScope,
        labels: &Labels,
    ) -> PersistenceResult<()> {
        set_attr(self, scope, "labels_json", labels, "metadata.set_labels").await
    }

    async fn set_tags(
        &self,
        scope: &MetadataScope,
        tags:  &Tags,
    ) -> PersistenceResult<()> {
        set_attr(self, scope, "tags_json", tags, "metadata.set_tags").await
    }
}

async fn upsert_metadata<T: serde::Serialize>(
    store: &SqliteStore,
    scope: &'static str,
    id:    &str,
    meta:  &T,
    ctx:   &'static str,
) -> PersistenceResult<()> {
    let body = serde_json::to_string(meta).map_err(json_err(ctx))?;
    let id = id.to_owned();
    store.conn
        .call(move |c| -> Result<(), rusqlite::Error> {
            c.execute(
                "INSERT INTO plan_metadata (scope, id, metadata_json) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(scope, id) DO UPDATE SET metadata_json = excluded.metadata_json",
                rusqlite::params![scope, id, body],
            )?;
            Ok(())
        })
        .await
        .map_err(rusqlite_err(ctx))?;
    Ok(())
}

async fn fetch_metadata<T: for<'de> serde::Deserialize<'de>>(
    store: &SqliteStore,
    scope: &'static str,
    id:    &str,
    ctx:   &'static str,
) -> PersistenceResult<Option<T>> {
    let id = id.to_owned();
    let raw = store.conn
        .call(move |c| -> Result<Option<String>, rusqlite::Error> {
            c.query_row(
                "SELECT metadata_json FROM plan_metadata WHERE scope = ?1 AND id = ?2",
                rusqlite::params![scope, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
        .map_err(rusqlite_err(ctx))?;
    match raw {
        None    => Ok(None),
        Some(s) => serde_json::from_str(&s).map(Some).map_err(json_err(ctx)),
    }
}

struct RawPage {
    items:      Vec<(String, String)>,
    next_token: Option<PageToken>,
    total_hint: Option<u64>,
}

async fn list_scope(
    store: &SqliteStore,
    scope: &'static str,
    page:  PageRequest,
    ctx:   &'static str,
) -> PersistenceResult<RawPage> {
    let page = page.clamped();
    let size = page.size as i64;
    let cursor = page.token.as_ref().map(|t| t.as_str().to_owned());
    let rows = store.conn
        .call(move |c| -> Result<Vec<(String, String)>, rusqlite::Error> {
            let limit = size + 1;
            match cursor {
                None => {
                    let mut stmt = c.prepare(
                        "SELECT id, metadata_json FROM plan_metadata \
                         WHERE scope = ?1 ORDER BY id ASC LIMIT ?2",
                    )?;
                    let rows: Vec<(String, String)> = stmt
                        .query_map(rusqlite::params![scope, limit], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })?
                        .collect::<Result<_, _>>()?;
                    Ok(rows)
                }
                Some(cur) => {
                    let mut stmt = c.prepare(
                        "SELECT id, metadata_json FROM plan_metadata \
                         WHERE scope = ?1 AND id > ?2 \
                         ORDER BY id ASC LIMIT ?3",
                    )?;
                    let rows: Vec<(String, String)> = stmt
                        .query_map(rusqlite::params![scope, cur, limit], |r| {
                            Ok((r.get(0)?, r.get(1)?))
                        })?
                        .collect::<Result<_, _>>()?;
                    Ok(rows)
                }
            }
        })
        .await
        .map_err(rusqlite_err(ctx))?;
    let has_next = rows.len() > size as usize;
    let kept: Vec<(String, String)> = rows.into_iter().take(size as usize).collect();
    let next_token = if has_next {
        kept.last().map(|(id, _)| PageToken::new(id.clone()))
    } else {
        None
    };
    Ok(RawPage { items: kept, next_token, total_hint: None })
}

async fn set_attr<T: serde::Serialize>(
    store: &SqliteStore,
    scope: &MetadataScope,
    col:   &'static str,
    value: &T,
    ctx:   &'static str,
) -> PersistenceResult<()> {
    let (sc, id) = scope_parts(scope);
    let body = serde_json::to_string(value).map_err(json_err(ctx))?;
    // `metadata_json` default for INSERT path must be valid JSON; use
    // `{}` if the record doesn't exist yet.
    let sql = format!(
        "INSERT INTO plan_metadata (scope, id, metadata_json, {col}) \
         VALUES (?1, ?2, '{{}}', ?3) \
         ON CONFLICT(scope, id) DO UPDATE SET {col} = excluded.{col}"
    );
    store.conn
        .call(move |c| -> Result<(), rusqlite::Error> {
            c.execute(&sql, rusqlite::params![sc, id, body])?;
            Ok(())
        })
        .await
        .map_err(rusqlite_err(ctx))?;
    Ok(())
}

fn scope_parts(scope: &MetadataScope) -> (&'static str, String) {
    match scope {
        MetadataScope::TestPlan      { fingerprint } => (SCOPE_TEST_PLAN,      fingerprint.to_string()),
        MetadataScope::ExecutionPlan { id }          => (SCOPE_EXECUTION_PLAN, id.to_string()),
        MetadataScope::Execution     { id }          => (SCOPE_EXECUTION,     id.to_string()),
        MetadataScope::Trial         { id }          => (SCOPE_TRIAL,         id.to_string()),
        MetadataScope::Artifact      { id }          => (SCOPE_ARTIFACT,      id.to_string()),
    }
}
