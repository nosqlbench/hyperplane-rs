// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_persistence::MetadataStore`].

use jiff::Timestamp;
use paramodel_elements::{Fingerprint, LabelKey, LabelValue, Labels};
use paramodel_persistence::{MetadataScope, MetadataStore, PageRequest};
use paramodel_plan::TestPlanMetadata;

use crate::providers::MetadataStoreProvider;

fn fp(tag: &[u8]) -> Fingerprint {
    Fingerprint::of(tag)
}

fn tpm() -> TestPlanMetadata {
    TestPlanMetadata::builder()
        .created_at(Timestamp::from_second(1_700_000_000).unwrap())
        .build()
}

/// Save + get round-trips `TestPlanMetadata`.
pub async fn tck_metadata_save_and_get<P: MetadataStoreProvider>(provider: &P) {
    let store = provider.fresh();
    store
        .save_test_plan_metadata(&fp(b"a"), &tpm())
        .await
        .unwrap();
    let got = store.get_test_plan_metadata(&fp(b"a")).await.unwrap();
    assert_eq!(got, Some(tpm()));
}

/// `list_test_plans` returns saved records up to the page size.
pub async fn tck_metadata_list_plans<P: MetadataStoreProvider>(provider: &P) {
    let store = provider.fresh();
    for i in 0u8..5 {
        store
            .save_test_plan_metadata(&fp(&[i]), &tpm())
            .await
            .unwrap();
    }
    let page = store.list_test_plans(PageRequest::first(3)).await.unwrap();
    assert_eq!(page.items.len(), 3);
}

/// `set_labels` stores the overlay for a given scope.
pub async fn tck_metadata_set_labels<P: MetadataStoreProvider>(provider: &P) {
    let store = provider.fresh();
    let mut labels = Labels::new();
    labels.insert(
        LabelKey::new("owner").unwrap(),
        LabelValue::new("ops").unwrap(),
    );
    let scope = MetadataScope::TestPlan {
        fingerprint: fp(b"a"),
    };
    store.set_labels(&scope, &labels).await.unwrap();
    // The trait doesn't require a read-back API; the mock exposes
    // one for tests, but the conformance check just asserts success.
}
