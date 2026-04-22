// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::ArtifactStore`].

use paramodel_elements::{ElementName, TrialId};
use paramodel_executor::ArtifactContent;
use paramodel_persistence::{
    ArtifactContext, ArtifactStore, PageRequest, PersistenceError, SaveMode,
};
use paramodel_trials::ArtifactRef;
use ulid::Ulid;

use crate::providers::ArtifactStoreProvider;

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn make_artifact(name: &str) -> ArtifactRef {
    ArtifactRef::builder()
        .element(ename("db"))
        .name(name.to_owned())
        .uri("inline://db/log".to_owned())
        .content_type("text/plain".to_owned())
        .build()
}

const fn trial_id(n: u64) -> TrialId {
    TrialId::from_ulid(Ulid::from_parts(n, 1))
}

fn ctx(trial: TrialId) -> ArtifactContext {
    ArtifactContext {
        execution: None,
        trial:     Some(trial),
        element:   Some(ename("db")),
    }
}

/// Save then download round-trips inline content.
pub async fn tck_artifact_save_and_download<P: ArtifactStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let id = store
        .save(
            &ctx(trial_id(1)),
            &make_artifact("log"),
            ArtifactContent::inline(b"hello".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    let content = store.download(&id).await.unwrap();
    match content {
        ArtifactContent::Inline(bytes) => assert_eq!(&bytes, b"hello"),
        ArtifactContent::Uri(_) => panic!("expected inline content"),
    }
}

/// `SaveMode::CreateNew` rejects a duplicate under the same logical
/// key.
pub async fn tck_artifact_save_mode_create_new_rejects_duplicate<
    P: ArtifactStoreProvider,
>(
    provider: &P,
) {
    let store = provider.fresh();
    let c = ctx(trial_id(1));
    let a = make_artifact("log");
    store
        .save(
            &c,
            &a,
            ArtifactContent::inline(b"first".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    let err = store
        .save(
            &c,
            &a,
            ArtifactContent::inline(b"second".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PersistenceError::Conflict { .. }));
}

/// `SaveMode::Replace` overwrites the prior record.
pub async fn tck_artifact_save_mode_replace_overwrites<P: ArtifactStoreProvider>(
    provider: &P,
) {
    let store = provider.fresh();
    let c = ctx(trial_id(1));
    let a = make_artifact("log");
    let first = store
        .save(
            &c,
            &a,
            ArtifactContent::inline(b"first".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    let second = store
        .save(
            &c,
            &a,
            ArtifactContent::inline(b"second".to_vec()),
            SaveMode::Replace,
        )
        .await
        .unwrap();
    assert_ne!(first, second);
    assert!(store.get(&first).await.is_err());
    let got = store.download(&second).await.unwrap();
    match got {
        ArtifactContent::Inline(b) => assert_eq!(&b, b"second"),
        ArtifactContent::Uri(_) => panic!("expected inline"),
    }
}

/// `list` matches the context filter.
pub async fn tck_artifact_list_by_context<P: ArtifactStoreProvider>(provider: &P) {
    let store = provider.fresh();
    store
        .save(
            &ctx(trial_id(1)),
            &make_artifact("log"),
            ArtifactContent::inline(b"a".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    store
        .save(
            &ctx(trial_id(2)),
            &make_artifact("log"),
            ArtifactContent::inline(b"b".to_vec()),
            SaveMode::CreateNew,
        )
        .await
        .unwrap();
    let page = store
        .list(&ctx(trial_id(1)), PageRequest::first(10))
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
}
