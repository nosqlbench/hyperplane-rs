// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_compiler::Compiler`].

use jiff::Timestamp;
use paramodel_compiler::{Compiler, CompilerOptions};
use paramodel_elements::{
    Element, ElementName, LabelValue, Labels, attributes::label,
};
use paramodel_plan::{PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use ulid::Ulid;

use crate::providers::CompilerProvider;

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn trivial_plan() -> TestPlan {
    let db = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc_labels())
        .build();
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("p").unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

/// Compilation must be pure over the source plan — the same input
/// yields the same execution-plan fingerprint across compiler
/// instances.
pub fn tck_compiler_fingerprint_stable<P: CompilerProvider>(provider: &P) {
    let plan = trivial_plan();
    let a = provider.compiler(CompilerOptions::default()).compile(&plan).unwrap();
    let b = provider.compiler(CompilerOptions::default()).compile(&plan).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
}

/// Compiled graphs must be acyclic and every dependency must
/// resolve to a step in the graph.
pub fn tck_compiler_graph_well_formed<P: CompilerProvider>(provider: &P) {
    use std::collections::BTreeSet;

    let plan = trivial_plan();
    let compiled = provider
        .compiler(CompilerOptions::default())
        .compile(&plan)
        .unwrap();
    let ids: BTreeSet<&str> = compiled
        .steps()
        .iter()
        .map(|s| s.id().as_str())
        .collect();
    for step in compiled.steps() {
        for dep in step.depends_on() {
            assert!(
                ids.contains(dep.as_str()),
                "dangling dep {} on {}",
                dep.as_str(),
                step.id().as_str(),
            );
        }
    }
    compiled
        .execution_graph
        .topological_sort()
        .expect("compiled graph must be acyclic");
}

/// Every compiled plan carries the source plan's fingerprint, giving
/// results a trace back to the authored plan.
pub fn tck_compiler_carries_source_fingerprint<P: CompilerProvider>(provider: &P) {
    let plan = trivial_plan();
    let compiled = provider
        .compiler(CompilerOptions::default())
        .compile(&plan)
        .unwrap();
    assert_eq!(compiled.source_plan_fingerprint, plan.fingerprint());
    assert_eq!(compiled.source_plan_id, plan.id);
}
