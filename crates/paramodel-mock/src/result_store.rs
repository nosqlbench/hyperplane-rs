// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockResultStore` — in-memory implementation of
//! [`paramodel_persistence::ResultStore`].
//!
//! Implements the full [`paramodel_persistence::ResultFilter`] AST
//! plus the [`paramodel_persistence::Aggregation`] operations. Group
//! dimensions that require trial-code parsing are handled at the
//! naive-string level (we don't link back to a compiled plan's
//! enumerator from within the mock).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use paramodel_elements::{ParameterName, TrialId, Value};
use paramodel_persistence::{
    AggregateResult, Aggregation, Comparison, GroupDimension, Page, PageRequest,
    PersistenceError, PersistenceResult, PersistenceStream, ResultFilter, ResultStore,
    TrialCodePattern,
};
use paramodel_trials::{TrialResult, TrialStatus};

/// In-memory result store.
#[derive(Debug, Default)]
pub struct MockResultStore {
    records: Mutex<BTreeMap<TrialId, TrialResult>>,
}

impl MockResultStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Evaluate `filter` against `record`.
    fn matches(record: &TrialResult, filter: &ResultFilter) -> bool {
        match filter {
            ResultFilter::Any => true,
            ResultFilter::TrialId { id } => record.trial_id == *id,
            ResultFilter::ExecutionId { .. }
            | ResultFilter::PlanFingerprint { .. }
            | ResultFilter::Assignment { .. }
            | ResultFilter::TrialCode { .. } => {
                // The `TrialResult` schema doesn't carry execution id,
                // plan fingerprint, assignment map, or trial code
                // directly in v0.1. A richer store linking results
                // to executions would.
                false
            }
            ResultFilter::Status { status } => record.status == *status,
            ResultFilter::StatusIn { statuses } => statuses.contains(&record.status),
            ResultFilter::StartedAfter { ts } => record.timing.started_at > *ts,
            ResultFilter::StartedBefore { ts } => record.timing.started_at < *ts,
            ResultFilter::AttemptNumber { cmp, value } => {
                compare_u32(record.attempt, *cmp, *value)
            }
            ResultFilter::Metric { coord, cmp, value } => record
                .metrics
                .get(&coord.element, &coord.parameter)
                .is_some_and(|v| compare_values(v, *cmp, value)),
            ResultFilter::LabelEquals { key, value } => {
                record.labels.get(key) == Some(value)
            }
            ResultFilter::TagEquals { key, value } => {
                record.tags.get(key) == Some(value)
            }
            ResultFilter::And { children } => {
                children.iter().all(|f| Self::matches(record, f))
            }
            ResultFilter::Or { children } => {
                children.iter().any(|f| Self::matches(record, f))
            }
            ResultFilter::Not { child } => !Self::matches(record, child),
        }
    }

    fn aggregate_scalar(
        values: Vec<f64>,
        op:     &AggregationKind,
        param:  &ParameterName,
    ) -> Option<AggregateResult> {
        if values.is_empty() {
            return None;
        }
        let scalar = match op {
            AggregationKind::Min => {
                values.iter().copied().fold(f64::INFINITY, f64::min)
            }
            AggregationKind::Max => {
                values.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            }
            AggregationKind::Sum => values.iter().sum(),
            AggregationKind::Avg => {
                #[allow(
                    clippy::cast_precision_loss,
                    reason = "aggregate mock; small counts"
                )]
                {
                    values.iter().sum::<f64>() / values.len() as f64
                }
            }
            AggregationKind::Percentile(p) => {
                let mut sorted = values;
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                #[allow(
                    clippy::cast_precision_loss,
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "aggregate mock; approximate percentile"
                )]
                let idx = ((sorted.len() - 1) as f64 * (*p).clamp(0.0, 1.0)) as usize;
                sorted[idx]
            }
        };
        Some(AggregateResult::Scalar {
            value: Value::double(param.clone(), scalar, None),
        })
    }

    fn compute_aggregation(
        records: &[TrialResult],
        agg:     &Aggregation,
    ) -> PersistenceResult<AggregateResult> {
        match agg {
            Aggregation::Count => Ok(AggregateResult::Count {
                n: u64::try_from(records.len()).unwrap_or(u64::MAX),
            }),
            Aggregation::Min { metric }
            | Aggregation::Max { metric }
            | Aggregation::Sum { metric }
            | Aggregation::Avg { metric }
            | Aggregation::Percentile { metric, .. } => {
                let values: Vec<f64> = records
                    .iter()
                    .filter_map(|r| {
                        r.metrics
                            .get(&metric.element, &metric.parameter)
                            .and_then(as_numeric)
                    })
                    .collect();
                let kind = match agg {
                    Aggregation::Min { .. } => AggregationKind::Min,
                    Aggregation::Max { .. } => AggregationKind::Max,
                    Aggregation::Sum { .. } => AggregationKind::Sum,
                    Aggregation::Avg { .. } => AggregationKind::Avg,
                    Aggregation::Percentile { p, .. } => AggregationKind::Percentile(*p),
                    _ => unreachable!(),
                };
                Self::aggregate_scalar(values, &kind, &metric.parameter)
                    .ok_or_else(|| {
                        PersistenceError::InvalidQuery(
                            "no values for scalar aggregation".to_owned(),
                        )
                    })
            }
            Aggregation::GroupBy { dimension, then } => {
                let groups = group_by(records, dimension);
                let mut out: Vec<(Value, Box<AggregateResult>)> = Vec::new();
                for (key, bucket) in groups {
                    let sub = Self::compute_aggregation(&bucket, then)?;
                    out.push((key, Box::new(sub)));
                }
                Ok(AggregateResult::Grouped { groups: out })
            }
        }
    }
}

enum AggregationKind {
    Min,
    Max,
    Sum,
    Avg,
    Percentile(f64),
}

const fn compare_u32(lhs: u32, cmp: Comparison, rhs: u32) -> bool {
    match cmp {
        Comparison::Eq => lhs == rhs,
        Comparison::Ne => lhs != rhs,
        Comparison::Lt => lhs < rhs,
        Comparison::Le => lhs <= rhs,
        Comparison::Gt => lhs > rhs,
        Comparison::Ge => lhs >= rhs,
    }
}

fn compare_values(lhs: &Value, cmp: Comparison, rhs: &Value) -> bool {
    match (as_numeric(lhs), as_numeric(rhs)) {
        (Some(l), Some(r)) => match cmp {
            Comparison::Eq => (l - r).abs() < f64::EPSILON,
            Comparison::Ne => (l - r).abs() >= f64::EPSILON,
            Comparison::Lt => l < r,
            Comparison::Le => l <= r,
            Comparison::Gt => l > r,
            Comparison::Ge => l >= r,
        },
        _ => matches!(cmp, Comparison::Eq) && lhs == rhs,
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "metric comparison mock; i64→f64 conversion"
)]
const fn as_numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(i.value as f64),
        Value::Double(d) => Some(d.value),
        _ => None,
    }
}

fn group_by(
    records:   &[TrialResult],
    dimension: &GroupDimension,
) -> Vec<(Value, Vec<TrialResult>)> {
    // `Value`'s `PartialEq` compares provenance (including
    // `generated_at`), so two `Value::string(..., None)` calls
    // produce distinct values. We key buckets by a stable string
    // derived from the dimension and carry one representative `Value`
    // per group for the caller.
    let mut buckets: Vec<(String, Value, Vec<TrialResult>)> = Vec::new();
    for r in records {
        let (group_key, key_value) = match dimension {
            GroupDimension::Status => {
                let label = match r.status {
                    TrialStatus::Pending => "pending",
                    TrialStatus::InProgress => "in_progress",
                    TrialStatus::Completed => "completed",
                    TrialStatus::Failed => "failed",
                    TrialStatus::Skipped => "skipped",
                    TrialStatus::Cancelled => "cancelled",
                };
                (
                    format!("status:{label}"),
                    Value::string(
                        ParameterName::new("status").expect("valid param"),
                        label.to_owned(),
                        None,
                    ),
                )
            }
            GroupDimension::Assignment { coord } => {
                // Assignments aren't modeled on TrialResult; use a
                // sentinel per coord so the group is still emitted.
                let tag = format!("assignment:{}:{}", coord.element, coord.parameter);
                (
                    tag.clone(),
                    Value::string(
                        ParameterName::new("assignment").expect("valid param"),
                        tag,
                        None,
                    ),
                )
            }
            GroupDimension::TrialCodeDigit { position } => {
                let tag = format!("trial_code_digit:{position}");
                (
                    tag.clone(),
                    Value::string(
                        ParameterName::new("trial_code_digit").expect("valid param"),
                        tag,
                        None,
                    ),
                )
            }
        };
        if let Some(bucket) = buckets.iter_mut().find(|(k, _, _)| k == &group_key) {
            bucket.2.push(r.clone());
        } else {
            buckets.push((group_key, key_value, vec![r.clone()]));
        }
    }
    buckets.into_iter().map(|(_, v, rs)| (v, rs)).collect()
}

// Quiet the unused-warning on TrialCodePattern when the filter
// evaluator doesn't inspect it.
const _: fn(&TrialCodePattern) = |_| {};

#[async_trait]
impl ResultStore for MockResultStore {
    async fn save(&self, result: &TrialResult) -> PersistenceResult<()> {
        self.records
            .lock()
            .expect("poisoned")
            .insert(result.trial_id, result.clone());
        Ok(())
    }

    async fn get(&self, trial: &TrialId) -> PersistenceResult<Option<TrialResult>> {
        Ok(self.records.lock().expect("poisoned").get(trial).cloned())
    }

    async fn delete(&self, trial: &TrialId) -> PersistenceResult<()> {
        self.records.lock().expect("poisoned").remove(trial);
        Ok(())
    }

    async fn query(
        &self,
        filter: &ResultFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<TrialResult>> {
        let page = page.clamped();
        let items: Vec<TrialResult> = self
            .records
            .lock()
            .expect("poisoned")
            .values()
            .filter(|r| Self::matches(r, filter))
            .take(page.size as usize)
            .cloned()
            .collect();
        Ok(Page::terminal(items))
    }

    async fn stream(&self, filter: &ResultFilter) -> PersistenceStream<TrialResult> {
        let snapshot: Vec<TrialResult> = self
            .records
            .lock()
            .expect("poisoned")
            .values()
            .filter(|r| Self::matches(r, filter))
            .cloned()
            .collect();
        stream::iter(snapshot.into_iter().map(Ok)).boxed()
    }

    async fn count(&self, filter: &ResultFilter) -> PersistenceResult<u64> {
        Ok(u64::try_from(
            self.records
                .lock()
                .expect("poisoned")
                .values()
                .filter(|r| Self::matches(r, filter))
                .count(),
        )
        .unwrap_or(u64::MAX))
    }

    async fn aggregate(
        &self,
        filter: &ResultFilter,
        agg:    &Aggregation,
    ) -> PersistenceResult<AggregateResult> {
        let records: Vec<TrialResult> = self
            .records
            .lock()
            .expect("poisoned")
            .values()
            .filter(|r| Self::matches(r, filter))
            .cloned()
            .collect();
        Self::compute_aggregation(&records, agg)
    }
}
