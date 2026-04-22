// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::ArtifactCollector`].

use paramodel_elements::{ElementName, TrialId};
use paramodel_executor::{ArtifactCollector, ArtifactContent, ArtifactKind};
use ulid::Ulid;

use crate::providers::ArtifactCollectorProvider;

fn tid() -> TrialId {
    TrialId::from_ulid(Ulid::new())
}

fn ename() -> ElementName {
    ElementName::new("db").unwrap()
}

/// `start_collection` then `collect_artifact` then `stop_collection`
/// — the stored payload appears in the returned `ArtifactCollection`.
pub async fn tck_collector_collect_then_stop<P: ArtifactCollectorProvider>(
    provider: &P,
) {
    let c = provider.fresh();
    let t = tid();
    c.start_collection(&t).await;
    c.collect_artifact(
        &t,
        &ename(),
        "log",
        ArtifactContent::inline(b"hello".to_vec()),
        ArtifactKind::Log,
    )
    .await;
    let coll = c.stop_collection(&t).await.expect("stop ok");
    assert_eq!(coll.artifacts.len(), 1);
    assert_eq!(coll.artifacts[0].name, "log");
    assert_eq!(coll.artifacts[0].element.as_str(), "db");
}

/// `stop_collection` for a trial that was never started errors
/// with `ArtifactError::NoCollection`.
pub async fn tck_collector_stop_without_start_errors<P>(provider: &P)
where
    P: ArtifactCollectorProvider,
{
    use paramodel_executor::ArtifactError;
    let c = provider.fresh();
    let err = c.stop_collection(&tid()).await.unwrap_err();
    assert!(matches!(err, ArtifactError::NoCollection { .. }));
}

/// `artifacts(trial)` returns the list for both open and already-
/// stopped collections.
pub async fn tck_collector_artifacts_accessor<P>(provider: &P)
where
    P: ArtifactCollectorProvider,
{
    let c = provider.fresh();
    let t = tid();
    c.start_collection(&t).await;
    c.collect_artifact(
        &t,
        &ename(),
        "trace",
        ArtifactContent::inline(b"x".to_vec()),
        ArtifactKind::Trace,
    )
    .await;
    assert_eq!(c.artifacts(&t).await.len(), 1);
    c.stop_collection(&t).await.expect("stop");
    assert_eq!(c.artifacts(&t).await.len(), 1, "stored still readable");
}

/// Uninteresting retention policy — `cleanup` returns a valid
/// report (default values are fine).
pub async fn tck_collector_cleanup_returns_report<P>(provider: &P)
where
    P: ArtifactCollectorProvider,
{
    use paramodel_executor::RetentionPolicy;
    let c = provider.fresh();
    let policy = RetentionPolicy {
        max_age:   None,
        max_count: Some(10),
        max_bytes: Some(1024 * 1024),
    };
    c.set_retention_policy(policy).await;
    let _ = c.cleanup().await;
}
