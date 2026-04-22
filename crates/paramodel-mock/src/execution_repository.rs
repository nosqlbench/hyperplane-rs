// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockExecutionRepository` — in-memory implementation of
//! [`paramodel_persistence::ExecutionRepository`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use paramodel_elements::Fingerprint;
use paramodel_executor::{ExecutionId, ExecutionResults};
use paramodel_persistence::{
    ExecutionFilter, ExecutionPlanSummary, ExecutionRepository, ExecutionSummary,
    Page, PageRequest, PersistenceError, PersistenceResult, PlanFilter,
};
use paramodel_plan::{ExecutionPlan, ExecutionPlanId};

/// In-memory execution repository.
#[derive(Debug, Default)]
pub struct MockExecutionRepository {
    state: Mutex<InnerState>,
}

#[derive(Debug, Default)]
struct InnerState {
    plans:      BTreeMap<ExecutionPlanId, ExecutionPlan>,
    executions: BTreeMap<ExecutionId, ExecutionResults>,
}

impl MockExecutionRepository {
    /// Construct an empty repository.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn plan_summary(plan: &ExecutionPlan) -> ExecutionPlanSummary {
        ExecutionPlanSummary {
            id:                      plan.id,
            source_plan_fingerprint: plan.source_plan_fingerprint,
            compiled_at:             plan.metadata.compiled_at,
            trial_count:             plan.metadata.trial_count,
            step_count:              plan.metadata.step_count,
            labels:                  plan.labels.clone(),
            tags:                    plan.tags.clone(),
        }
    }

    fn execution_summary(
        results: &ExecutionResults,
        plan_fp: Fingerprint,
    ) -> ExecutionSummary {
        ExecutionSummary {
            id:                results.execution_id,
            plan_fingerprint:  plan_fp,
            final_status:      results.status,
            started_at:        results.started_at,
            completed_at:      Some(results.completed_at),
            successful_trials: u32::try_from(
                results
                    .trial_results
                    .iter()
                    .filter(|r| r.status.is_success())
                    .count(),
            )
            .unwrap_or(u32::MAX),
            failed_trials:     u32::try_from(
                results
                    .trial_results
                    .iter()
                    .filter(|r| r.status.is_failure())
                    .count(),
            )
            .unwrap_or(u32::MAX),
            skipped_trials:    u32::try_from(
                results
                    .trial_results
                    .iter()
                    .filter(|r| matches!(r.status, paramodel_trials::TrialStatus::Skipped))
                    .count(),
            )
            .unwrap_or(u32::MAX),
            total_trials:      u32::try_from(results.trial_results.len())
                .unwrap_or(u32::MAX),
        }
    }

    fn plan_matches(plan: &ExecutionPlan, filter: &PlanFilter) -> bool {
        if let Some(fp) = filter.source_plan_fingerprint
            && plan.source_plan_fingerprint != fp
        {
            return false;
        }
        if let Some(before) = filter.compiled_before
            && plan.metadata.compiled_at >= before
        {
            return false;
        }
        if let Some(after) = filter.compiled_after
            && plan.metadata.compiled_at <= after
        {
            return false;
        }
        for (k, v) in &filter.has_label {
            if plan.labels.get(k) != Some(v) {
                return false;
            }
        }
        for (k, v) in &filter.has_tag {
            if plan.tags.get(k) != Some(v) {
                return false;
            }
        }
        // name_contains is matched against the plan's labels — we
        // have no raw "name" here; left as a no-op when None.
        true
    }

    fn execution_matches(
        results:   &ExecutionResults,
        plan_fp:   Fingerprint,
        filter:    &ExecutionFilter,
    ) -> bool {
        if let Some(fp) = filter.plan_fingerprint
            && plan_fp != fp
        {
            return false;
        }
        if let Some(status) = filter.status
            && results.status != status
        {
            return false;
        }
        if let Some(after) = filter.started_after
            && results.started_at <= after
        {
            return false;
        }
        if let Some(before) = filter.started_before
            && results.started_at >= before
        {
            return false;
        }
        // Execution-level labels/tags aren't modeled on ExecutionResults;
        // the filter fields are accepted but match nothing.
        if !filter.has_label.is_empty() || !filter.has_tag.is_empty() {
            return false;
        }
        true
    }

    fn lookup_plan_fp(
        plans: &BTreeMap<ExecutionPlanId, ExecutionPlan>,
        results: &ExecutionResults,
    ) -> Fingerprint {
        // The `ExecutionResults` records execution_id, not plan id.
        // Reverse the mapping heuristically — pick any stored plan
        // whose fingerprint matches some stored association.
        //
        // Without a stored plan↔execution mapping we fall back to
        // the first plan's fingerprint. Adopters wanting accurate
        // filtering should pair executions with their compiled plan
        // when saving.
        let _ = results;
        plans
            .values()
            .next()
            .map_or_else(|| Fingerprint::of(b""), ExecutionPlan::fingerprint)
    }
}

#[async_trait]
impl ExecutionRepository for MockExecutionRepository {
    async fn save_plan(&self, plan: &ExecutionPlan) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .plans
            .insert(plan.id, plan.clone());
        Ok(())
    }

    async fn get_plan(
        &self,
        id: &ExecutionPlanId,
    ) -> PersistenceResult<ExecutionPlan> {
        self.state
            .lock()
            .expect("poisoned")
            .plans
            .get(id)
            .cloned()
            .ok_or_else(|| PersistenceError::not_found("execution_plan", id.to_string()))
    }

    async fn get_plan_by_fingerprint(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<ExecutionPlan>> {
        Ok(self
            .state
            .lock()
            .expect("poisoned")
            .plans
            .values()
            .find(|p| p.fingerprint() == *fp)
            .cloned())
    }

    async fn list_plans(
        &self,
        filter: &PlanFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionPlanSummary>> {
        let page = page.clamped();
        let state = self.state.lock().expect("poisoned");
        let mut items: Vec<ExecutionPlanSummary> = state
            .plans
            .values()
            .filter(|p| Self::plan_matches(p, filter))
            .map(Self::plan_summary)
            .collect();
        items.sort_by_key(|s| std::cmp::Reverse(s.compiled_at));
        Ok(Page::terminal(items.into_iter().take(page.size as usize).collect()))
    }

    async fn save_execution(
        &self,
        results: &ExecutionResults,
    ) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .executions
            .insert(results.execution_id, results.clone());
        Ok(())
    }

    async fn get_execution(
        &self,
        id: &ExecutionId,
    ) -> PersistenceResult<ExecutionResults> {
        self.state
            .lock()
            .expect("poisoned")
            .executions
            .get(id)
            .cloned()
            .ok_or_else(|| PersistenceError::not_found("execution", id.to_string()))
    }

    async fn list_executions(
        &self,
        filter: &ExecutionFilter,
        page:   PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>> {
        let page = page.clamped();
        let state = self.state.lock().expect("poisoned");
        let mut items: Vec<ExecutionSummary> = state
            .executions
            .values()
            .filter_map(|r| {
                let fp = Self::lookup_plan_fp(&state.plans, r);
                if Self::execution_matches(r, fp, filter) {
                    Some(Self::execution_summary(r, fp))
                } else {
                    None
                }
            })
            .collect();
        items.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(Page::terminal(items.into_iter().take(page.size as usize).collect()))
    }

    async fn executions_of_plan(
        &self,
        plan: &ExecutionPlanId,
        page: PageRequest,
    ) -> PersistenceResult<Page<ExecutionSummary>> {
        let page = page.clamped();
        let state = self.state.lock().expect("poisoned");
        let Some(p) = state.plans.get(plan) else {
            return Ok(Page::empty());
        };
        let fp = p.fingerprint();
        let mut items: Vec<ExecutionSummary> = state
            .executions
            .values()
            .map(|r| Self::execution_summary(r, fp))
            .collect();
        items.sort_by_key(|s| std::cmp::Reverse(s.started_at));
        Ok(Page::terminal(items.into_iter().take(page.size as usize).collect()))
    }

    async fn latest_execution_of_plan(
        &self,
        plan: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionSummary>> {
        let state = self.state.lock().expect("poisoned");
        let Some(p) = state.plans.get(plan) else {
            return Ok(None);
        };
        let fp = p.fingerprint();
        Ok(state
            .executions
            .values()
            .max_by_key(|r| r.started_at)
            .map(|r| Self::execution_summary(r, fp)))
    }
}
