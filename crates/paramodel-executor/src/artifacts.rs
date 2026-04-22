// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ArtifactCollector` trait + supporting types.
//!
//! V0.1 ships `InMemoryArtifactCollector` — a reference impl that
//! keeps collected items in a `Mutex<BTreeMap>` for tests and small
//! runs. Real backends live in the persistence tier.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use paramodel_elements::{ElementName, ParameterName, TrialId, Value};
use paramodel_trials::ArtifactRef;
use serde::{Deserialize, Serialize};

use crate::error::ArtifactError;

// ---------------------------------------------------------------------------
// Content, kind, and retention types.
// ---------------------------------------------------------------------------

/// Broad categorisation of a collected artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// `stdout` capture.
    Stdout,
    /// `stderr` capture.
    Stderr,
    /// General log.
    Log,
    /// Metric dump.
    Metric,
    /// Trace (e.g. distributed tracing spans).
    Trace,
    /// CPU/memory profile.
    Profile,
    /// Screenshot.
    Screenshot,
    /// Video recording.
    Video,
    /// Generated report.
    Report,
    /// Structured result payload.
    Result,
    /// Stack trace on failure.
    StackTrace,
    /// Core dump.
    CoreDump,
    /// Memory dump.
    MemoryDump,
    /// Network trace.
    NetworkTrace,
    /// CPU-only profile.
    CpuProfile,
    /// Memory-only profile.
    MemoryProfile,
    /// Adopter-defined.
    Custom,
}

/// How an artifact's payload is provided to the collector.
#[derive(Debug, Clone)]
pub enum ArtifactContent {
    /// Bytes embedded directly.
    Inline(Vec<u8>),
    /// Already-stored content reachable at a URI.
    Uri(String),
}

/// Group of collected artifacts for one trial.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactCollection {
    /// Artifacts indexed by `(element, name)`.
    pub artifacts: Vec<ArtifactRef>,
}

/// Retention budget for stored artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Maximum age before cleanup may remove an artifact.
    pub max_age: Option<Duration>,
    /// Maximum count before cleanup may remove the oldest.
    pub max_count: Option<u32>,
    /// Maximum total bytes across kept artifacts.
    pub max_bytes: Option<u64>,
}

/// Summary returned by [`ArtifactCollector::cleanup`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Count of artifacts removed.
    pub removed_count: u32,
    /// Bytes freed.
    pub removed_bytes: u64,
}

// ---------------------------------------------------------------------------
// ArtifactCollector trait.
// ---------------------------------------------------------------------------

/// Collect typed metrics and opaque artifacts from an executing
/// trial.
#[async_trait]
pub trait ArtifactCollector: Send + Sync + 'static {
    /// Begin a collection session for `trial`.
    async fn start_collection(&self, trial: &TrialId);

    /// Finalise the session; returns everything collected.
    async fn stop_collection(
        &self,
        trial: &TrialId,
    ) -> Result<ArtifactCollection, ArtifactError>;

    /// Collect an opaque artifact (log, trace, video, …).
    async fn collect_artifact(
        &self,
        trial:   &TrialId,
        element: &ElementName,
        name:    &str,
        content: ArtifactContent,
        kind:    ArtifactKind,
    );

    /// Collect a typed-metrics payload from a `SaveOutput` step.
    async fn collect_metrics(
        &self,
        trial:   &TrialId,
        element: &ElementName,
        metrics: &BTreeMap<ParameterName, Value>,
    );

    /// List artifacts attached to a trial.
    async fn artifacts(&self, trial: &TrialId) -> Vec<ArtifactRef>;

    /// Apply (or replace) the retention policy.
    async fn set_retention_policy(&self, policy: RetentionPolicy);

    /// Run cleanup per the active retention policy.
    async fn cleanup(&self) -> CleanupReport;
}

// ---------------------------------------------------------------------------
// InMemoryArtifactCollector — reference impl for tests.
// ---------------------------------------------------------------------------

/// Tiny in-memory reference collector.
#[derive(Debug, Default)]
pub struct InMemoryArtifactCollector {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    open_trials: BTreeMap<TrialId, ArtifactCollection>,
    finished:    BTreeMap<TrialId, ArtifactCollection>,
    metrics:     BTreeMap<TrialId, BTreeMap<(ElementName, ParameterName), Value>>,
    policy:      Option<RetentionPolicy>,
}

impl InMemoryArtifactCollector {
    /// Construct.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ArtifactCollector for InMemoryArtifactCollector {
    async fn start_collection(&self, trial: &TrialId) {
        let mut s = self.state.lock().expect("poisoned");
        s.open_trials.entry(*trial).or_default();
    }

    async fn stop_collection(
        &self,
        trial: &TrialId,
    ) -> Result<ArtifactCollection, ArtifactError> {
        let mut s = self.state.lock().expect("poisoned");
        let collected = s
            .open_trials
            .remove(trial)
            .ok_or_else(|| ArtifactError::NoCollection {
                trial: format!("{trial}"),
            })?;
        s.finished.insert(*trial, collected.clone());
        Ok(collected)
    }

    async fn collect_artifact(
        &self,
        trial:   &TrialId,
        element: &ElementName,
        name:    &str,
        content: ArtifactContent,
        kind:    ArtifactKind,
    ) {
        let (uri, size) = match content {
            ArtifactContent::Inline(bytes) => (
                format!("inline://{element}/{name}"),
                Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
            ),
            ArtifactContent::Uri(u) => (u, None),
        };
        let aref = ArtifactRef::builder()
            .element(element.clone())
            .name(name.to_owned())
            .uri(uri)
            .content_type(content_type_for(kind).to_owned())
            .maybe_size_bytes(size)
            .build();
        let mut s = self.state.lock().expect("poisoned");
        s.open_trials
            .entry(*trial)
            .or_default()
            .artifacts
            .push(aref);
    }

    async fn collect_metrics(
        &self,
        trial:   &TrialId,
        element: &ElementName,
        metrics: &BTreeMap<ParameterName, Value>,
    ) {
        let mut s = self.state.lock().expect("poisoned");
        let entry = s.metrics.entry(*trial).or_default();
        for (k, v) in metrics {
            entry.insert((element.clone(), k.clone()), v.clone());
        }
    }

    async fn artifacts(&self, trial: &TrialId) -> Vec<ArtifactRef> {
        let s = self.state.lock().expect("poisoned");
        s.open_trials
            .get(trial)
            .or_else(|| s.finished.get(trial))
            .map(|c| c.artifacts.clone())
            .unwrap_or_default()
    }

    async fn set_retention_policy(&self, policy: RetentionPolicy) {
        self.state.lock().expect("poisoned").policy = Some(policy);
    }

    async fn cleanup(&self) -> CleanupReport {
        CleanupReport::default()
    }
}

const fn content_type_for(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Stdout
        | ArtifactKind::Stderr
        | ArtifactKind::Log
        | ArtifactKind::StackTrace => "text/plain",
        ArtifactKind::Metric | ArtifactKind::Result => "application/json",
        ArtifactKind::Trace | ArtifactKind::NetworkTrace => "application/trace",
        ArtifactKind::Profile
        | ArtifactKind::CpuProfile
        | ArtifactKind::MemoryProfile => "application/profile",
        ArtifactKind::Screenshot => "image/png",
        ArtifactKind::Video => "video/mp4",
        ArtifactKind::Report => "text/markdown",
        ArtifactKind::CoreDump
        | ArtifactKind::MemoryDump
        | ArtifactKind::Custom => "application/octet-stream",
    }
}

fn content_type_for_runtime(kind: ArtifactKind) -> String {
    content_type_for(kind).to_owned()
}

impl ArtifactContent {
    /// Convenience: inline-bytes wrapper.
    #[must_use]
    pub fn inline(bytes: impl Into<Vec<u8>>) -> Self {
        Self::Inline(bytes.into())
    }

    /// Convenience: URI wrapper.
    #[must_use]
    pub fn uri(u: impl Into<String>) -> Self {
        Self::Uri(u.into())
    }
}

// Allow unused helper in case the explicit per-kind table ever needs
// to be dynamic.
#[allow(dead_code)]
const _: fn(ArtifactKind) -> String = content_type_for_runtime;

#[cfg(test)]
mod tests {
    use paramodel_elements::{ElementName, TrialId};
    use ulid::Ulid;

    use super::*;

    #[tokio::test]
    async fn collect_then_stop_returns_artifacts() {
        let c = InMemoryArtifactCollector::new();
        let tid = TrialId::from_ulid(Ulid::from_parts(1, 1));
        c.start_collection(&tid).await;
        c.collect_artifact(
            &tid,
            &ElementName::new("db").unwrap(),
            "log",
            ArtifactContent::inline(b"hello".to_vec()),
            ArtifactKind::Log,
        )
        .await;
        let coll = c.stop_collection(&tid).await.unwrap();
        assert_eq!(coll.artifacts.len(), 1);
    }
}
