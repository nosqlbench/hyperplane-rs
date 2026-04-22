// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ResultStore` trait + `ResultFilter` + `Aggregation` (SRD-0012).
//!
//! `ResultFilter` is a declarative filter AST — no closures, no
//! string DSL. Backends are expected to push it down into native
//! queries (`SQL WHERE` in the `SQLite` case).
//!
//! `Aggregation` is intentionally shallow: scalar + single-dimension
//! group-by. Deeper analytics go through the streaming accessor and
//! run client-side.

use std::collections::BTreeSet;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{
    Fingerprint, LabelKey, LabelValue, TagKey, TagValue, TrialId, Value,
};
use paramodel_executor::ExecutionId;
use paramodel_plan::ElementParameterRef;
use paramodel_trials::{TrialResult, TrialStatus};
use serde::{Deserialize, Serialize};

use crate::error::PersistenceResult;
use crate::page::{Page, PageRequest, PersistenceStream};

// ---------------------------------------------------------------------------
// Comparison + TrialCodePattern.
// ---------------------------------------------------------------------------

/// Ordering comparator used by metric / attempt leaves.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// `a == b`.
    Eq,
    /// `a != b`.
    Ne,
    /// `a < b`.
    Lt,
    /// `a <= b`.
    Le,
    /// `a > b`.
    Gt,
    /// `a >= b`.
    Ge,
}

/// How the filter matches a trial code string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrialCodePattern {
    /// Exact match.
    Exact {
        /// The literal code (e.g. `"0x0003"`).
        code: String,
    },
    /// Prefix match.
    Prefix {
        /// The prefix to match.
        prefix: String,
    },
}

// ---------------------------------------------------------------------------
// ResultFilter.
// ---------------------------------------------------------------------------

/// Declarative filter AST. Leaves + three combinators; per SRD-0012 D4
/// the shape is pushdown-friendly by construction.
///
/// `Eq` is not derived because `Value` (embedded in `Metric` /
/// `Assignment`) is only `PartialEq` — floats may participate.
///
/// `Serialize` / `Deserialize` are implemented below by hand because
/// the derived impls fan out through `Value` + the self-referential
/// combinators in a way that drives rustc's trait-resolution past
/// any practical recursion limit when instantiated through
/// `serde_json::to_string` or similar.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum ResultFilter {
    /// Match every record.
    #[default]
    Any,

    // --- leaf predicates --------------------------------------------------
    /// Match one trial by id.
    TrialId {
        /// Target.
        id: TrialId,
    },
    /// Match trials belonging to an execution.
    ExecutionId {
        /// Target.
        id: ExecutionId,
    },
    /// Match trials belonging to any execution of a plan.
    PlanFingerprint {
        /// Plan fingerprint.
        fp: Fingerprint,
    },
    /// Match one terminal status.
    Status {
        /// Target.
        status: TrialStatus,
    },
    /// Match any of a set of statuses.
    StatusIn {
        /// Target set.
        statuses: BTreeSet<TrialStatus>,
    },
    /// Match trials started after a timestamp.
    StartedAfter {
        /// Lower bound (exclusive).
        ts: Timestamp,
    },
    /// Match trials started before a timestamp.
    StartedBefore {
        /// Upper bound (exclusive).
        ts: Timestamp,
    },
    /// Match on attempt number.
    AttemptNumber {
        /// Comparator.
        cmp:   Comparison,
        /// Right-hand side.
        value: u32,
    },
    /// Match metric threshold at a specific `(element, parameter)`.
    Metric {
        /// Metric coordinate.
        coord: ElementParameterRef,
        /// Comparator.
        cmp:   Comparison,
        /// Right-hand side. Type must match the parameter's kind.
        value: Value,
    },
    /// Match trials whose assignment equals `value` at `coord`.
    Assignment {
        /// Assignment coordinate.
        coord: ElementParameterRef,
        /// Expected value.
        value: Value,
    },
    /// Match by trial code pattern.
    TrialCode {
        /// Pattern to apply.
        pattern: TrialCodePattern,
    },
    /// Match by label equality.
    LabelEquals {
        /// Label key.
        key:   LabelKey,
        /// Expected value.
        value: LabelValue,
    },
    /// Match by tag equality.
    TagEquals {
        /// Tag key.
        key:   TagKey,
        /// Expected value.
        value: TagValue,
    },

    // --- combinators ------------------------------------------------------
    /// Match iff every child matches.
    And {
        /// Conjuncts.
        children: Vec<Self>,
    },
    /// Match iff any child matches.
    Or {
        /// Disjuncts.
        children: Vec<Self>,
    },
    /// Match iff the child does not.
    Not {
        /// Negated child.
        child: Box<Self>,
    },
}

impl ResultFilter {
    /// Combine with `other` under [`Self::And`].
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self::And {
            children: vec![self, other],
        }
    }

    /// Combine with `other` under [`Self::Or`].
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self::Or {
            children: vec![self, other],
        }
    }

    /// Negate — wraps in [`Self::Not`]. Named `negate` rather than
    /// `not` to avoid shadowing [`std::ops::Not::not`].
    #[must_use]
    pub fn negate(self) -> Self {
        Self::Not {
            child: Box::new(self),
        }
    }
}

// ---------------------------------------------------------------------------
// Aggregation + result.
// ---------------------------------------------------------------------------

/// Group dimension for `Aggregation::GroupBy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupDimension {
    /// Group by one assignment coordinate (e.g. `db.threads`).
    Assignment {
        /// Coordinate.
        coord: ElementParameterRef,
    },
    /// Group by terminal status.
    Status,
    /// Group by the digit at `position` in the trial code.
    TrialCodeDigit {
        /// 0-indexed digit position (most significant first).
        position: u32,
    },
}

/// Aggregation operator. Scalar aggregations yield `AggregateResult::Scalar`
/// or `Count`; `GroupBy` yields `Grouped`.
///
/// `Serialize` / `Deserialize` are implemented below by hand for the
/// same reason as [`ResultFilter`].
#[derive(Debug, Clone, PartialEq)]
pub enum Aggregation {
    /// Record count.
    Count,
    /// Minimum of a metric.
    Min {
        /// Metric coordinate.
        metric: ElementParameterRef,
    },
    /// Maximum of a metric.
    Max {
        /// Metric coordinate.
        metric: ElementParameterRef,
    },
    /// Sum of a metric.
    Sum {
        /// Metric coordinate.
        metric: ElementParameterRef,
    },
    /// Arithmetic mean of a metric.
    Avg {
        /// Metric coordinate.
        metric: ElementParameterRef,
    },
    /// `p`-th percentile of a metric. `p` is in `0.0..=1.0`.
    Percentile {
        /// Metric coordinate.
        metric: ElementParameterRef,
        /// Quantile.
        p:      f64,
    },
    /// Group by a single dimension, then apply `then` per group.
    GroupBy {
        /// Group dimension.
        dimension: GroupDimension,
        /// Per-group aggregation.
        then:      Box<Self>,
    },
}

/// What `aggregate` returns.
///
/// `Serialize` / `Deserialize` are implemented below by hand for the
/// same reason as [`ResultFilter`].
#[derive(Debug, Clone, PartialEq)]
pub enum AggregateResult {
    /// A scalar typed value (min / max / sum / avg / percentile).
    Scalar {
        /// The value.
        value: Value,
    },
    /// A count.
    Count {
        /// Record count.
        n: u64,
    },
    /// One sub-result per group value. Stored as an ordered list of
    /// `(group_key, result)` pairs; `Value` isn't `Ord`, so a
    /// `BTreeMap` isn't an option, and the backend fixes the order.
    Grouped {
        /// Group key + per-group aggregate.
        groups: Vec<(Value, Box<Self>)>,
    },
}

// ---------------------------------------------------------------------------
// Trait.
// ---------------------------------------------------------------------------

/// Durable store for per-trial `TrialResult` records.
#[async_trait]
pub trait ResultStore: Send + Sync + 'static {
    /// Save one trial result. Idempotent on `result.trial_id`.
    async fn save(&self, result: &TrialResult) -> PersistenceResult<()>;

    /// Load one trial result.
    async fn get(&self, trial: &TrialId) -> PersistenceResult<Option<TrialResult>>;

    /// Delete one trial result.
    async fn delete(&self, trial: &TrialId) -> PersistenceResult<()>;

    /// Page through results matching `filter`.
    async fn query(
        &self,
        filter: &ResultFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<TrialResult>>;

    /// Stream every result matching `filter` — no page bound.
    async fn stream(&self, filter: &ResultFilter) -> PersistenceStream<TrialResult>;

    /// Count records matching `filter`.
    async fn count(&self, filter: &ResultFilter) -> PersistenceResult<u64>;

    /// Apply `agg` to records matching `filter`.
    async fn aggregate(
        &self,
        filter: &ResultFilter,
        agg:    &Aggregation,
    ) -> PersistenceResult<AggregateResult>;
}

// Unit tests for the recursive serde-derived types in this module
// live in `tests/result_store.rs`. Exercising them inline triggers
// pathological recursion-limit growth during monomorphisation.
