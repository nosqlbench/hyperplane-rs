// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Golden-file tests for the default compiler. The golden is
//! regeneratable via `PARAMODEL_TCK_BLESS=1 cargo test`.

use std::path::PathBuf;

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Element, ElementName, LabelValue, Labels, attributes::label,
};
use paramodel_plan::{PlanName, TestPlan, TestPlanId, TestPlanMetadata};
use ulid::Ulid;

fn service_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn trivial_plan() -> TestPlan {
    let db = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(service_labels())
        .build();
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("trivial").unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

fn golden_path(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/goldens");
    p.push(format!("{name}.json"));
    p
}

#[test]
fn trivial_plan_golden_matches() {
    let plan = trivial_plan();
    let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");
    paramodel_tck::golden::assert_golden(&compiled, golden_path("trivial_plan"))
        .expect("golden match");
}
