// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionRepository` impl backed by `execution_plans` + `executions`.
//!
//! Both plans and executions live as JSON blobs keyed by id.
//! Indexable filter fields (fingerprint, status, timestamps) live in
//! dedicated columns so SQL drives the bulk of the work; label/tag
//! filters (rare in practice) fall back to a post-fetch Rust filter.

use async_trait::async_trait;
use paramodel_elements::{Fingerprint, Labels, Tags};
use paramodel_executor::{ExecutionId, ExecutionResults, ExecutionStatus};
use paramodel_persistence::{
    ExecutionFilter, ExecutionRepository, ExecutionPlanSummary, ExecutionSummary, Page,
    PageRequest, PageToken, PersistenceError, PersistenceResult, PlanFilter,
};
use paramodel_plan::{ExecutionPlan, ExecutionPlanId};
use paramodel_trials::TrialStatus;
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::{SqliteStore, json_err, rusqlite_err};

#[async_trait]
impl ExecutionRepository for SqliteStore {
    async fn save_plan(&self, plan: &ExecutionPlan) -> PersistenceResult<()> {
        let id   = plan.id.to_string();
        let fp   = plan.fingerprint().to_string();
        let body = serde_json::to_string(plan).map_err(json_err("repo.save_plan"))?;
        let created = plan.metadata.compiled_at.to_string();

        let id_for_conflict = id.clone();
        let existing: Option<String> = self.conn
            .call({
                let id = id.clone();
                move |c| -> Result<Option<String>, rusqlite::Error> {
                    c.query_row(
                        "SELECT plan_json FROM execution_plans WHERE id = ?1",
                        rusqlite::params![id],
                        |r| r.get(0),
                    )
                    .optional()
                }
            })
            .await
            .map_err(rusqlite_err("repo.save_plan"))?;
        if let Some(existing) = existing {
            if existing == body {
                return Ok(()); // idempotent
            }
            return Err(PersistenceError::Conflict {
                kind: "execution_plan",
                id:   id_for_conflict,
            });
        }
        let source_fp = plan.source_plan_fingerprint.to_string();
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "INSERT INTO execution_plans \
                     (id, fingerprint, source_plan_fingerprint, plan_json, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, fp, source_fp, body, created],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("repo.save_plan"))?;
        Ok(())
    }

    async fn get_plan(&self, id: &ExecutionPlanId) -> PersistenceResult<ExecutionPlan> {
        let key = id.to_string();
        let id_for_error = key.clone();
        let raw = self.conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT plan_json FROM execution_plans WHERE id = ?1",
                    rusqlite::params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("repo.get_plan"))?;
        let body = raw.ok_or_else(|| PersistenceError::NotFound {
            kind: "execution_plan",
            id:   id_for_error,
        })?;
        serde_json::from_str(&body).map_err(json_err("repo.get_plan"))
    }

    async fn get_plan_by_fingerprint(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<ExecutionPlan>> {
        let fp = fp.to_string();
        let raw = self.conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT plan_json FROM execution_plans WHERE fingerprint = ?1",
                    rusqlite::params![fp],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("repo.get_plan_by_fingerprint"))?;
        match raw {
            None    => Ok(None),
            Some(s) => serde_json::from_str(&s).map(Some).map_err(json_err("repo.get_plan_by_fingerprint")),
        }
    }

    async fn list_plans(
        &self,
        filter: &PlanFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionPlanSummary>> {
        // For v1: do a broad SQL fetch by fingerprint / time range,
        // then filter labels/tags/name in-memory. At expected catalog
        // sizes this is fine; specialise later if rows grow.
        let page = page.clamped();
        let size = page.size as i64;
        let limit = size + 1;
        let cursor = page.token.as_ref().map(|t| t.as_str().to_owned());
        let fp = filter.source_plan_fingerprint.as_ref().map(Fingerprint::to_string);
        let after  = filter.compiled_after.map(|t| t.to_string());
        let before = filter.compiled_before.map(|t| t.to_string());
        let plan_filter = filter.clone();

        let rows = self.conn
            .call(move |c| -> Result<Vec<(String, String)>, rusqlite::Error> {
                let mut sql = String::from(
                    "SELECT id, plan_json FROM execution_plans WHERE 1=1",
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(fp) = fp.as_ref() {
                    sql.push_str(" AND source_plan_fingerprint = ?");
                    params.push(Box::new(fp.clone()));
                }
                if let Some(t) = after.as_ref() {
                    sql.push_str(" AND created_at >= ?");
                    params.push(Box::new(t.clone()));
                }
                if let Some(t) = before.as_ref() {
                    sql.push_str(" AND created_at <= ?");
                    params.push(Box::new(t.clone()));
                }
                if let Some(cur) = cursor.as_ref() {
                    sql.push_str(" AND id > ?");
                    params.push(Box::new(cur.clone()));
                }
                sql.push_str(" ORDER BY id ASC LIMIT ?");
                params.push(Box::new(limit));
                let mut stmt = c.prepare(&sql)?;
                let rows: Vec<(String, String)> = stmt
                    .query_map(rusqlite::params_from_iter(params), |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(rusqlite_err("repo.list_plans"))?;

        let mut items: Vec<ExecutionPlanSummary> = Vec::with_capacity(rows.len());
        let mut last_id: Option<String> = None;
        for (id, body) in &rows {
            let plan: ExecutionPlan = serde_json::from_str(body)
                .map_err(json_err("repo.list_plans"))?;
            if !matches_labels(&plan.labels, &plan_filter.has_label)
                || !matches_tags(&plan.tags, &plan_filter.has_tag)
            {
                continue;
            }
            if let Some(substr) = plan_filter.name_contains.as_ref() {
                // Execution plans don't carry a human name; match
                // against the plan's compiler_version + id as a
                // lightweight substitute.
                let haystack = format!("{}|{}", plan.id, plan.metadata.compiler_version);
                if !haystack.contains(substr) { continue; }
            }
            items.push(ExecutionPlanSummary {
                id:                      plan.id.clone(),
                source_plan_fingerprint: plan.source_plan_fingerprint,
                compiled_at:             plan.metadata.compiled_at,
                trial_count:             plan.metadata.trial_count,
                step_count:              plan.metadata.step_count,
                labels:                  plan.labels.clone(),
                tags:                    plan.tags.clone(),
            });
            last_id = Some(id.clone());
            if items.len() >= size as usize { break; }
        }
        let has_next = rows.len() > size as usize;
        let next_token = if has_next {
            last_id.map(PageToken::new)
        } else {
            None
        };
        Ok(Page { items, next_token, total_hint: None })
    }

    async fn save_execution(&self, results: &ExecutionResults) -> PersistenceResult<()> {
        let id   = results.execution_id.to_string();
        let body = serde_json::to_string(results).map_err(json_err("repo.save_execution"))?;
        let started   = results.started_at.to_string();
        let completed = Some(results.completed_at.to_string());
        let status    = status_str(results.status);

        // We don't know the plan fingerprint directly on
        // ExecutionResults (it's ExecutionId-keyed). Look up via the
        // journal? For v1 we accept an unknown fingerprint and let
        // later callers set it via set_plan_fingerprint if needed.
        // Store empty string and treat as "unknown".
        let plan_fp = String::new();

        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                c.execute(
                    "INSERT INTO executions \
                     (id, plan_fingerprint, started_at, completed_at, status, summary_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(id) DO UPDATE SET \
                       plan_fingerprint = excluded.plan_fingerprint, \
                       started_at       = excluded.started_at, \
                       completed_at     = excluded.completed_at, \
                       status           = excluded.status, \
                       summary_json     = excluded.summary_json",
                    rusqlite::params![id, plan_fp, started, completed, status, body],
                )?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("repo.save_execution"))?;
        Ok(())
    }

    async fn get_execution(
        &self,
        id: &ExecutionId,
    ) -> PersistenceResult<ExecutionResults> {
        let key = id.to_string();
        let id_for_error = key.clone();
        let raw = self.conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT summary_json FROM executions WHERE id = ?1",
                    rusqlite::params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("repo.get_execution"))?;
        let body = raw.ok_or_else(|| PersistenceError::NotFound {
            kind: "execution",
            id:   id_for_error,
        })?;
        serde_json::from_str(&body).map_err(json_err("repo.get_execution"))
    }

    async fn list_executions(
        &self,
        filter: &ExecutionFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>> {
        let page = page.clamped();
        let size = page.size as i64;
        let limit = size + 1;
        let cursor = page.token.as_ref().map(|t| t.as_str().to_owned());
        let fp = filter.plan_fingerprint.as_ref().map(Fingerprint::to_string);
        let status = filter.status.map(status_str);
        let after  = filter.started_after.map(|t| t.to_string());
        let before = filter.started_before.map(|t| t.to_string());

        let rows = self.conn
            .call(move |c| -> Result<Vec<(String, String)>, rusqlite::Error> {
                let mut sql = String::from(
                    "SELECT id, summary_json FROM executions WHERE 1=1",
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
                if let Some(fp) = fp.as_ref() {
                    sql.push_str(" AND plan_fingerprint = ?");
                    params.push(Box::new(fp.clone()));
                }
                if let Some(s) = status.as_ref() {
                    sql.push_str(" AND status = ?");
                    params.push(Box::new(s.to_string()));
                }
                if let Some(t) = after.as_ref() {
                    sql.push_str(" AND started_at >= ?");
                    params.push(Box::new(t.clone()));
                }
                if let Some(t) = before.as_ref() {
                    sql.push_str(" AND started_at <= ?");
                    params.push(Box::new(t.clone()));
                }
                if let Some(cur) = cursor.as_ref() {
                    sql.push_str(" AND id > ?");
                    params.push(Box::new(cur.clone()));
                }
                sql.push_str(" ORDER BY id ASC LIMIT ?");
                params.push(Box::new(limit));
                let mut stmt = c.prepare(&sql)?;
                let rows: Vec<(String, String)> = stmt
                    .query_map(rusqlite::params_from_iter(params), |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(rusqlite_err("repo.list_executions"))?;

        let mut items: Vec<ExecutionSummary> = Vec::with_capacity(rows.len());
        let mut last_id: Option<String> = None;
        for (id, body) in &rows {
            let res: ExecutionResults = serde_json::from_str(body)
                .map_err(json_err("repo.list_executions"))?;
            items.push(summary_of(&res));
            last_id = Some(id.clone());
            if items.len() >= size as usize { break; }
        }
        let has_next = rows.len() > size as usize;
        Ok(Page {
            items,
            next_token: if has_next { last_id.map(PageToken::new) } else { None },
            total_hint: None,
        })
    }

    async fn executions_of_plan(
        &self,
        plan: &ExecutionPlanId,
        page: PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>> {
        // The `executions` row doesn't cross-reference plan id
        // directly (only plan_fingerprint), so look the fingerprint
        // up first, then delegate to `list_executions`.
        let plan = self.get_plan(plan).await?;
        let fp = plan.fingerprint();
        self.list_executions(
            &ExecutionFilter {
                plan_fingerprint: Some(fp),
                ..Default::default()
            },
            page,
        )
        .await
    }

    async fn latest_execution_of_plan(
        &self,
        plan: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionSummary>> {
        let plan = match self.get_plan(plan).await {
            Ok(p) => p,
            Err(PersistenceError::NotFound { .. }) => return Ok(None),
            Err(e) => return Err(e),
        };
        let fp = plan.fingerprint().to_string();
        let raw = self.conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT summary_json FROM executions \
                     WHERE plan_fingerprint = ?1 \
                     ORDER BY started_at DESC LIMIT 1",
                    rusqlite::params![fp],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("repo.latest_execution_of_plan"))?;
        match raw {
            None => Ok(None),
            Some(body) => {
                let res: ExecutionResults = serde_json::from_str(&body)
                    .map_err(json_err("repo.latest_execution_of_plan"))?;
                Ok(Some(summary_of(&res)))
            }
        }
    }
}

fn status_str(s: ExecutionStatus) -> &'static str {
    match s {
        ExecutionStatus::Succeeded      => "succeeded",
        ExecutionStatus::PartialFailure => "partial_failure",
        ExecutionStatus::Halted         => "halted",
        ExecutionStatus::Cancelled      => "cancelled",
    }
}

fn matches_labels(actual: &Labels, required: &[(paramodel_elements::LabelKey, paramodel_elements::LabelValue)]) -> bool {
    required.iter().all(|(k, v)| actual.get(k).is_some_and(|av| av == v))
}

fn matches_tags(actual: &Tags, required: &[(paramodel_elements::TagKey, paramodel_elements::TagValue)]) -> bool {
    required.iter().all(|(k, v)| actual.get(k).is_some_and(|av| av == v))
}

fn summary_of(r: &ExecutionResults) -> ExecutionSummary {
    let mut succ = 0u32;
    let mut fail = 0u32;
    let mut skip = 0u32;
    for t in &r.trial_results {
        match t.status {
            TrialStatus::Completed => succ = succ.saturating_add(1),
            TrialStatus::Failed    => fail = fail.saturating_add(1),
            TrialStatus::Skipped   => skip = skip.saturating_add(1),
            _ => {}
        }
    }
    let total = u32::try_from(r.trial_results.len()).unwrap_or(u32::MAX);
    ExecutionSummary {
        id:                r.execution_id,
        // Same "unknown plan fingerprint" placeholder as save_execution.
        plan_fingerprint:  paramodel_elements::Fingerprint::of(b""),
        final_status:      r.status,
        started_at:        r.started_at,
        completed_at:      Some(r.completed_at),
        successful_trials: succ,
        failed_trials:     fail,
        skipped_trials:    skip,
        total_trials:      total,
    }
}
