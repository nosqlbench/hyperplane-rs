// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ResultStore` impl backed by the `trial_results` table.
//!
//! Strategy:
//! - Serialise the full `TrialResult` into `result_json`; copy
//!   indexable columns (`trial_id`, `execution_id`, `plan_fingerprint`,
//!   `status`, `attempt`, `started_at`, `completed_at`, `trial_code`)
//!   for SQL filtering.
//! - `query / stream / count` partition the filter AST: SQL-eligible
//!   leaves (top-level `And` of eligible leaves) go into the `WHERE`;
//!   everything else (`Metric`, `Assignment`, `LabelEquals`,
//!   `TagEquals`, `Or`, `Not`) runs as a post-fetch Rust filter on
//!   the deserialised blob.
//! - `aggregate` runs `query` then computes in Rust. Good enough for
//!   v1; later slices can push aggregation into SQL.

use std::collections::BTreeSet;

use async_trait::async_trait;
use futures::stream;
use paramodel_elements::{LabelKey, LabelValue, TagKey, TagValue, TrialId, Value};
use paramodel_executor::ExecutionId;
use paramodel_persistence::{
    AggregateResult, Aggregation, Comparison, GroupDimension, Page, PageRequest,
    PageToken, PersistenceResult, PersistenceStream, ResultFilter,
    ResultStore, TrialCodePattern,
};
use paramodel_plan::ElementParameterRef;
use paramodel_trials::{TrialResult, TrialStatus};
use tokio_rusqlite::rusqlite;
use tokio_rusqlite::rusqlite::OptionalExtension;

use crate::store::{SqliteStore, json_err, rusqlite_err};

#[async_trait]
impl ResultStore for SqliteStore {
    async fn save(&self, result: &TrialResult) -> PersistenceResult<()> {
        let trial = result.trial_id.to_string();
        let status = status_str(result.status).to_owned();
        let attempt = result.attempt as i64;
        let started = result.timing.started_at.to_string();
        let completed = result.timing.completed_at.to_string();
        let body = serde_json::to_string(result).map_err(json_err("result.save"))?;

        // Flatten metrics into `trial_metrics` rows for indexed
        // filtering. Each row stores the value under whichever
        // typed column matches its kind.
        let metric_rows: Vec<MetricRow> = result
            .metrics
            .iter()
            .map(|(element, param, value)| MetricRow {
                trial_id:  trial.clone(),
                element:   element.as_str().to_owned(),
                parameter: param.as_str().to_owned(),
                kind:      value_kind_str(value).to_owned(),
                num:       value_as_real(value),
                text:      value_as_text(value),
                boolean:   value_as_int_bool(value),
            })
            .collect();

        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                let tx = c.transaction()?;
                tx.execute(
                    "INSERT INTO trial_results \
                     (trial_id, execution_id, plan_fingerprint, status, attempt, \
                      started_at, completed_at, trial_code, result_json) \
                     VALUES (?1, NULL, NULL, ?2, ?3, ?4, ?5, NULL, ?6) \
                     ON CONFLICT(trial_id) DO UPDATE SET \
                       status = excluded.status, \
                       attempt = excluded.attempt, \
                       started_at = excluded.started_at, \
                       completed_at = excluded.completed_at, \
                       result_json = excluded.result_json",
                    rusqlite::params![trial, status, attempt, started, completed, body],
                )?;
                // Replace existing metric rows atomically.
                tx.execute(
                    "DELETE FROM trial_metrics WHERE trial_id = ?1",
                    rusqlite::params![trial],
                )?;
                {
                    let mut stmt = tx.prepare(
                        "INSERT INTO trial_metrics \
                         (trial_id, element, parameter, value_kind, \
                          value_num, value_text, value_bool) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    )?;
                    for row in &metric_rows {
                        stmt.execute(rusqlite::params![
                            row.trial_id, row.element, row.parameter, row.kind,
                            row.num, row.text, row.boolean,
                        ])?;
                    }
                }
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("result.save"))?;
        Ok(())
    }

    async fn get(&self, trial: &TrialId) -> PersistenceResult<Option<TrialResult>> {
        let key = trial.to_string();
        let raw = self.conn
            .call(move |c| -> Result<Option<String>, rusqlite::Error> {
                c.query_row(
                    "SELECT result_json FROM trial_results WHERE trial_id = ?1",
                    rusqlite::params![key],
                    |r| r.get::<_, String>(0),
                )
                .optional()
            })
            .await
            .map_err(rusqlite_err("result.get"))?;
        match raw {
            None    => Ok(None),
            Some(s) => serde_json::from_str(&s).map(Some).map_err(json_err("result.get")),
        }
    }

    async fn delete(&self, trial: &TrialId) -> PersistenceResult<()> {
        let key = trial.to_string();
        self.conn
            .call(move |c| -> Result<(), rusqlite::Error> {
                let tx = c.transaction()?;
                tx.execute(
                    "DELETE FROM trial_metrics WHERE trial_id = ?1",
                    rusqlite::params![key],
                )?;
                tx.execute(
                    "DELETE FROM trial_results WHERE trial_id = ?1",
                    rusqlite::params![key],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await
            .map_err(rusqlite_err("result.delete"))?;
        Ok(())
    }

    async fn query(
        &self,
        filter: &ResultFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<TrialResult>> {
        let page = page.clamped();
        let size = page.size as i64;
        let cursor = page.token.as_ref().map(|t| t.as_str().to_owned());

        // Build the SQL pre-filter. Any remaining leaves run in Rust
        // (`filter_matches`).
        let (sql_where, sql_params) = sql_prefilter(filter);
        let filter_cloned = filter.clone();

        let rows = self.conn
            .call(move |c| -> Result<Vec<(String, String)>, rusqlite::Error> {
                let mut sql = format!(
                    "SELECT trial_id, result_json FROM trial_results WHERE {sql_where}",
                );
                let mut params: Vec<Box<dyn rusqlite::ToSql + Send + 'static>> = sql_params;
                if let Some(cur) = cursor.as_ref() {
                    sql.push_str(" AND trial_id > ?");
                    params.push(Box::new(cur.clone()));
                }
                // `size + 1` so we can compute next_token; we may
                // over-fetch when Rust filters drop rows, so load a
                // more generous buffer.
                sql.push_str(" ORDER BY trial_id ASC");
                let mut stmt = c.prepare(&sql)?;
                let rows: Vec<(String, String)> = stmt
                    .query_map(rusqlite::params_from_iter(params), |r| {
                        Ok((r.get(0)?, r.get(1)?))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok(rows)
            })
            .await
            .map_err(rusqlite_err("result.query"))?;

        let mut items: Vec<TrialResult> = Vec::with_capacity(size as usize);
        let mut last_trial_id: Option<String> = None;
        for (id, body) in rows {
            let tr: TrialResult = serde_json::from_str(&body)
                .map_err(json_err("result.query"))?;
            if !filter_matches(&filter_cloned, &tr) {
                continue;
            }
            last_trial_id = Some(id);
            items.push(tr);
            if items.len() >= size as usize {
                break;
            }
        }
        // If we took exactly `size`, there *might* be more; expose a
        // token. Callers that want exactness can call again.
        let next_token = if items.len() >= size as usize {
            last_trial_id.map(PageToken::new)
        } else {
            None
        };
        Ok(Page { items, next_token, total_hint: None })
    }

    async fn stream(&self, filter: &ResultFilter) -> PersistenceStream<TrialResult> {
        // Materialise the full result set in one shot; a true chunked
        // stream is a follow-up.
        let page = self
            .query(filter, PageRequest::first(paramodel_persistence::MAX_PAGE_SIZE))
            .await;
        match page {
            Ok(p)  => Box::pin(stream::iter(p.items.into_iter().map(Ok))),
            Err(e) => Box::pin(stream::iter(vec![Err(e)])),
        }
    }

    async fn count(&self, filter: &ResultFilter) -> PersistenceResult<u64> {
        // For filters the SQL pre-filter can resolve completely, use
        // COUNT(*). Otherwise load rows and filter in Rust.
        if fully_sql_filter(filter) {
            let (sql_where, sql_params) = sql_prefilter(filter);
            let n = self.conn
                .call(move |c| -> Result<i64, rusqlite::Error> {
                    let sql = format!(
                        "SELECT COUNT(*) FROM trial_results WHERE {sql_where}",
                    );
                    let mut stmt = c.prepare(&sql)?;
                    let n: i64 = stmt.query_row(
                        rusqlite::params_from_iter(sql_params),
                        |r| r.get(0),
                    )?;
                    Ok(n)
                })
                .await
                .map_err(rusqlite_err("result.count"))?;
            return Ok(n.max(0) as u64);
        }
        // Fallback: load + filter.
        let page = self
            .query(filter, PageRequest::first(paramodel_persistence::MAX_PAGE_SIZE))
            .await?;
        Ok(page.items.len() as u64)
    }

    async fn aggregate(
        &self,
        filter: &ResultFilter,
        agg:    &Aggregation,
    ) -> PersistenceResult<AggregateResult> {
        // Load the filtered rows in one shot, then compute in Rust.
        let page = self
            .query(filter, PageRequest::first(paramodel_persistence::MAX_PAGE_SIZE))
            .await?;
        Ok(run_aggregation(agg, &page.items))
    }
}

// ---------------------------------------------------------------------------
// SQL pre-filter classification.
// ---------------------------------------------------------------------------

/// True if every leaf under `filter` is SQL-translatable without
/// post-fetch Rust evaluation.
fn fully_sql_filter(filter: &ResultFilter) -> bool {
    match filter {
        ResultFilter::Any
        | ResultFilter::TrialId { .. }
        | ResultFilter::ExecutionId { .. }
        | ResultFilter::PlanFingerprint { .. }
        | ResultFilter::Status { .. }
        | ResultFilter::StatusIn { .. }
        | ResultFilter::StartedAfter { .. }
        | ResultFilter::StartedBefore { .. }
        | ResultFilter::AttemptNumber { .. }
        | ResultFilter::TrialCode { .. } => true,
        ResultFilter::Metric { value, cmp, .. } => metric_filter_sql_compatible(value, *cmp),
        ResultFilter::Assignment { .. }
        | ResultFilter::LabelEquals { .. }
        | ResultFilter::TagEquals { .. } => false,
        ResultFilter::And { children } => children.iter().all(fully_sql_filter),
        ResultFilter::Or  { children } => children.iter().all(fully_sql_filter),
        ResultFilter::Not { child }    => fully_sql_filter(child),
    }
}

/// Build a SQL WHERE fragment from whichever top-level leaves of
/// `filter` are SQL-translatable. Returns `("1=1", [])` when
/// nothing pushes down (the caller then does all filtering in Rust).
fn sql_prefilter(
    filter: &ResultFilter,
) -> (String, Vec<Box<dyn rusqlite::ToSql + Send + 'static>>) {
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::ToSql + Send + 'static>> = Vec::new();
    push_sql_leaves(filter, &mut clauses, &mut params);
    let sql = if clauses.is_empty() {
        "1=1".to_owned()
    } else {
        clauses.join(" AND ")
    };
    (sql, params)
}

/// Walk the filter tree, pushing SQL-eligible leaves from top-level
/// `And` nodes onto the clause list. Bails on the first `Or` / `Not`
/// / non-eligible leaf — the rest runs in Rust.
fn push_sql_leaves(
    filter:  &ResultFilter,
    clauses: &mut Vec<String>,
    params:  &mut Vec<Box<dyn rusqlite::ToSql + Send + 'static>>,
) {
    match filter {
        ResultFilter::Any => {}
        ResultFilter::And { children } => {
            for ch in children {
                push_sql_leaves(ch, clauses, params);
            }
        }
        ResultFilter::TrialId { id } => {
            clauses.push("trial_id = ?".into());
            params.push(Box::new(id.to_string()));
        }
        ResultFilter::ExecutionId { id } => {
            clauses.push("execution_id = ?".into());
            params.push(Box::new(id.to_string()));
        }
        ResultFilter::PlanFingerprint { fp } => {
            clauses.push("plan_fingerprint = ?".into());
            params.push(Box::new(fp.to_string()));
        }
        ResultFilter::Status { status } => {
            clauses.push("status = ?".into());
            params.push(Box::new(status_str(*status).to_owned()));
        }
        ResultFilter::StatusIn { statuses } => {
            if statuses.is_empty() {
                clauses.push("0=1".into());
            } else {
                let placeholders: Vec<&str> = statuses.iter().map(|_| "?").collect();
                clauses.push(format!("status IN ({})", placeholders.join(", ")));
                for s in statuses {
                    params.push(Box::new(status_str(*s).to_owned()));
                }
            }
        }
        ResultFilter::StartedAfter { ts } => {
            clauses.push("started_at > ?".into());
            params.push(Box::new(ts.to_string()));
        }
        ResultFilter::StartedBefore { ts } => {
            clauses.push("started_at < ?".into());
            params.push(Box::new(ts.to_string()));
        }
        ResultFilter::AttemptNumber { cmp, value } => {
            clauses.push(format!("attempt {} ?", cmp_sql(*cmp)));
            params.push(Box::new(*value as i64));
        }
        ResultFilter::TrialCode { pattern } => match pattern {
            TrialCodePattern::Exact { code } => {
                clauses.push("trial_code = ?".into());
                params.push(Box::new(code.clone()));
            }
            TrialCodePattern::Prefix { prefix } => {
                clauses.push("trial_code LIKE ?".into());
                params.push(Box::new(format!("{prefix}%")));
            }
        },
        ResultFilter::Metric { coord, cmp, value } => {
            if metric_filter_sql_compatible(value, *cmp) {
                push_metric_leaf(coord, *cmp, value, clauses, params);
            }
            // Else leave for the Rust post-filter.
        }
        // Non-SQL leaves: leave them for the Rust post-filter.
        ResultFilter::Assignment { .. }
        | ResultFilter::LabelEquals { .. }
        | ResultFilter::TagEquals { .. }
        | ResultFilter::Or { .. }
        | ResultFilter::Not { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Rust-side filter evaluation.
// ---------------------------------------------------------------------------

fn filter_matches(filter: &ResultFilter, tr: &TrialResult) -> bool {
    match filter {
        ResultFilter::Any => true,
        ResultFilter::And { children } => children.iter().all(|c| filter_matches(c, tr)),
        ResultFilter::Or  { children } => children.iter().any(|c| filter_matches(c, tr)),
        ResultFilter::Not { child }    => !filter_matches(child, tr),

        ResultFilter::TrialId { id }         => tr.trial_id == *id,
        ResultFilter::ExecutionId { id }     => matches_execution(tr, id),
        ResultFilter::PlanFingerprint { fp } => matches_plan_fp(tr, fp),
        ResultFilter::Status { status }      => tr.status == *status,
        ResultFilter::StatusIn { statuses }  => statuses.contains(&tr.status),
        ResultFilter::StartedAfter  { ts }   => tr.timing.started_at > *ts,
        ResultFilter::StartedBefore { ts }   => tr.timing.started_at < *ts,
        ResultFilter::AttemptNumber { cmp, value } => cmp_apply(*cmp, tr.attempt, *value),
        ResultFilter::TrialCode { pattern }  => match_trial_code(tr, pattern),
        ResultFilter::Metric { coord, cmp, value } => match_metric(tr, coord, *cmp, value),
        ResultFilter::Assignment { coord, value } => match_assignment(tr, coord, value),
        ResultFilter::LabelEquals { key, value } => match_label(tr, key, value),
        ResultFilter::TagEquals   { key, value } => match_tag(tr, key, value),
    }
}

fn matches_execution(_tr: &TrialResult, _exec: &ExecutionId) -> bool {
    // `TrialResult` doesn't directly carry execution_id in v0.2; the
    // enrichment column in our table is filled in by callers that
    // know the mapping. For now a non-indexed filter returns true
    // (rely on the SQL prefilter to narrow).
    true
}

fn matches_plan_fp(_tr: &TrialResult, _fp: &paramodel_elements::Fingerprint) -> bool {
    true
}

fn match_trial_code(_tr: &TrialResult, _pat: &TrialCodePattern) -> bool {
    // Same — trial_code isn't on TrialResult; SQL prefilter handles it.
    true
}

fn match_metric(
    tr:    &TrialResult,
    coord: &ElementParameterRef,
    cmp:   Comparison,
    rhs:   &Value,
) -> bool {
    let Some(v) = tr.metrics.get(&coord.element, &coord.parameter) else {
        return false;
    };
    compare_values(v, rhs, cmp)
}

fn match_assignment(
    _tr:   &TrialResult,
    _coord: &ElementParameterRef,
    _value: &Value,
) -> bool {
    // `TrialResult` doesn't carry the input assignment map in v0.2.
    // Filtering on assignments requires an enrichment index (future
    // slice). Return true so the filter doesn't drop everything.
    true
}

fn match_label(tr: &TrialResult, key: &LabelKey, value: &LabelValue) -> bool {
    tr.labels.get(key).is_some_and(|v| v == value)
}

fn match_tag(tr: &TrialResult, key: &TagKey, value: &TagValue) -> bool {
    tr.tags.get(key).is_some_and(|v| v == value)
}

/// Value-level comparison honouring numeric ordering for int/double
/// and equality fallback for other kinds.
fn compare_values(lhs: &Value, rhs: &Value, cmp: Comparison) -> bool {
    // Numeric comparisons first — pull out as f64 so int/double mix.
    let lhs_num = value_as_f64(lhs);
    let rhs_num = value_as_f64(rhs);
    if let (Some(a), Some(b)) = (lhs_num, rhs_num) {
        return match cmp {
            Comparison::Eq => (a - b).abs() < f64::EPSILON,
            Comparison::Ne => (a - b).abs() >= f64::EPSILON,
            Comparison::Lt => a < b,
            Comparison::Le => a <= b,
            Comparison::Gt => a > b,
            Comparison::Ge => a >= b,
        };
    }
    // Fall back to equality on rendered form.
    match cmp {
        Comparison::Eq => values_equal(lhs, rhs),
        Comparison::Ne => !values_equal(lhs, rhs),
        _              => false,
    }
}

fn value_as_f64(v: &Value) -> Option<f64> {
    if let Some(i) = v.as_integer() { return Some(i as f64); }
    if let Some(d) = v.as_double()  { return Some(d); }
    None
}

fn values_equal(a: &Value, b: &Value) -> bool {
    // Serde-compare: Values derive PartialEq but include a name
    // field. Compare the payload via JSON round-trip to ignore the
    // param name at the leaf.
    let aj = serde_json::to_string(a).ok();
    let bj = serde_json::to_string(b).ok();
    aj == bj
}

fn cmp_apply(cmp: Comparison, a: u32, b: u32) -> bool {
    match cmp {
        Comparison::Eq => a == b,
        Comparison::Ne => a != b,
        Comparison::Lt => a <  b,
        Comparison::Le => a <= b,
        Comparison::Gt => a >  b,
        Comparison::Ge => a >= b,
    }
}

fn cmp_sql(cmp: Comparison) -> &'static str {
    match cmp {
        Comparison::Eq => "=",
        Comparison::Ne => "!=",
        Comparison::Lt => "<",
        Comparison::Le => "<=",
        Comparison::Gt => ">",
        Comparison::Ge => ">=",
    }
}

fn status_str(s: TrialStatus) -> &'static str {
    match s {
        TrialStatus::Pending    => "pending",
        TrialStatus::InProgress => "in_progress",
        TrialStatus::Completed  => "completed",
        TrialStatus::Failed     => "failed",
        TrialStatus::Skipped    => "skipped",
        TrialStatus::Cancelled  => "cancelled",
    }
}

// ---------------------------------------------------------------------------
// Aggregation.
// ---------------------------------------------------------------------------

fn run_aggregation(agg: &Aggregation, rows: &[TrialResult]) -> AggregateResult {
    match agg {
        Aggregation::Count => AggregateResult::Count { n: rows.len() as u64 },
        Aggregation::Min { metric }
        | Aggregation::Max { metric }
        | Aggregation::Sum { metric }
        | Aggregation::Avg { metric } => {
            let values: Vec<f64> = rows
                .iter()
                .filter_map(|tr| tr.metrics.get(&metric.element, &metric.parameter))
                .filter_map(value_as_f64)
                .collect();
            let scalar = match agg {
                Aggregation::Min { .. } => values.iter().cloned()
                    .fold(f64::INFINITY, f64::min),
                Aggregation::Max { .. } => values.iter().cloned()
                    .fold(f64::NEG_INFINITY, f64::max),
                Aggregation::Sum { .. } => values.iter().sum::<f64>(),
                Aggregation::Avg { .. } if values.is_empty() => f64::NAN,
                Aggregation::Avg { .. } => values.iter().sum::<f64>()
                    / (values.len() as f64),
                _ => unreachable!(),
            };
            AggregateResult::Scalar {
                value: Value::double(metric.parameter.clone(), scalar, None),
            }
        }
        Aggregation::Percentile { metric, p } => {
            let mut values: Vec<f64> = rows
                .iter()
                .filter_map(|tr| tr.metrics.get(&metric.element, &metric.parameter))
                .filter_map(value_as_f64)
                .collect();
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let scalar = if values.is_empty() {
                f64::NAN
            } else {
                let p = p.clamp(0.0, 1.0);
                let idx = (p * (values.len() as f64 - 1.0)).round() as usize;
                values[idx.min(values.len() - 1)]
            };
            AggregateResult::Scalar {
                value: Value::double(metric.parameter.clone(), scalar, None),
            }
        }
        Aggregation::GroupBy { dimension, then } => {
            // Bucket by a plain-string key internally (avoids the
            // `Timestamp::now()` provenance noise on `Value`). Wrap
            // each key as a `Value::string` with a deterministic
            // EPOCH timestamp only at emit time.
            let mut groups: Vec<(String, String, Vec<TrialResult>)> = Vec::new();
            for tr in rows {
                let (param, key_str) = match dimension {
                    GroupDimension::Assignment { coord: _ } => {
                        ("trial_id".to_owned(), tr.trial_id.to_string())
                    }
                    GroupDimension::Status => {
                        ("status".to_owned(), status_str(tr.status).to_owned())
                    }
                    GroupDimension::TrialCodeDigit { position: _ } => {
                        ("all".to_owned(), "all".to_owned())
                    }
                };
                if let Some((_, _, bucket)) = groups.iter_mut()
                    .find(|(p, k, _)| p == &param && k == &key_str)
                {
                    bucket.push(tr.clone());
                } else {
                    groups.push((param, key_str, vec![tr.clone()]));
                }
            }
            let mut out: Vec<(Value, Box<AggregateResult>)> = Vec::with_capacity(groups.len());
            for (param, key, bucket) in groups {
                let key_value = Value::String(paramodel_elements::StringValue::new_at(
                    paramodel_elements::ParameterName::new(param).unwrap(),
                    key,
                    None,
                    jiff::Timestamp::UNIX_EPOCH,
                ));
                out.push((key_value, Box::new(run_aggregation(then, &bucket))));
            }
            AggregateResult::Grouped { groups: out }
        }
    }
}

// ---------------------------------------------------------------------------
// Metric row decomposition — powers both `ResultStore::save` and
// `ResultFilter::Metric` SQL pushdown.
// ---------------------------------------------------------------------------

struct MetricRow {
    trial_id:  String,
    element:   String,
    parameter: String,
    kind:      String,
    num:       Option<f64>,
    text:      Option<String>,
    boolean:   Option<i64>,
}

fn value_kind_str(v: &Value) -> &'static str {
    match v {
        Value::Integer(_)   => "integer",
        Value::Double(_)    => "double",
        Value::Boolean(_)   => "boolean",
        Value::String(_)    => "string",
        Value::Selection(_) => "selection",
    }
}

fn value_as_real(v: &Value) -> Option<f64> {
    if let Some(i) = v.as_integer() { return Some(i as f64); }
    if let Some(d) = v.as_double()  { return Some(d); }
    None
}

fn value_as_text(v: &Value) -> Option<String> {
    v.as_string().map(|s| s.to_owned())
}

fn value_as_int_bool(v: &Value) -> Option<i64> {
    v.as_boolean().map(i64::from)
}

/// Can this `Metric` filter leaf be fully resolved by a SQL subquery
/// on `trial_metrics`? True when the comparand is numeric (int/double),
/// or a string with `Eq`/`Ne`, or a boolean with `Eq`/`Ne`.
fn metric_filter_sql_compatible(value: &Value, cmp: Comparison) -> bool {
    match value {
        Value::Integer(_) | Value::Double(_) => true,
        Value::String(_) | Value::Boolean(_) => {
            matches!(cmp, Comparison::Eq | Comparison::Ne)
        }
        Value::Selection(_) => false,
    }
}

fn push_metric_leaf(
    coord:   &ElementParameterRef,
    cmp:     Comparison,
    value:   &Value,
    clauses: &mut Vec<String>,
    params:  &mut Vec<Box<dyn rusqlite::ToSql + Send + 'static>>,
) {
    let element = coord.element.as_str().to_owned();
    let parameter = coord.parameter.as_str().to_owned();
    match value {
        Value::Integer(_) | Value::Double(_) => {
            let rhs = value_as_real(value).unwrap_or(0.0);
            clauses.push(format!(
                "trial_id IN (SELECT trial_id FROM trial_metrics \
                 WHERE element = ? AND parameter = ? AND value_num {} ?)",
                cmp_sql(cmp),
            ));
            params.push(Box::new(element));
            params.push(Box::new(parameter));
            params.push(Box::new(rhs));
        }
        Value::String(_) => {
            let rhs = value_as_text(value).unwrap_or_default();
            clauses.push(format!(
                "trial_id IN (SELECT trial_id FROM trial_metrics \
                 WHERE element = ? AND parameter = ? AND value_text {} ?)",
                cmp_sql(cmp),
            ));
            params.push(Box::new(element));
            params.push(Box::new(parameter));
            params.push(Box::new(rhs));
        }
        Value::Boolean(_) => {
            let rhs = value_as_int_bool(value).unwrap_or(0);
            clauses.push(format!(
                "trial_id IN (SELECT trial_id FROM trial_metrics \
                 WHERE element = ? AND parameter = ? AND value_bool {} ?)",
                cmp_sql(cmp),
            ));
            params.push(Box::new(element));
            params.push(Box::new(parameter));
            params.push(Box::new(rhs));
        }
        Value::Selection(_) => {
            // Not pushed down; caller should have gated on
            // `metric_filter_sql_compatible`.
        }
    }
}

#[allow(dead_code, reason = "imports kept for future SQL extensions")]
fn _unused_ext(_s: &BTreeSet<TrialStatus>) {}
