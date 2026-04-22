// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Semantic-correctness coverage for `ResultFilter` and
//! `Aggregation` — a fixed curated dataset fed through every
//! variant, with expected cardinalities / values asserted against a
//! reference evaluator (arithmetic by hand).

use std::collections::BTreeMap;

use jiff::Timestamp;
use paramodel_elements::{
    ElementName, ParameterName, TrialId, Value,
};
use paramodel_persistence::{
    AggregateResult, Aggregation, Comparison, GroupDimension, PageRequest,
    ResultFilter, ResultStore,
};
use paramodel_plan::ElementParameterRef;
use paramodel_tck::{MockResultStoreProvider, ResultStoreProvider};
use paramodel_trials::{ExecutionTiming, Metrics, TrialResult, TrialStatus};
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

/// Construct a `TrialResult` with a single metric `db.latency_ms`
/// and a status.
fn record(trial_ord: u64, status: TrialStatus, latency: i64, attempt: u32) -> TrialResult {
    let mut inner = BTreeMap::new();
    inner.insert(
        pname("latency_ms"),
        Value::integer(pname("latency_ms"), latency, None),
    );
    let mut outer = BTreeMap::new();
    outer.insert(ename("db"), inner);
    let metrics = Metrics::from_map(outer);
    let ord_i64 = i64::try_from(trial_ord).unwrap_or(i64::MAX);
    let builder = TrialResult::builder()
        .trial_id(TrialId::from_ulid(Ulid::from_parts(trial_ord, 1)))
        .status(status)
        .attempt(attempt)
        .metrics(metrics)
        .timing(
            ExecutionTiming::new(
                Timestamp::from_second(1_700_000_000 + ord_i64).unwrap(),
                Timestamp::from_second(1_700_000_001 + ord_i64).unwrap(),
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

async fn populate() -> paramodel_mock::MockResultStore {
    let store = MockResultStoreProvider.fresh();
    // Six records: 4 Completed with latencies [10, 20, 30, 40],
    // 1 Failed with latency 500, 1 Skipped with latency 999.
    store.save(&record(1, TrialStatus::Completed, 10, 1)).await.unwrap();
    store.save(&record(2, TrialStatus::Completed, 20, 1)).await.unwrap();
    store.save(&record(3, TrialStatus::Completed, 30, 2)).await.unwrap();
    store.save(&record(4, TrialStatus::Completed, 40, 1)).await.unwrap();
    store.save(&record(5, TrialStatus::Failed,    500, 3)).await.unwrap();
    store.save(&record(6, TrialStatus::Skipped,   999, 1)).await.unwrap();
    store
}

// ---------------------------------------------------------------------------
// ResultFilter: every leaf + combinator against the curated dataset.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn filter_any_matches_all() {
    let store = populate().await;
    let n = store.count(&ResultFilter::Any).await.unwrap();
    assert_eq!(n, 6);
}

#[tokio::test]
async fn filter_status_matches_exact() {
    let store = populate().await;
    let n = store
        .count(&ResultFilter::Status {
            status: TrialStatus::Completed,
        })
        .await
        .unwrap();
    assert_eq!(n, 4);
}

#[tokio::test]
async fn filter_status_in_matches_set() {
    let store = populate().await;
    let statuses: std::collections::BTreeSet<TrialStatus> =
        [TrialStatus::Failed, TrialStatus::Skipped].into_iter().collect();
    let n = store
        .count(&ResultFilter::StatusIn { statuses })
        .await
        .unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn filter_started_after_prunes_earlier() {
    let store = populate().await;
    let ts = Timestamp::from_second(1_700_000_003).unwrap(); // keep trials 4,5,6
    let n = store
        .count(&ResultFilter::StartedAfter { ts })
        .await
        .unwrap();
    assert_eq!(n, 3);
}

#[tokio::test]
async fn filter_attempt_number_cmp_variants() {
    let store = populate().await;
    for (cmp, value, want) in [
        (Comparison::Eq, 1, 4),
        (Comparison::Ne, 1, 2),
        (Comparison::Lt, 2, 4),
        (Comparison::Le, 2, 5),
        (Comparison::Gt, 1, 2),
        (Comparison::Ge, 1, 6),
    ] {
        let n = store
            .count(&ResultFilter::AttemptNumber { cmp, value })
            .await
            .unwrap();
        assert_eq!(n, want, "cmp={cmp:?} value={value}");
    }
}

#[tokio::test]
async fn filter_metric_cmp_variants() {
    let store = populate().await;
    let coord = ElementParameterRef::new(ename("db"), pname("latency_ms"));
    // Latencies across all records: [10, 20, 30, 40, 500, 999].
    for (cmp, rhs, want) in [
        (Comparison::Lt, 50i64, 4),  // 10,20,30,40
        (Comparison::Ge, 100i64, 2), // 500,999
        (Comparison::Eq, 500i64, 1),
    ] {
        let n = store
            .count(&ResultFilter::Metric {
                coord: coord.clone(),
                cmp,
                value: Value::integer(pname("latency_ms"), rhs, None),
            })
            .await
            .unwrap();
        assert_eq!(n, want, "cmp={cmp:?} rhs={rhs}");
    }
}

#[tokio::test]
async fn filter_and_is_intersection() {
    let store = populate().await;
    let coord = ElementParameterRef::new(ename("db"), pname("latency_ms"));
    let f = ResultFilter::Status {
        status: TrialStatus::Completed,
    }
    .and(ResultFilter::Metric {
        coord,
        cmp:   Comparison::Le,
        value: Value::integer(pname("latency_ms"), 25, None),
    });
    let n = store.count(&f).await.unwrap();
    assert_eq!(n, 2, "completed AND lat ≤ 25 → trials 1,2");
}

#[tokio::test]
async fn filter_or_is_union() {
    let store = populate().await;
    let f = ResultFilter::Status {
        status: TrialStatus::Failed,
    }
    .or(ResultFilter::Status {
        status: TrialStatus::Skipped,
    });
    let n = store.count(&f).await.unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn filter_not_is_complement() {
    let store = populate().await;
    let f = ResultFilter::Status {
        status: TrialStatus::Completed,
    }
    .negate();
    let n = store.count(&f).await.unwrap();
    assert_eq!(n, 2, "6 total − 4 completed = 2 others");
}

#[tokio::test]
async fn filter_trial_id_matches_single() {
    let store = populate().await;
    let id = TrialId::from_ulid(Ulid::from_parts(3, 1));
    let n = store.count(&ResultFilter::TrialId { id }).await.unwrap();
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------------------
// Aggregation: every operator over the curated dataset.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aggregation_count_over_any_returns_total() {
    let store = populate().await;
    let r = store
        .aggregate(&ResultFilter::Any, &Aggregation::Count)
        .await
        .unwrap();
    match r {
        AggregateResult::Count { n } => assert_eq!(n, 6),
        other => panic!("expected Count, got {other:?}"),
    }
}

#[tokio::test]
async fn aggregation_min_max_sum_avg_match_hand_computed() {
    let store = populate().await;
    let metric = ElementParameterRef::new(ename("db"), pname("latency_ms"));

    // Min across all records = 10.
    match store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Min {
                metric: metric.clone(),
            },
        )
        .await
        .unwrap()
    {
        AggregateResult::Scalar { value } => {
            assert!((value.as_double().unwrap() - 10.0).abs() < 1e-6);
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
    // Max = 999.
    match store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Max {
                metric: metric.clone(),
            },
        )
        .await
        .unwrap()
    {
        AggregateResult::Scalar { value } => {
            assert!((value.as_double().unwrap() - 999.0).abs() < 1e-6);
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
    // Sum = 10+20+30+40+500+999 = 1599.
    match store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Sum {
                metric: metric.clone(),
            },
        )
        .await
        .unwrap()
    {
        AggregateResult::Scalar { value } => {
            assert!((value.as_double().unwrap() - 1599.0).abs() < 1e-6);
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
    // Avg over Completed only (10,20,30,40) = 25.
    match store
        .aggregate(
            &ResultFilter::Status {
                status: TrialStatus::Completed,
            },
            &Aggregation::Avg {
                metric: metric.clone(),
            },
        )
        .await
        .unwrap()
    {
        AggregateResult::Scalar { value } => {
            assert!((value.as_double().unwrap() - 25.0).abs() < 1e-6);
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
}

#[tokio::test]
async fn aggregation_percentile_is_nearest_rank() {
    let store = populate().await;
    let metric = ElementParameterRef::new(ename("db"), pname("latency_ms"));
    // Sorted latencies: [10, 20, 30, 40, 500, 999]. p=0.5 → index 2 → 30.
    match store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::Percentile {
                metric: metric.clone(),
                p:      0.5,
            },
        )
        .await
        .unwrap()
    {
        AggregateResult::Scalar { value } => {
            let got = value.as_double().unwrap();
            assert!((got - 30.0).abs() < 1e-6, "want 30, got {got}");
        }
        other => panic!("expected Scalar, got {other:?}"),
    }
}

#[tokio::test]
async fn aggregation_group_by_status_returns_three_groups() {
    let store = populate().await;
    let r = store
        .aggregate(
            &ResultFilter::Any,
            &Aggregation::GroupBy {
                dimension: GroupDimension::Status,
                then:      Box::new(Aggregation::Count),
            },
        )
        .await
        .unwrap();
    match r {
        AggregateResult::Grouped { groups } => {
            // Completed, Failed, Skipped → 3 groups.
            assert_eq!(groups.len(), 3);
            // Sum of all group counts must equal total.
            let sum: u64 = groups
                .iter()
                .map(|(_k, v)| match v.as_ref() {
                    AggregateResult::Count { n } => *n,
                    _ => 0,
                })
                .sum();
            assert_eq!(sum, 6);
        }
        other => panic!("expected Grouped, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Query pagination — page_size honoured.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn query_respects_page_size() {
    let store = populate().await;
    let page = store
        .query(&ResultFilter::Any, PageRequest::first(3))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
}
