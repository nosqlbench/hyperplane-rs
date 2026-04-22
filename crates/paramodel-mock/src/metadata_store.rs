// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockMetadataStore` — in-memory implementation of
//! [`paramodel_persistence::MetadataStore`].
//!
//! Labels / tags set via `set_labels` / `set_tags` are stored in an
//! overlay keyed by `MetadataScope`. The underlying plan metadata
//! records don't carry labels/tags directly; adopters wanting to
//! observe the overlay should query it separately (out of scope
//! for the minimal TCK surface).

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use paramodel_elements::{Fingerprint, Labels, Tags};
use paramodel_persistence::{
    MetadataScope, MetadataStore, Page, PageRequest, PersistenceResult,
};
use paramodel_plan::{ExecutionPlanId, ExecutionPlanMetadata, TestPlanMetadata};

/// In-memory metadata store.
#[derive(Debug, Default)]
pub struct MockMetadataStore {
    state: Mutex<InnerState>,
}

#[derive(Debug, Default)]
struct InnerState {
    test_plans:     BTreeMap<Fingerprint, TestPlanMetadata>,
    execution_plans: BTreeMap<ExecutionPlanId, ExecutionPlanMetadata>,
    labels_overlay: BTreeMap<MetadataScope, Labels>,
    tags_overlay:   BTreeMap<MetadataScope, Tags>,
}

impl MockMetadataStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inspect the label overlay for a scope (useful in tests).
    #[must_use]
    pub fn labels_for(&self, scope: &MetadataScope) -> Option<Labels> {
        self.state
            .lock()
            .expect("poisoned")
            .labels_overlay
            .get(scope)
            .cloned()
    }

    /// Inspect the tag overlay for a scope (useful in tests).
    #[must_use]
    pub fn tags_for(&self, scope: &MetadataScope) -> Option<Tags> {
        self.state
            .lock()
            .expect("poisoned")
            .tags_overlay
            .get(scope)
            .cloned()
    }
}

#[async_trait]
impl MetadataStore for MockMetadataStore {
    async fn save_test_plan_metadata(
        &self,
        fp:   &Fingerprint,
        meta: &TestPlanMetadata,
    ) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .test_plans
            .insert(*fp, meta.clone());
        Ok(())
    }

    async fn get_test_plan_metadata(
        &self,
        fp: &Fingerprint,
    ) -> PersistenceResult<Option<TestPlanMetadata>> {
        Ok(self
            .state
            .lock()
            .expect("poisoned")
            .test_plans
            .get(fp)
            .cloned())
    }

    async fn list_test_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(Fingerprint, TestPlanMetadata)>> {
        let page = page.clamped();
        let items: Vec<(Fingerprint, TestPlanMetadata)> = self
            .state
            .lock()
            .expect("poisoned")
            .test_plans
            .iter()
            .take(page.size as usize)
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        Ok(Page::terminal(items))
    }

    async fn save_execution_plan_metadata(
        &self,
        id:   &ExecutionPlanId,
        meta: &ExecutionPlanMetadata,
    ) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .execution_plans
            .insert(*id, meta.clone());
        Ok(())
    }

    async fn get_execution_plan_metadata(
        &self,
        id: &ExecutionPlanId,
    ) -> PersistenceResult<Option<ExecutionPlanMetadata>> {
        Ok(self
            .state
            .lock()
            .expect("poisoned")
            .execution_plans
            .get(id)
            .cloned())
    }

    async fn list_execution_plans(
        &self,
        page: PageRequest,
    ) -> PersistenceResult<Page<(ExecutionPlanId, ExecutionPlanMetadata)>> {
        let page = page.clamped();
        let items: Vec<(ExecutionPlanId, ExecutionPlanMetadata)> = self
            .state
            .lock()
            .expect("poisoned")
            .execution_plans
            .iter()
            .take(page.size as usize)
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        Ok(Page::terminal(items))
    }

    async fn set_labels(
        &self,
        scope:  &MetadataScope,
        labels: &Labels,
    ) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .labels_overlay
            .insert(scope.clone(), labels.clone());
        Ok(())
    }

    async fn set_tags(
        &self,
        scope: &MetadataScope,
        tags:  &Tags,
    ) -> PersistenceResult<()> {
        self.state
            .lock()
            .expect("poisoned")
            .tags_overlay
            .insert(scope.clone(), tags.clone());
        Ok(())
    }
}
