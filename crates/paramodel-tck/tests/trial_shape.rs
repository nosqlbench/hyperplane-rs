// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Trial` / `Assignments` / `TrialMetadata` construction + invariants.

use std::collections::BTreeMap;

use paramodel_elements::{
    Assignments, ElementName, ParameterName, Trial, TrialId, TrialMetadata, Value,
};
use ulid::Ulid;

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}
fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
const fn tid(n: u64) -> TrialId {
    TrialId::from_ulid(Ulid::from_parts(n, 1))
}

fn simple_assignments() -> Assignments {
    let mut per_param = BTreeMap::new();
    per_param.insert(pname("threads"), Value::integer(pname("threads"), 4, None));
    let mut outer = BTreeMap::new();
    outer.insert(ename("db"), per_param);
    Assignments::new(outer).expect("assignments")
}

// ---------------------------------------------------------------------------
// Assignments: get / iter / empty.
// ---------------------------------------------------------------------------

#[test]
fn assignments_empty_is_empty() {
    let a = Assignments::empty();
    assert!(a.is_empty());
    assert_eq!(a.len(), 0);
}

#[test]
fn assignments_get_returns_stored_value() {
    let a = simple_assignments();
    let v = a.get(&ename("db"), &pname("threads"));
    assert!(v.is_some());
    assert_eq!(v.and_then(Value::as_integer), Some(4));
}

#[test]
fn assignments_get_missing_returns_none() {
    let a = simple_assignments();
    assert!(a.get(&ename("ghost"), &pname("threads")).is_none());
    assert!(a.get(&ename("db"), &pname("ghost")).is_none());
}

// ---------------------------------------------------------------------------
// Trial construction + serde.
// ---------------------------------------------------------------------------

#[test]
fn trial_builder_records_id_and_assignments() {
    let t = Trial::builder()
        .id(tid(1))
        .assignments(simple_assignments())
        .build();
    assert_eq!(t.id, tid(1));
    assert_eq!(t.assignments.len(), 1);
}

#[test]
fn trial_serde_roundtrip() {
    let t = Trial::builder()
        .id(tid(1))
        .assignments(simple_assignments())
        .metadata(
            TrialMetadata::builder()
                .enumeration_index(42u32)
                .group("A".to_owned())
                .priority(7i32)
                .build(),
        )
        .build();
    let json = serde_json::to_string(&t).unwrap();
    let back: Trial = serde_json::from_str(&json).unwrap();
    assert_eq!(t.id, back.id);
    assert_eq!(
        t.metadata.as_ref().and_then(|m| m.enumeration_index),
        back.metadata.as_ref().and_then(|m| m.enumeration_index),
    );
    assert_eq!(
        t.metadata.as_ref().and_then(|m| m.group.clone()),
        back.metadata.as_ref().and_then(|m| m.group.clone()),
    );
}

// ---------------------------------------------------------------------------
// Trial::canonical_bytes is deterministic across reconstructions.
// ---------------------------------------------------------------------------

#[test]
fn trial_canonical_bytes_is_deterministic() {
    let a = Trial::builder()
        .id(tid(1))
        .assignments(simple_assignments())
        .build();
    let b = Trial::builder()
        .id(tid(1))
        .assignments(simple_assignments())
        .build();
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
}

#[test]
fn trial_canonical_bytes_is_assignment_based_not_id_based() {
    // `Trial::canonical_bytes` is intentionally id-independent —
    // two trials with the same assignments hash to the same bytes
    // even with different ids.
    let a = Trial::builder()
        .id(tid(1))
        .assignments(Assignments::empty())
        .build();
    let b = Trial::builder()
        .id(tid(2))
        .assignments(Assignments::empty())
        .build();
    assert_eq!(a.canonical_bytes(), b.canonical_bytes());
    // But different assignments → different bytes.
    let c = Trial::builder()
        .id(tid(1))
        .assignments(simple_assignments())
        .build();
    assert_ne!(a.canonical_bytes(), c.canonical_bytes());
}
