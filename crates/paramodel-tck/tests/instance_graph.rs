// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `InstanceScope`, `InstanceId`, `ElementInstance`, and
//! `ElementInstanceGraph` accessor + serde coverage.

use std::collections::BTreeMap;

use paramodel_elements::{ElementName, ParameterName, RelationshipType, Value};
use paramodel_plan::{
    ElementInstance, ElementInstanceGraph, InstanceDependency, InstanceId,
    InstanceScope,
};

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}
fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

// ---------------------------------------------------------------------------
// InstanceId: from_parts is deterministic.
// ---------------------------------------------------------------------------

#[test]
fn instance_id_from_parts_is_deterministic() {
    let a = InstanceId::from_parts(&ename("db"), 0);
    let b = InstanceId::from_parts(&ename("db"), 0);
    assert_eq!(a, b);
}

#[test]
fn instance_id_differs_across_element_or_instance_number() {
    let a = InstanceId::from_parts(&ename("db"), 0);
    let b = InstanceId::from_parts(&ename("db"), 1);
    let c = InstanceId::from_parts(&ename("other"), 0);
    assert_ne!(a, b);
    assert_ne!(a, c);
}

// ---------------------------------------------------------------------------
// InstanceScope serde.
// ---------------------------------------------------------------------------

#[test]
fn instance_scope_serde_is_snake_case() {
    for (scope, expected) in [
        (InstanceScope::Study, "\"study\""),
        (InstanceScope::Trial, "\"trial\""),
        (InstanceScope::Invocation, "\"invocation\""),
    ] {
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, expected);
        let back: InstanceScope = serde_json::from_str(&json).unwrap();
        assert_eq!(scope, back);
    }
}

// ---------------------------------------------------------------------------
// ElementInstance construction via builder.
// ---------------------------------------------------------------------------

fn simple_instance(n: u32, scope: InstanceScope) -> ElementInstance {
    let mut bindings = BTreeMap::new();
    bindings.insert(
        pname("threads"),
        Value::integer(pname("threads"), i64::from(n) + 1, None),
    );
    ElementInstance::builder()
        .id(InstanceId::from_parts(&ename("db"), n))
        .element(ename("db"))
        .instance_number(n)
        .bindings(bindings)
        .group_level(0)
        .trial_code(format!("0x{n:02x}"))
        .scope(scope)
        .build()
}

#[test]
fn element_instance_builder_records_fields() {
    let inst = simple_instance(3, InstanceScope::Trial);
    assert_eq!(inst.instance_number, 3);
    assert_eq!(inst.scope, InstanceScope::Trial);
    assert_eq!(inst.trial_code.as_deref(), Some("0x03"));
    assert_eq!(inst.bindings.len(), 1);
}

// ---------------------------------------------------------------------------
// ElementInstanceGraph accessors.
// ---------------------------------------------------------------------------

#[test]
fn graph_instance_lookup_by_id() {
    let i0 = simple_instance(0, InstanceScope::Trial);
    let i1 = simple_instance(1, InstanceScope::Trial);
    let graph = ElementInstanceGraph::builder()
        .instances(vec![i0.clone(), i1.clone()])
        .build();
    assert!(graph.instance(&i0.id).is_some());
    assert!(graph.instance(&i1.id).is_some());
    assert!(
        graph
            .instance(&InstanceId::from_parts(&ename("ghost"), 0))
            .is_none()
    );
}

#[test]
fn graph_instances_of_filters_by_element() {
    let db0 = simple_instance(0, InstanceScope::Trial);
    let db1 = simple_instance(1, InstanceScope::Trial);
    let client = ElementInstance::builder()
        .id(InstanceId::from_parts(&ename("client"), 0))
        .element(ename("client"))
        .instance_number(0)
        .group_level(0)
        .scope(InstanceScope::Trial)
        .build();
    let graph = ElementInstanceGraph::builder()
        .instances(vec![db0, db1, client])
        .build();
    assert_eq!(graph.instances_of(&ename("db")).len(), 2);
    assert_eq!(graph.instances_of(&ename("client")).len(), 1);
    assert_eq!(graph.instances_of(&ename("ghost")).len(), 0);
}

#[test]
fn graph_serde_roundtrip() {
    let inst = simple_instance(0, InstanceScope::Trial);
    let edge = InstanceDependency {
        source:       inst.id.clone(),
        target:       InstanceId::from_parts(&ename("other"), 0),
        relationship: RelationshipType::Shared,
    };
    let graph = ElementInstanceGraph::builder()
        .instances(vec![inst])
        .edges(vec![edge])
        .build();
    let json = serde_json::to_string(&graph).unwrap();
    let back: ElementInstanceGraph = serde_json::from_str(&json).unwrap();
    assert_eq!(graph, back);
}

// ---------------------------------------------------------------------------
// Total instance count accessor (defined via len semantics).
// ---------------------------------------------------------------------------

#[test]
fn graph_total_instances_counts_instances_vec() {
    let graph = ElementInstanceGraph::builder()
        .instances(vec![
            simple_instance(0, InstanceScope::Trial),
            simple_instance(1, InstanceScope::Trial),
            simple_instance(2, InstanceScope::Trial),
        ])
        .build();
    assert_eq!(graph.total_instances(), 3);
}
