// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionRepository` trait + supporting types (SRD-0012).
//!
//! Stores both compiled `ExecutionPlan`s and their aggregate
//! `ExecutionResults`. Filter shapes are narrow serde-able structs;
//! browsing returns `ExecutionPlanSummary` / `ExecutionSummary` so
//! the heavy plan + result bodies are only loaded on demand.

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::{Fingerprint, LabelKey, LabelValue, Labels, TagKey, TagValue, Tags};
use paramodel_executor::{ExecutionId, ExecutionResults, ExecutionStatus};
use paramodel_plan::{ExecutionPlan, ExecutionPlanId};
use serde::{Deserialize, Serialize};

use crate::error::PersistenceResult;
use crate::page::{Page, PageRequest};

// ---------------------------------------------------------------------------
// Filters.
// ---------------------------------------------------------------------------

/// Filter for `list_plans`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFilter {
    /// Match the source `TestPlan` fingerprint exactly.
    pub source_plan_fingerprint: Option<Fingerprint>,
    /// Substring match against plan name / labels.
    pub name_contains:           Option<String>,
    /// Compiled-at lower bound.
    pub compiled_after:          Option<Timestamp>,
    /// Compiled-at upper bound.
    pub compiled_before:         Option<Timestamp>,
    /// Every entry must match a label on the plan.
    pub has_label:               Vec<(LabelKey, LabelValue)>,
    /// Every entry must match a tag on the plan.
    pub has_tag:                 Vec<(TagKey, TagValue)>,
}

/// Filter for `list_executions`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionFilter {
    /// Match the compiled plan's fingerprint exactly.
    pub plan_fingerprint: Option<Fingerprint>,
    /// Match the final execution status.
    pub status:           Option<ExecutionStatus>,
    /// Started-at lower bound.
    pub started_after:    Option<Timestamp>,
    /// Started-at upper bound.
    pub started_before:   Option<Timestamp>,
    /// Every entry must match a label on the execution record.
    pub has_label:        Vec<(LabelKey, LabelValue)>,
    /// Every entry must match a tag on the execution record.
    pub has_tag:          Vec<(TagKey, TagValue)>,
}

// ---------------------------------------------------------------------------
// Summary records.
// ---------------------------------------------------------------------------

/// Lightweight browsing record for a compiled plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPlanSummary {
    /// Plan id.
    pub id:                      ExecutionPlanId,
    /// Source `TestPlan` fingerprint.
    pub source_plan_fingerprint: Fingerprint,
    /// When the compiler produced the plan.
    pub compiled_at:             Timestamp,
    /// Trial count.
    pub trial_count:             u32,
    /// Step count.
    pub step_count:              u32,
    /// Intrinsic facts.
    pub labels:                  Labels,
    /// Organisational tags.
    pub tags:                    Tags,
}

/// Lightweight browsing record for one execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionSummary {
    /// Execution id.
    pub id:                ExecutionId,
    /// Compiled plan fingerprint.
    pub plan_fingerprint:  Fingerprint,
    /// Final execution status.
    pub final_status:      ExecutionStatus,
    /// When the run started.
    pub started_at:        Timestamp,
    /// When the run finished; `None` if still in flight / aborted
    /// before completion.
    pub completed_at:      Option<Timestamp>,
    /// Trials that terminated `Completed`.
    pub successful_trials: u32,
    /// Trials that terminated `Failed`.
    pub failed_trials:     u32,
    /// Trials that terminated `Skipped`.
    pub skipped_trials:    u32,
    /// Total trials the plan enumerated.
    pub total_trials:      u32,
}

// ---------------------------------------------------------------------------
// Trait.
// ---------------------------------------------------------------------------

/// Durable store for compiled plans and execution aggregates.
#[async_trait]
pub trait ExecutionRepository: Send + Sync + 'static {
    // ---- execution plans --------------------------------------------------

    /// Save a compiled plan. Idempotent on `plan.id`; a repeat save
    /// of an identical payload is a no-op. A repeat save of a
    /// different payload under the same id is a `Conflict`.
    async fn save_plan(&self, plan: &ExecutionPlan) -> PersistenceResult<()>;

    /// Load a plan by id.
    async fn get_plan(&self, id: &ExecutionPlanId) -> PersistenceResult<ExecutionPlan>;

    /// Load a plan by its execution-plan fingerprint.
    async fn get_plan_by_fingerprint(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<ExecutionPlan>>;

    /// Browse plans matching a filter.
    async fn list_plans(
        &self,
        filter: &PlanFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionPlanSummary>>;

    // ---- execution results -----------------------------------------------

    /// Save an execution aggregate. Idempotent on `results.execution_id`.
    async fn save_execution(&self, results: &ExecutionResults) -> PersistenceResult<()>;

    /// Load an execution aggregate by id.
    async fn get_execution(
        &self,
        id: &ExecutionId,
    ) -> PersistenceResult<ExecutionResults>;

    /// Browse executions matching a filter.
    async fn list_executions(
        &self,
        filter: &ExecutionFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>>;

    /// Browse executions of a given plan.
    async fn executions_of_plan(
        &self,
        plan: &ExecutionPlanId,
        page: PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>>;

    /// Most recent execution of a plan, or `None` if none exist.
    async fn latest_execution_of_plan(
        &self,
        plan: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionSummary>>;
}
