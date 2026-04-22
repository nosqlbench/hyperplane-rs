// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MetadataStore` trait + `MetadataScope` (SRD-0012).
//!
//! Catalog layer: stores `TestPlanMetadata` and `ExecutionPlanMetadata`
//! plus post-hoc label / tag updates scoped via `MetadataScope`. Heavy
//! plan / result bodies live in their dedicated stores.

use async_trait::async_trait;
use paramodel_elements::{Fingerprint, Labels, Tags, TrialId};
use paramodel_executor::ExecutionId;
use paramodel_plan::{ExecutionPlanId, ExecutionPlanMetadata, TestPlanMetadata};
use serde::{Deserialize, Serialize};

use crate::artifact_store::ArtifactId;
use crate::error::PersistenceResult;
use crate::page::{Page, PageRequest};

/// What a `set_labels` / `set_tags` call targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetadataScope {
    /// An authored `TestPlan` addressed by fingerprint.
    TestPlan {
        /// Source plan fingerprint.
        fingerprint: Fingerprint,
    },
    /// A compiled `ExecutionPlan`.
    ExecutionPlan {
        /// Compiled plan id.
        id: ExecutionPlanId,
    },
    /// One execution of a plan.
    Execution {
        /// Execution id.
        id: ExecutionId,
    },
    /// One trial.
    Trial {
        /// Trial id.
        id: TrialId,
    },
    /// One stored artifact.
    Artifact {
        /// Artifact id.
        id: ArtifactId,
    },
}

/// Durable store for lightweight catalog metadata.
#[async_trait]
pub trait MetadataStore: Send + Sync + 'static {
    // ---- TestPlanMetadata ------------------------------------------------

    /// Save `TestPlanMetadata` keyed by the test plan's fingerprint.
    /// Idempotent.
    async fn save_test_plan_metadata(
        &self,
        fp:   &Fingerprint,
        meta: &TestPlanMetadata,
    ) -> PersistenceResult<()>;

    /// Load `TestPlanMetadata` by fingerprint.
    async fn get_test_plan_metadata(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<TestPlanMetadata>>;

    /// Browse known test plans.
    async fn list_test_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(Fingerprint, TestPlanMetadata)>>;

    // ---- ExecutionPlanMetadata -------------------------------------------

    /// Save `ExecutionPlanMetadata` keyed by plan id. Idempotent.
    async fn save_execution_plan_metadata(
        &self,
        id:   &ExecutionPlanId,
        meta: &ExecutionPlanMetadata,
    ) -> PersistenceResult<()>;

    /// Load `ExecutionPlanMetadata` by id.
    async fn get_execution_plan_metadata(
        &self,
        id: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionPlanMetadata>>;

    /// Browse known execution plans.
    async fn list_execution_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(ExecutionPlanId, ExecutionPlanMetadata)>>;

    // ---- scoped label / tag updates --------------------------------------

    /// Replace labels on the scoped record.
    async fn set_labels(
        &self,
        scope:  &MetadataScope,
        labels: &Labels,
    ) -> PersistenceResult<()>;

    /// Replace tags on the scoped record.
    async fn set_tags(
        &self,
        scope: &MetadataScope,
        tags:  &Tags,
    ) -> PersistenceResult<()>;
}
