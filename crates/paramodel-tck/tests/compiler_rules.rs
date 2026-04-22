// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Per-rule verification for the default compiler.
//!
//! Each test builds a minimal plan that isolates a rule and asserts
//! the shape reducto prescribes. Broad coverage beyond what
//! `paramodel-compiler`'s unit tests already exercise.

use std::collections::BTreeSet;

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Dependency, Element, ElementName, HealthCheckSpec, IntegerParameter, LabelValue,
    Labels, Parameter, ParameterName, Value, attributes::label,
};
use paramodel_plan::{
    AtomicStep, Axis, AxisName, BarrierKind, ElementParameterRef, PlanName, TestPlan,
    TestPlanId, TestPlanMetadata,
};
use ulid::Ulid;

fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}
fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}
fn plain(name: &str) -> Element {
    Element::builder()
        .name(ename(name))
        .labels(svc_labels())
        .build()
}
fn with_axis(name: &str, p: &str) -> Element {
    Element::builder()
        .name(ename(name))
        .labels(svc_labels())
        .parameters(vec![Parameter::Integer(
            IntegerParameter::range(pname(p), 1, 64).unwrap(),
        )])
        .build()
}
fn axis_on(el: &str, p: &str, values: &[i64]) -> Axis {
    Axis::builder()
        .name(AxisName::new(format!("{el}_{p}")).unwrap())
        .target(ElementParameterRef::new(ename(el), pname(p)))
        .values(
            values
                .iter()
                .map(|v| Value::integer(pname(p), *v, None))
                .collect(),
        )
        .build()
}
fn plan(elements: Vec<Element>, axes: Vec<Axis>) -> TestPlan {
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("p").unwrap())
        .elements(elements)
        .axes(axes)
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

fn step_ids(plan: &paramodel_plan::ExecutionPlan) -> BTreeSet<String> {
    plan.steps()
        .iter()
        .map(|s| s.id().as_str().to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 1 — lifecycle expansion.
// ---------------------------------------------------------------------------

#[test]
fn rule1_emits_deploy_and_teardown_per_element_per_trial() {
    let p = plan(
        vec![with_axis("db", "threads")],
        vec![axis_on("db", "threads", &[1, 2, 3])],
    );
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    let ids = step_ids(&compiled);
    for t in 0..3 {
        assert!(ids.contains(&format!("activate_db_t{t}")));
        assert!(ids.contains(&format!("deactivate_db_t{t}")));
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — Shared.
// ---------------------------------------------------------------------------

#[test]
fn rule2_shared_wires_deploy_forward_and_teardown_reverse() {
    let mut client = plain("client");
    client.dependencies.push(Dependency::shared(ename("db")));
    let p = plan(vec![plain("db"), client], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();

    // activate_client_t0 transitively depends on activate_db_t0.
    assert!(paramodel_tck_support::depends_transitively(
        &compiled,
        "activate_client_t0",
        "activate_db_t0",
    ));
    // deactivate_db_t0 transitively depends on deactivate_client_t0.
    assert!(paramodel_tck_support::depends_transitively(
        &compiled,
        "deactivate_db_t0",
        "deactivate_client_t0",
    ));
}

// ---------------------------------------------------------------------------
// Rule 5 — health-check readiness gate.
// ---------------------------------------------------------------------------

#[test]
fn rule5_inserts_readiness_barrier_for_health_checked_element() {
    use std::time::Duration;
    let mut db = plain("db");
    db.health_check = Some(HealthCheckSpec::new(
        Duration::from_secs(5),
        3,
        Duration::from_millis(500),
    ));
    let mut client = plain("client");
    client.dependencies.push(Dependency::shared(ename("db")));
    let p = plan(vec![db, client], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();

    // A Barrier step with kind ElementReady exists.
    assert!(compiled.steps().iter().any(|s| matches!(
        s,
        AtomicStep::Barrier {
            barrier_kind: BarrierKind::ElementReady,
            ..
        }
    )));
    // Client's deploy no longer directly depends on activate_db_t0
    // — routes through the gate.
    let client_deploy = compiled
        .steps()
        .iter()
        .find(|s| s.id().as_str() == "activate_client_t0")
        .expect("client deploy");
    assert!(
        !client_deploy
            .depends_on()
            .iter()
            .any(|d| d.as_str() == "activate_db_t0"),
    );
}

// ---------------------------------------------------------------------------
// Rule 6 — concurrency metadata is stamped on Deploy.
// ---------------------------------------------------------------------------

#[test]
fn rule6_concurrency_metadata_stamped_on_deploy() {
    let mut db = plain("db");
    db.max_concurrency = Some(4);
    db.max_group_concurrency = Some(2);
    let p = plan(vec![db], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    let deploy = compiled
        .steps()
        .iter()
        .find(|s| s.id().as_str() == "activate_db_t0")
        .expect("deploy");
    if let AtomicStep::Deploy {
        max_concurrency,
        max_group_concurrency,
        ..
    } = deploy
    {
        assert_eq!(*max_concurrency, Some(4));
        assert_eq!(*max_group_concurrency, Some(2));
    } else {
        panic!("expected Deploy");
    }
}

// ---------------------------------------------------------------------------
// Rule 7 — Start / End sentinels.
// ---------------------------------------------------------------------------

#[test]
fn rule7_emits_start_and_end_sentinels() {
    let p = plan(vec![plain("db")], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    let ids = step_ids(&compiled);
    assert!(ids.contains("start"));
    assert!(ids.contains("end"));
    // Start has no dependencies.
    let start = compiled
        .steps()
        .iter()
        .find(|s| s.id().as_str() == "start")
        .unwrap();
    assert!(start.depends_on().is_empty());
}

// ---------------------------------------------------------------------------
// Rule 8 — transitive reduction removes redundant direct edges.
// ---------------------------------------------------------------------------

#[test]
fn rule8_transitive_reduction_drops_redundancies() {
    // A ← B, A ← C, B ← C.  The direct A → C edge is redundant
    // (implied by A → B → C).
    let mut b = plain("b");
    b.dependencies.push(Dependency::shared(ename("a")));
    let mut c = plain("c");
    c.dependencies.push(Dependency::shared(ename("a")));
    c.dependencies.push(Dependency::shared(ename("b")));
    let p = plan(vec![plain("a"), b, c], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();

    let deploy_c = compiled
        .steps()
        .iter()
        .find(|s| s.id().as_str() == "activate_c_t0")
        .unwrap();
    // Direct a edge must be gone.
    assert!(
        !deploy_c
            .depends_on()
            .iter()
            .any(|d| d.as_str() == "activate_a_t0"),
    );
    // But transitive reachability preserved.
    assert!(paramodel_tck_support::depends_transitively(
        &compiled,
        "activate_c_t0",
        "activate_a_t0",
    ));
}

// ---------------------------------------------------------------------------
// Rule 3 — coalescing: a level-0 element folds across all trials.
// ---------------------------------------------------------------------------

#[test]
fn rule3_run_scoped_element_folds_to_one_activate() {
    // `db` has no axes (run-scoped); client has one axis — 3 trials.
    let mut client = with_axis("client", "threads");
    client.dependencies.push(Dependency::shared(ename("db")));
    let p = plan(
        vec![plain("db"), client],
        vec![axis_on("client", "threads", &[1, 2, 3])],
    );
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    let db_activates = compiled
        .steps()
        .iter()
        .filter(|s| s.id().as_str().starts_with("activate_db_"))
        .count();
    let db_teardowns = compiled
        .steps()
        .iter()
        .filter(|s| s.id().as_str().starts_with("deactivate_db_"))
        .count();
    assert_eq!(db_activates, 1);
    assert_eq!(db_teardowns, 1);
}

// ---------------------------------------------------------------------------
// Rule 4 — TrialStart / TrialEnd per trial.
// ---------------------------------------------------------------------------

#[test]
fn rule4_emits_trial_notifications_per_trial() {
    let mut client = with_axis("client", "threads");
    client.dependencies.push(Dependency::shared(ename("db")));
    let p = plan(
        vec![plain("db"), client],
        vec![axis_on("client", "threads", &[1, 2])],
    );
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    for t in 0..2 {
        assert!(compiled.steps().iter().any(
            |s| s.id().as_str() == format!("notify_trial_start_t{t}"),
        ));
        assert!(compiled.steps().iter().any(
            |s| s.id().as_str() == format!("notify_trial_end_t{t}"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — Dedicated.
// ---------------------------------------------------------------------------

#[test]
fn rule2_dedicated_materialises_owner_specific_instance() {
    let mut client = plain("client");
    client.dependencies.push(Dependency::dedicated(ename("db")));
    let p = plan(vec![plain("db"), client], vec![]);
    let compiled = DefaultCompiler::default().compile(&p).unwrap();
    assert!(
        compiled
            .steps()
            .iter()
            .any(|s| s.id().as_str() == "activate_dedicated_db_for_client_t0"),
    );
}

// Tiny reachability helper used above — kept here so the test file
// is standalone.
mod paramodel_tck_support {
    use paramodel_plan::ExecutionPlan;
    use std::collections::BTreeSet;

    pub fn depends_transitively(plan: &ExecutionPlan, from: &str, to: &str) -> bool {
        let mut stack: Vec<String> = plan
            .steps()
            .iter()
            .find(|s| s.id().as_str() == from)
            .map(|s| {
                s.depends_on()
                    .iter()
                    .map(|d| d.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        while let Some(curr) = stack.pop() {
            if curr == to {
                return true;
            }
            if !seen.insert(curr.clone()) {
                continue;
            }
            if let Some(s) = plan.steps().iter().find(|s| s.id().as_str() == curr) {
                for d in s.depends_on() {
                    stack.push(d.as_str().to_owned());
                }
            }
        }
        false
    }
}
