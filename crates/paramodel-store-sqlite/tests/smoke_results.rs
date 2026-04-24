// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Smoke tests for `SqliteStore::ResultStore`.
//!
//! Covers: save/get round-trip, delete, query with several filter
//! kinds (SQL-eligible + Rust-only), count, and a scalar aggregation.

use std::collections::BTreeMap;

use jiff::Timestamp;
use paramodel_elements::{ElementName, ParameterName, TrialId, Value};
use paramodel_persistence::{
    AggregateResult, Aggregation, Comparison, PageRequest, ResultFilter, ResultStore,
};
use paramodel_plan::ElementParameterRef;
use paramodel_store_sqlite::SqliteStore;
use paramodel_trials::{
    ExecutionTiming, Metrics, TrialResult, TrialStatus,
};
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn build_result(
    n: u64,
    status: TrialStatus,
    attempt: u32,
    latency: i64,
    started_secs: i64,
) -> TrialResult {
    let mut outer = BTreeMap::new();
    let mut inner = BTreeMap::new();
    inner.insert(pname("latency"), Value::integer(pname("latency"), latency, None));
    outer.insert(ename("db"), inner);
    TrialResult::builder()
        .trial_id(TrialId::from_ulid(Ulid::from_parts(n, 1)))
        .status(status)
        .attempt(attempt)
        .metrics(Metrics::from_map(outer))
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(started_secs).unwrap(),
                Timestamp::from_second(started_secs + 1).unwrap(),
            )
            .unwrap(),
        )
        .build()
}

#[tokio::test]
async fn save_get_delete_roundtrip() {
    let store = SqliteStore::in_memory().await.unwrap();
    let r = build_result(1, TrialStatus::Completed, 1, 100, 1_700_000_000);
    store.save(&r).await.unwrap();
    let got = store.get(&r.trial_id).await.unwrap().unwrap();
    assert_eq!(got.trial_id, r.trial_id);
    assert_eq!(got.status,   TrialStatus::Completed);
    store.delete(&r.trial_id).await.unwrap();
    assert!(store.get(&r.trial_id).await.unwrap().is_none());
}

#[tokio::test]
async fn query_filter_by_status() {
    let store = SqliteStore::in_memory().await.unwrap();
    for (n, status, lat) in [
        (1, TrialStatus::Completed, 100),
        (2, TrialStatus::Failed,    200),
        (3, TrialStatus::Completed, 150),
    ] {
        store.save(&build_result(n, status, 1, lat, 1_700_000_000 + n as i64))
            .await.unwrap();
    }
    let page = store
        .query(
            &ResultFilter::Status { status: TrialStatus::Completed },
            PageRequest::first(10),
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|r| r.status == TrialStatus::Completed));
}

#[tokio::test]
async fn query_filter_by_metric_post_fetch() {
    let store = SqliteStore::in_memory().await.unwrap();
    for (n, lat) in [(1, 50), (2, 150), (3, 250)] {
        store.save(&build_result(n, TrialStatus::Completed, 1, lat, 1_700_000_000 + n as i64))
            .await.unwrap();
    }
    // `Metric { latency > 100 }` is a Rust-side filter.
    let filter = ResultFilter::Metric {
        coord: ElementParameterRef::new(ename("db"), pname("latency")),
        cmp:   Comparison::Gt,
        value: Value::integer(pname("latency"), 100, None),
    };
    let page = store.query(&filter, PageRequest::first(10)).await.unwrap();
    assert_eq!(page.items.len(), 2);
    let lats: Vec<i64> = page
        .items
        .iter()
        .map(|r| r.metrics.get(&ename("db"), &pname("latency")).unwrap().as_integer().unwrap())
        .collect();
    assert!(lats.iter().all(|&l| l > 100));
}

#[tokio::test]
async fn count_with_and_combinator() {
    let store = SqliteStore::in_memory().await.unwrap();
    for (n, status, lat) in [
        (1, TrialStatus::Completed, 100),
        (2, TrialStatus::Completed, 200),
        (3, TrialStatus::Failed,    300),
        (4, TrialStatus::Completed, 400),
    ] {
        store.save(&build_result(n, status, 1, lat, 1_700_000_000 + n as i64))
            .await.unwrap();
    }
    // SQL-eligible AND — combines cleanly.
    let filter = ResultFilter::Status { status: TrialStatus::Completed }
        .and(ResultFilter::AttemptNumber { cmp: Comparison::Eq, value: 1 });
    let n = store.count(&filter).await.unwrap();
    assert_eq!(n, 3);
}

/// Verify the metric-SQL-pushdown path: `ResultStore::count` with a
/// pure-Metric filter should resolve via SQL (we prove this by
/// checking the `fully_sql_filter` path via correctness — same
/// answer as the Rust fallback).
#[tokio::test]
async fn metric_sql_pushdown_count_matches_rust_fallback() {
    let store = SqliteStore::in_memory().await.unwrap();
    for (n, lat) in [(1, 50), (2, 100), (3, 150), (4, 200), (5, 250)] {
        store.save(&build_result(n, TrialStatus::Completed, 1, lat, 1_700_000_000 + n as i64))
            .await.unwrap();
    }
    let filter = ResultFilter::Metric {
        coord: ElementParameterRef::new(ename("db"), pname("latency")),
        cmp:   Comparison::Ge,
        value: Value::integer(pname("latency"), 150, None),
    };
    // Count goes through the SQL fast-path because Metric + numeric
    // is fully_sql_filter-compatible.
    let n = store.count(&filter).await.unwrap();
    assert_eq!(n, 3, "three rows with latency >= 150");

    // Query returns the same set.
    let page = store
        .query(&filter, PageRequest::first(10))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
}

#[tokio::test]
async fn aggregate_avg_metric() {
    let store = SqliteStore::in_memory().await.unwrap();
    for (n, lat) in [(1, 100), (2, 200), (3, 300)] {
        store.save(&build_result(n, TrialStatus::Completed, 1, lat, 1_700_000_000 + n as i64))
            .await.unwrap();
    }
    let result = store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Avg {
                metric: ElementParameterRef::new(ename("db"), pname("latency")),
            },
        )
        .await
        .unwrap();
    match result {
        AggregateResult::Scalar { value } => {
            let v = value.as_double().unwrap();
            assert!((v - 200.0).abs() < 1e-6, "want 200, got {v}");
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
}
