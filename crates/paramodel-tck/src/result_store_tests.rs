// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::ResultStore`].

use std::collections::BTreeMap;

use futures::StreamExt;
use jiff::Timestamp;
use paramodel_elements::{ElementName, ParameterName, TrialId, Value};
use paramodel_persistence::{
    Aggregation, GroupDimension, PageRequest, ResultFilter, ResultStore,
};
use paramodel_plan::ElementParameterRef;
use paramodel_trials::{ExecutionTiming, Metrics, TrialResult, TrialStatus};
use ulid::Ulid;

use crate::providers::ResultStoreProvider;

const fn trial_id(n: u64) -> TrialId {
    TrialId::from_ulid(Ulid::from_parts(n, 1))
}

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

fn make_result(id: u64, status: TrialStatus, latency_ms: i64) -> TrialResult {
    let mut inner = BTreeMap::new();
    inner.insert(
        pname("latency_ms"),
        Value::integer(pname("latency_ms"), latency_ms, None),
    );
    let mut outer = BTreeMap::new();
    outer.insert(ename("db"), inner);
    let metrics = Metrics::from_map(outer);
    let builder = TrialResult::builder()
        .trial_id(trial_id(id))
        .status(status)
        .attempt(1)
        .metrics(metrics)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1_700_000_000).unwrap(),
                Timestamp::from_second(1_700_000_001).unwrap(),
            )
            .unwrap(),
        );
    match status {
        TrialStatus::Failed => builder
            .error(
                paramodel_trials::ErrorInfo::builder()
                    .kind("x".to_owned())
                    .message("y".to_owned())
                    .build(),
            )
            .build(),
        TrialStatus::Skipped => builder.skip_reason("upstream".to_owned()).build(),
        _ => builder.build(),
    }
}

/// Save + get round-trips.
pub async fn tck_result_save_and_get<P: ResultStoreProvider>(provider: &P) {
    let store = provider.fresh();
    let r = make_result(1, TrialStatus::Completed, 100);
    store.save(&r).await.unwrap();
    let got = store.get(&r.trial_id).await.unwrap();
    assert_eq!(got, Some(r));
}

/// `query` honours the status filter.
pub async fn tck_result_query_by_status<P: ResultStoreProvider>(provider: &P) {
    let store = provider.fresh();
    store
        .save(&make_result(1, TrialStatus::Completed, 50))
        .await
        .unwrap();
    store
        .save(&make_result(2, TrialStatus::Failed, 150))
        .await
        .unwrap();
    store
        .save(&make_result(3, TrialStatus::Completed, 200))
        .await
        .unwrap();

    let page = store
        .query(
            &ResultFilter::Status {
                status: TrialStatus::Completed,
            },
            PageRequest::first(10),
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|r| r.status == TrialStatus::Completed));
}

/// `count` returns a consistent total against the same filter.
pub async fn tck_result_count<P: ResultStoreProvider>(provider: &P) {
    let store = provider.fresh();
    for i in 1..=4 {
        let status = if i % 2 == 0 {
            TrialStatus::Completed
        } else {
            TrialStatus::Failed
        };
        store.save(&make_result(i, status, 0)).await.unwrap();
    }
    assert_eq!(store.count(&ResultFilter::Any).await.unwrap(), 4);
    assert_eq!(
        store
            .count(&ResultFilter::Status {
                status: TrialStatus::Completed
            })
            .await
            .unwrap(),
        2,
    );
}

/// `stream` emits matching records one at a time.
pub async fn tck_result_stream<P: ResultStoreProvider>(provider: &P) {
    let store = provider.fresh();
    for i in 1..=3 {
        store
            .save(&make_result(i, TrialStatus::Completed, 0))
            .await
            .unwrap();
    }
    let items: Vec<TrialResult> = store
        .stream(&ResultFilter::Any)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(items.len(), 3);
}

/// `aggregate` computes Count, Avg, and group-by correctly.
pub async fn tck_result_aggregate<P: ResultStoreProvider>(provider: &P) {
    use paramodel_persistence::AggregateResult;

    let store = provider.fresh();
    store
        .save(&make_result(1, TrialStatus::Completed, 50))
        .await
        .unwrap();
    store
        .save(&make_result(2, TrialStatus::Completed, 150))
        .await
        .unwrap();
    store
        .save(&make_result(3, TrialStatus::Failed, 250))
        .await
        .unwrap();

    // Count over Any.
    let count = store
        .aggregate(&ResultFilter::Any, &Aggregation::Count)
        .await
        .unwrap();
    match count {
        AggregateResult::Count { n } => assert_eq!(n, 3),
        other => panic!("expected Count, got {other:?}"),
    }

    // Avg of latency_ms.
    let avg = store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Avg {
                metric: ElementParameterRef::new(ename("db"), pname("latency_ms")),
            },
        )
        .await
        .unwrap();
    match avg {
        AggregateResult::Scalar { value } => {
            let v = value.as_double().expect("double");
            assert!((v - 150.0).abs() < 1e-6, "expected avg 150, got {v}");
        }
        other => panic!("expected Scalar, got {other:?}"),
    }

    // GroupBy status → Count.
    let grouped = store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::GroupBy {
                dimension: GroupDimension::Status,
                then:      Box::new(Aggregation::Count),
            },
        )
        .await
        .unwrap();
    match grouped {
        AggregateResult::Grouped { groups } => {
            assert_eq!(groups.len(), 2, "two statuses seen");
        }
        other => panic!("expected Grouped, got {other:?}"),
    }
}
