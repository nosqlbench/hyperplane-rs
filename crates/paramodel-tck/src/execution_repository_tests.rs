// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::ExecutionRepository`].

use std::collections::BTreeMap;
use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{Fingerprint, LabelValue, Labels, attributes::label};
use paramodel_executor::{ExecutionId, ExecutionResults, ExecutionStatus};
use paramodel_persistence::{ExecutionFilter, ExecutionRepository, PageRequest, PlanFilter};
use paramodel_plan::{
    ElementInstanceGraph, ExecutionGraph, ExecutionPlan, ExecutionPlanId,
    ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, TestPlanId,
};
use ulid::Ulid;

use crate::providers::ExecutionRepositoryProvider;

fn fp(tag: &[u8]) -> Fingerprint {
    Fingerprint::of(tag)
}

fn default_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn make_plan(id: u64) -> ExecutionPlan {
    let _ = default_labels();
    ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(id, 1)))
        .source_plan_fingerprint(fp(format!("src-{id}").as_bytes()))
        .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(id, 2)))
        .execution_graph(ExecutionGraph::default())
        .element_instance_graph(ElementInstanceGraph::default())
        .metadata(
            ExecutionPlanMetadata::builder()
                .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                .compilation_duration(Duration::from_secs(1))
                .compiler_version("test-0.1".to_owned())
                .optimization_level(OptimizationStrategy::Basic)
                .trial_count(0)
                .step_count(0)
                .barrier_count(0)
                .element_instance_count(0)
                .performance_metrics(PerformanceMetrics {
                    critical_path_duration: None,
                    total_duration:         None,
                    maximum_parallelism:    1,
                    average_parallelism:    1.0,
                    speedup_factor:         1.0,
                })
                .build(),
        )
        .build()
}

fn make_execution(id: u64, status: ExecutionStatus) -> ExecutionResults {
    let id_i64 = i64::try_from(id).unwrap_or(i64::MAX);
    ExecutionResults {
        execution_id:  ExecutionId::from_ulid(Ulid::from_parts(id, 3)),
        status,
        started_at:    Timestamp::from_second(1_700_000_000 + id_i64).unwrap(),
        completed_at:  Timestamp::from_second(1_700_000_010 + id_i64).unwrap(),
        duration:      Duration::from_secs(10),
        step_stats:    BTreeMap::new(),
        trial_results: Vec::new(),
    }
}

/// `save_plan` + `get_plan` round-trips.
pub async fn tck_execution_save_and_get_plan<P: ExecutionRepositoryProvider>(
    provider: &P,
) {
    let repo = provider.fresh();
    let plan = make_plan(1);
    repo.save_plan(&plan).await.unwrap();
    let got = repo.get_plan(&plan.id).await.unwrap();
    assert_eq!(got.id, plan.id);
}

/// `save_execution` + `get_execution` round-trips.
pub async fn tck_execution_save_and_get_execution<P: ExecutionRepositoryProvider>(
    provider: &P,
) {
    let repo = provider.fresh();
    let results = make_execution(1, ExecutionStatus::Succeeded);
    repo.save_execution(&results).await.unwrap();
    let got = repo.get_execution(&results.execution_id).await.unwrap();
    assert_eq!(got.execution_id, results.execution_id);
}

/// `list_plans` respects the filter.
pub async fn tck_execution_list_plans_filter<P: ExecutionRepositoryProvider>(
    provider: &P,
) {
    let repo = provider.fresh();
    let target_fp = fp(b"src-1");
    repo.save_plan(&make_plan(1)).await.unwrap();
    repo.save_plan(&make_plan(2)).await.unwrap();
    let filter = PlanFilter {
        source_plan_fingerprint: Some(target_fp),
        ..PlanFilter::default()
    };
    let page = repo.list_plans(&filter, PageRequest::first(10)).await.unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].source_plan_fingerprint, target_fp);
}

/// Filtered execution list honours status.
pub async fn tck_execution_filter_status<P: ExecutionRepositoryProvider>(
    provider: &P,
) {
    let repo = provider.fresh();
    repo.save_plan(&make_plan(1)).await.unwrap();
    repo.save_execution(&make_execution(1, ExecutionStatus::Succeeded))
        .await
        .unwrap();
    repo.save_execution(&make_execution(2, ExecutionStatus::PartialFailure))
        .await
        .unwrap();
    let filter = ExecutionFilter {
        status: Some(ExecutionStatus::Succeeded),
        ..ExecutionFilter::default()
    };
    let page = repo
        .list_executions(&filter, PageRequest::first(10))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].final_status, ExecutionStatus::Succeeded);
}
