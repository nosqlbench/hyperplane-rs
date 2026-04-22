// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MockArtifactStore` — in-memory implementation of
//! [`paramodel_persistence::ArtifactStore`].

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use jiff::Timestamp;
use paramodel_executor::ArtifactContent;
use paramodel_persistence::{
    ArtifactContext, ArtifactId, ArtifactRecord, ArtifactStore, CleanupReport, Page,
    PageRequest, PersistenceError, PersistenceResult, PersistenceStream, RetentionPolicy,
    SaveMode,
};
use paramodel_trials::ArtifactRef;
use ulid::Ulid;

/// In-memory artifact store. Serves as a reference implementation
/// suitable for testing and documentation.
#[derive(Debug, Default)]
pub struct MockArtifactStore {
    state: Mutex<InnerState>,
}

#[derive(Debug, Default)]
struct InnerState {
    records:  BTreeMap<ArtifactId, ArtifactRecord>,
    contents: BTreeMap<ArtifactId, ArtifactContent>,
}

impl MockArtifactStore {
    /// Construct an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next_id() -> ArtifactId {
        ArtifactId::from_ulid(Ulid::new())
    }

    fn size_of(content: &ArtifactContent) -> u64 {
        match content {
            ArtifactContent::Inline(b) => u64::try_from(b.len()).unwrap_or(u64::MAX),
            ArtifactContent::Uri(_) => 0,
        }
    }

    fn matches(record: &ArtifactRecord, ctx: &ArtifactContext) -> bool {
        ctx.execution
            .as_ref()
            .is_none_or(|e| record.context.execution.as_ref() == Some(e))
            && ctx
                .trial
                .as_ref()
                .is_none_or(|t| record.context.trial.as_ref() == Some(t))
            && ctx
                .element
                .as_ref()
                .is_none_or(|el| record.context.element.as_ref() == Some(el))
    }
}

#[async_trait]
impl ArtifactStore for MockArtifactStore {
    async fn save(
        &self,
        ctx:      &ArtifactContext,
        artifact: &ArtifactRef,
        content:  ArtifactContent,
        mode:     SaveMode,
    ) -> PersistenceResult<ArtifactId> {
        let size = Self::size_of(&content);
        let mut state = self.state.lock().expect("poisoned");

        // Duplicate detection uses (context, element, name) as the
        // logical key.
        let existing: Vec<ArtifactId> = state
            .records
            .iter()
            .filter(|(_, r)| {
                r.context == *ctx
                    && r.artifact.element == artifact.element
                    && r.artifact.name == artifact.name
            })
            .map(|(id, _)| *id)
            .collect();

        match mode {
            SaveMode::CreateNew if !existing.is_empty() => {
                return Err(PersistenceError::conflict(
                    "artifact",
                    format!("{}/{}", artifact.element, artifact.name),
                ));
            }
            SaveMode::Replace => {
                for id in &existing {
                    state.records.remove(id);
                    state.contents.remove(id);
                }
            }
            _ => {}
        }

        let id = Self::next_id();
        let record = ArtifactRecord {
            id,
            context: ctx.clone(),
            artifact: artifact.clone(),
            stored_at: Timestamp::now(),
            size_bytes: size,
            sha256: None,
        };
        state.records.insert(id, record);
        state.contents.insert(id, content);
        Ok(id)
    }

    async fn get(&self, id: &ArtifactId) -> PersistenceResult<ArtifactRecord> {
        self.state
            .lock()
            .expect("poisoned")
            .records
            .get(id)
            .cloned()
            .ok_or_else(|| PersistenceError::not_found("artifact", id.to_string()))
    }

    async fn download(
        &self,
        id: &ArtifactId,
    ) -> PersistenceResult<ArtifactContent> {
        self.state
            .lock()
            .expect("poisoned")
            .contents
            .get(id)
            .cloned()
            .ok_or_else(|| PersistenceError::not_found("artifact", id.to_string()))
    }

    async fn list(
        &self,
        context: &ArtifactContext,
        page:    PageRequest,
    ) -> PersistenceResult<Page<ArtifactRecord>> {
        let page = page.clamped();
        let state = self.state.lock().expect("poisoned");
        let mut matching: Vec<ArtifactRecord> = state
            .records
            .values()
            .filter(|r| Self::matches(r, context))
            .cloned()
            .collect();
        matching.sort_by_key(|r| r.stored_at);
        let take = page.size as usize;
        let items: Vec<ArtifactRecord> = matching.into_iter().take(take).collect();
        Ok(Page::terminal(items))
    }

    async fn stream(
        &self,
        context: &ArtifactContext,
    ) -> PersistenceStream<ArtifactRecord> {
        let snapshot: Vec<ArtifactRecord> = self
            .state
            .lock()
            .expect("poisoned")
            .records
            .values()
            .filter(|r| Self::matches(r, context))
            .cloned()
            .collect();
        stream::iter(snapshot.into_iter().map(Ok)).boxed()
    }

    async fn delete(&self, id: &ArtifactId) -> PersistenceResult<()> {
        let mut state = self.state.lock().expect("poisoned");
        state.records.remove(id);
        state.contents.remove(id);
        Ok(())
    }

    async fn run_retention(
        &self,
        _policy: &RetentionPolicy,
    ) -> PersistenceResult<CleanupReport> {
        // V0.1 mock: retention is a no-op. Real retention lives in
        // production backends.
        Ok(CleanupReport::default())
    }
}
