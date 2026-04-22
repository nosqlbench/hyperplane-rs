// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Proptest-driven algebraic laws for paramodel primitives.
//!
//! These laws are adopter-agnostic — they target the core types in
//! `paramodel-elements` rather than an implementation of a trait,
//! and serve as regression coverage for the invariants the algebra
//! relies on.

use std::collections::BTreeSet;

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler, MixedRadixEnumerator};
use paramodel_elements::{
    Constraint, Element, ElementName, Fingerprint, IntConstraint, IntegerParameter,
    LabelValue, Labels, ParameterName, Value, attributes::label,
};
use paramodel_plan::{
    Axis, AxisName, ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata,
};
use proptest::prelude::*;
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

// ---------------------------------------------------------------------------
// Fingerprint determinism.
// ---------------------------------------------------------------------------

proptest! {
    /// Same bytes → same fingerprint.
    #[test]
    fn fingerprint_is_deterministic(bytes: Vec<u8>) {
        let a = Fingerprint::of(&bytes);
        let b = Fingerprint::of(&bytes);
        prop_assert_eq!(a, b);
    }

    /// Different byte strings *usually* produce different
    /// fingerprints. The proptest shrinker catches the rare case
    /// where the property falsifies (it shouldn't for blake3).
    #[test]
    fn fingerprints_of_distinct_inputs_usually_differ(
        a in proptest::collection::vec(any::<u8>(), 1..32),
        b in proptest::collection::vec(any::<u8>(), 1..32),
    ) {
        if a != b {
            prop_assert_ne!(Fingerprint::of(&a), Fingerprint::of(&b));
        }
    }
}

// ---------------------------------------------------------------------------
// Value fingerprint is independent of provenance timestamp.
// ---------------------------------------------------------------------------

proptest! {
    /// Two `Value::integer` observations with the same parameter
    /// and number must agree on canonical-bytes-derived fingerprints,
    /// even though their provenance timestamps differ at
    /// construction.
    #[test]
    fn value_fingerprint_ignores_provenance(n: i64) {
        let v1 = Value::integer(pname("x"), n, None);
        let v2 = Value::integer(pname("x"), n, None);
        prop_assert_eq!(
            v1.provenance().fingerprint,
            v2.provenance().fingerprint,
        );
    }
}

// ---------------------------------------------------------------------------
// IntegerParameter::range membership.
// ---------------------------------------------------------------------------

proptest! {
    /// Every integer in [min, max] is a member of
    /// `IntegerParameter::range(min, max)`'s domain; no integer
    /// outside is.
    #[test]
    fn integer_range_membership(
        (min, max) in (-1000i64..=1000).prop_flat_map(|m| (Just(m), (m..=1000))),
        probe in -2000i64..=2000,
    ) {
        let p = IntegerParameter::range(pname("v"), min, max).unwrap();
        let inside = (min..=max).contains(&probe);
        prop_assert_eq!(p.domain.contains_native(probe), inside);
    }
}

// ---------------------------------------------------------------------------
// MixedRadixEnumerator — `trial_index ∘ offsets` is identity.
// ---------------------------------------------------------------------------

fn axis_of(card: u32) -> Axis {
    Axis::builder()
        .name(AxisName::new("ax").unwrap())
        .target(ElementParameterRef::new(
            paramodel_elements::ElementName::new("e").unwrap(),
            pname("p"),
        ))
        .values(
            (0..card)
                .map(|i| Value::integer(pname("p"), i64::from(i), None))
                .collect(),
        )
        .build()
}

proptest! {
    /// `trial_index(offsets(t)) == t` for every valid trial.
    #[test]
    fn trial_index_offsets_roundtrip(
        cards in proptest::collection::vec(1u32..=5, 1..4),
    ) {
        let axes: Vec<Axis> = cards.iter().copied().map(axis_of).collect();
        let enumerator = MixedRadixEnumerator::new(&axes);
        for t in 0..enumerator.trial_count() {
            let offsets = enumerator.offsets(t);
            prop_assert_eq!(enumerator.trial_index(&offsets), t);
        }
    }
}

// ---------------------------------------------------------------------------
// Constraint Boolean algebra.
// ---------------------------------------------------------------------------

/// Evaluate a constraint against an integer value.
fn test_int(c: &Constraint, n: i64) -> bool {
    c.test(&Value::integer(pname("v"), n, None))
}

proptest! {
    /// Constraint evaluation is deterministic.
    #[test]
    fn constraint_determinism(lt in -100i64..=100, probe in -200i64..=200) {
        let c = Constraint::Integer(IntConstraint::Max { n: lt });
        prop_assert_eq!(test_int(&c, probe), test_int(&c, probe));
    }

    /// `test(And[X, Y], v) == test(X, v) && test(Y, v)` — intersection.
    #[test]
    fn int_and_is_intersection(
        a in -50i64..=50,
        b in -50i64..=50,
        probe in -100i64..=100,
    ) {
        let x = IntConstraint::Min { n: a };
        let y = IntConstraint::Max { n: b };
        let both = IntConstraint::And { children: vec![x.clone(), y.clone()] };
        let combined = Constraint::Integer(both);
        let expected = test_int(&Constraint::Integer(x), probe)
            && test_int(&Constraint::Integer(y), probe);
        prop_assert_eq!(test_int(&combined, probe), expected);
    }

    /// `test(Or[X, Y], v) == test(X, v) || test(Y, v)` — union.
    #[test]
    fn int_or_is_union(
        a in -50i64..=50,
        b in -50i64..=50,
        probe in -100i64..=100,
    ) {
        let x = IntConstraint::Max { n: a };
        let y = IntConstraint::Min { n: b };
        let either = IntConstraint::Or { children: vec![x.clone(), y.clone()] };
        let combined = Constraint::Integer(either);
        let expected = test_int(&Constraint::Integer(x), probe)
            || test_int(&Constraint::Integer(y), probe);
        prop_assert_eq!(test_int(&combined, probe), expected);
    }

    /// `test(Not(X), v) == !test(X, v)` — complement.
    #[test]
    fn int_not_is_complement(a in -50i64..=50, probe in -100i64..=100) {
        let x = IntConstraint::Max { n: a };
        let not_x = IntConstraint::Not { child: Box::new(x.clone()) };
        prop_assert_eq!(
            test_int(&Constraint::Integer(not_x), probe),
            !test_int(&Constraint::Integer(x), probe),
        );
    }

    /// `test(Not(Not(X)), v) == test(X, v)` — double negation.
    #[test]
    fn int_double_negation(a in -50i64..=50, probe in -100i64..=100) {
        let x = IntConstraint::Max { n: a };
        let nn = IntConstraint::Not {
            child: Box::new(IntConstraint::Not { child: Box::new(x.clone()) }),
        };
        prop_assert_eq!(
            test_int(&Constraint::Integer(nn), probe),
            test_int(&Constraint::Integer(x), probe),
        );
    }

    /// De Morgan: `Not(And[X, Y]) == Or[Not(X), Not(Y)]`.
    #[test]
    fn int_de_morgan_and(
        a in -50i64..=50,
        b in -50i64..=50,
        probe in -100i64..=100,
    ) {
        let x = IntConstraint::Min { n: a };
        let y = IntConstraint::Max { n: b };
        let left = IntConstraint::Not {
            child: Box::new(IntConstraint::And {
                children: vec![x.clone(), y.clone()],
            }),
        };
        let right = IntConstraint::Or {
            children: vec![
                IntConstraint::Not { child: Box::new(x) },
                IntConstraint::Not { child: Box::new(y) },
            ],
        };
        prop_assert_eq!(
            test_int(&Constraint::Integer(left), probe),
            test_int(&Constraint::Integer(right), probe),
        );
    }
}

// ---------------------------------------------------------------------------
// Plan fingerprint stability.
// ---------------------------------------------------------------------------

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

fn simple_plan(name: &str) -> TestPlan {
    let db = Element::builder()
        .name(ElementName::new("db").unwrap())
        .labels(svc_labels())
        .build();
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new(name).unwrap())
        .elements(vec![db])
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

#[test]
fn compiled_plan_fingerprint_is_stable_across_compiles() {
    // Compile the same source plan twice; the execution-plan
    // fingerprint must match. Compilation is pure over the source
    // plan; wall-clock fields in metadata are excluded from the
    // fingerprint (SRD-0009 D11).
    let plan = simple_plan("p");
    let compiled_a = DefaultCompiler::default().compile(&plan).unwrap();
    let compiled_b = DefaultCompiler::default().compile(&plan).unwrap();
    assert_eq!(compiled_a.fingerprint(), compiled_b.fingerprint());
}

#[test]
fn compiled_plan_fingerprint_differs_when_source_differs() {
    // Adding a label on the source plan perturbs the source-plan
    // fingerprint... wait — labels aren't in the TestPlan fingerprint
    // either. Instead, differ the element count.
    let plan_a = simple_plan("a");
    let mut plan_b_elements = plan_a.elements.clone();
    plan_b_elements.push(
        Element::builder()
            .name(ElementName::new("extra").unwrap())
            .labels(svc_labels())
            .build(),
    );
    let plan_b = TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
        .name(PlanName::new("b").unwrap())
        .elements(plan_b_elements)
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build();
    let compiled_a = DefaultCompiler::default().compile(&plan_a).unwrap();
    let compiled_b = DefaultCompiler::default().compile(&plan_b).unwrap();
    assert_ne!(compiled_a.fingerprint(), compiled_b.fingerprint());
}

// ---------------------------------------------------------------------------
// Execution-graph invariants.
// ---------------------------------------------------------------------------

#[test]
fn compiled_graph_is_acyclic_and_every_dep_resolves() {
    let plan = simple_plan("p");
    let compiled = DefaultCompiler::default().compile(&plan).unwrap();
    let ids: BTreeSet<&str> = compiled
        .steps()
        .iter()
        .map(|s| s.id().as_str())
        .collect();
    for s in compiled.steps() {
        for d in s.depends_on() {
            assert!(
                ids.contains(d.as_str()),
                "dangling dep {} on {}",
                d.as_str(),
                s.id().as_str(),
            );
        }
    }
    // Acyclicity via topological sort round-trip: every step appears
    // exactly once.
    let sorted = compiled.execution_graph.topological_sort().unwrap();
    assert_eq!(sorted.len(), compiled.steps().len());
}
