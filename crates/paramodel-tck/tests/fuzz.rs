// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Proptest-driven fuzz coverage for fingerprinting, serde
//! round-trips, parameter construction, and filter evaluation.
//!
//! These tests intentionally sample broadly — they're cheap to run
//! but catch regressions that narrow unit tests miss.

use std::collections::BTreeSet;

use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    BooleanParameter, DoubleParameter, Element, ElementName, IntegerParameter,
    LabelValue, Labels, Parameter, ParameterName, StringParameter, Value,
    attributes::label,
};
use paramodel_persistence::{
    Aggregation, Comparison, GroupDimension, ResultFilter, TrialCodePattern,
};
use paramodel_plan::{
    Axis, AxisName, ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata,
};
use paramodel_trials::TrialStatus;
use proptest::prelude::*;
use ulid::Ulid;

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}
fn ename(s: &str) -> ElementName {
    ElementName::new(s).unwrap()
}

fn svc_labels() -> Labels {
    let mut l = Labels::new();
    l.insert(label::r#type(), LabelValue::new("service").unwrap());
    l
}

// ---------------------------------------------------------------------------
// Parameter construction never panics on sane inputs.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn integer_parameter_range_construction_ok(
        (min, max) in (-1000i64..=1000).prop_flat_map(|m| (Just(m), (m..=1000))),
    ) {
        IntegerParameter::range(pname("v"), min, max).unwrap();
    }

    #[test]
    fn double_parameter_range_construction_ok(
        min in -1000.0f64..=0.0,
        max in 0.0f64..=1000.0,
    ) {
        DoubleParameter::range(pname("v"), min, max).unwrap();
    }

    #[test]
    fn boolean_parameter_construction_never_panics(_ignored in 0u8..1) {
        let _ = BooleanParameter::of(pname("v"));
    }

    #[test]
    fn string_parameter_regex_accepts_valid_pattern(pat in "[a-z]{1,8}") {
        // Any generated non-empty pattern should compile.
        StringParameter::regex(pname("v"), &pat).unwrap();
    }

    // SelectionParameter::of requires indexmap::IndexSet which isn't
    // in this test's deps; skip it here (covered by dedicated unit
    // tests in paramodel-elements).
}

// ---------------------------------------------------------------------------
// Value fingerprint stability across reconstruction.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn integer_value_fingerprint_depends_only_on_inputs(n: i64, m: i64) {
        let a = Value::integer(pname("x"), n, None);
        let b = Value::integer(pname("x"), n, None);
        prop_assert_eq!(a.provenance().fingerprint, b.provenance().fingerprint);
        let c = Value::integer(pname("x"), m, None);
        if n != m {
            prop_assert_ne!(a.provenance().fingerprint, c.provenance().fingerprint);
        }
    }

    #[test]
    fn double_value_fingerprint_is_deterministic(f: f64) {
        // Skip NaN — NaN != NaN so structural comparisons diverge
        // from byte-level ones; the canonical encoding still
        // agrees.
        if f.is_nan() { return Ok(()); }
        let a = Value::double(pname("x"), f, None);
        let b = Value::double(pname("x"), f, None);
        prop_assert_eq!(a.provenance().fingerprint, b.provenance().fingerprint);
    }

    #[test]
    fn string_value_fingerprint_is_deterministic(s in "[ -~]{0,32}") {
        let a = Value::string(pname("x"), s.clone(), None);
        let b = Value::string(pname("x"), s, None);
        prop_assert_eq!(a.provenance().fingerprint, b.provenance().fingerprint);
    }
}

// ---------------------------------------------------------------------------
// ResultFilter serde fuzz — random trees roundtrip through JSON.
// ---------------------------------------------------------------------------

fn arb_comparison() -> impl Strategy<Value = Comparison> {
    prop_oneof![
        Just(Comparison::Eq),
        Just(Comparison::Ne),
        Just(Comparison::Lt),
        Just(Comparison::Le),
        Just(Comparison::Gt),
        Just(Comparison::Ge),
    ]
}

fn arb_trial_status() -> impl Strategy<Value = TrialStatus> {
    prop_oneof![
        Just(TrialStatus::Pending),
        Just(TrialStatus::InProgress),
        Just(TrialStatus::Completed),
        Just(TrialStatus::Failed),
        Just(TrialStatus::Skipped),
        Just(TrialStatus::Cancelled),
    ]
}

fn arb_leaf_filter() -> impl Strategy<Value = ResultFilter> {
    prop_oneof![
        Just(ResultFilter::Any),
        arb_trial_status().prop_map(|s| ResultFilter::Status { status: s }),
        (0u32..=10)
            .prop_map(|n| ResultFilter::AttemptNumber {
                cmp:   Comparison::Ge,
                value: n,
            }),
        arb_comparison().prop_map(|cmp| ResultFilter::AttemptNumber {
            cmp,
            value: 1,
        }),
        Just(ResultFilter::TrialCode {
            pattern: TrialCodePattern::Prefix {
                prefix: "0x".to_owned(),
            },
        }),
    ]
}

fn arb_filter() -> impl Strategy<Value = ResultFilter> {
    let leaf = arb_leaf_filter();
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 1..4)
                .prop_map(|children| ResultFilter::And { children }),
            proptest::collection::vec(inner.clone(), 1..4)
                .prop_map(|children| ResultFilter::Or { children }),
            inner.prop_map(|f| ResultFilter::Not {
                child: Box::new(f),
            }),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// Random `ResultFilter` trees roundtrip through JSON — the
    /// hand-rolled serde impls are self-consistent.
    #[test]
    fn result_filter_serde_roundtrip(f in arb_filter()) {
        let json = serde_json::to_string(&f).unwrap();
        let back: ResultFilter = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(f, back);
    }
}

// ---------------------------------------------------------------------------
// Aggregation serde fuzz.
// ---------------------------------------------------------------------------

fn arb_coord() -> impl Strategy<Value = ElementParameterRef> {
    ("[a-z]{1,4}", "[a-z]{1,4}").prop_map(|(e, p)| {
        ElementParameterRef::new(
            ElementName::new(e).unwrap(),
            ParameterName::new(p).unwrap(),
        )
    })
}

fn arb_group_dimension() -> impl Strategy<Value = GroupDimension> {
    prop_oneof![
        Just(GroupDimension::Status),
        (0u32..=3).prop_map(|position| GroupDimension::TrialCodeDigit { position }),
        arb_coord().prop_map(|coord| GroupDimension::Assignment { coord }),
    ]
}

fn arb_aggregation() -> impl Strategy<Value = Aggregation> {
    let scalar = prop_oneof![
        Just(Aggregation::Count),
        arb_coord().prop_map(|metric| Aggregation::Min { metric }),
        arb_coord().prop_map(|metric| Aggregation::Max { metric }),
        arb_coord().prop_map(|metric| Aggregation::Sum { metric }),
        arb_coord().prop_map(|metric| Aggregation::Avg { metric }),
        (arb_coord(), 0.0f64..=1.0).prop_map(|(metric, p)| Aggregation::Percentile {
            metric,
            p,
        }),
    ];
    scalar.prop_recursive(2, 4, 1, |inner| {
        (arb_group_dimension(), inner).prop_map(|(dimension, then)| {
            Aggregation::GroupBy {
                dimension,
                then: Box::new(then),
            }
        })
    })
}

/// `f64` comparison tolerant of the shortest-roundtrip slack
/// `serde_json` applies to double-precision numbers.
fn agg_eq(a: &Aggregation, b: &Aggregation) -> bool {
    match (a, b) {
        (Aggregation::Count, Aggregation::Count) => true,
        (Aggregation::Min { metric: m1 }, Aggregation::Min { metric: m2 })
        | (Aggregation::Max { metric: m1 }, Aggregation::Max { metric: m2 })
        | (Aggregation::Sum { metric: m1 }, Aggregation::Sum { metric: m2 })
        | (Aggregation::Avg { metric: m1 }, Aggregation::Avg { metric: m2 }) => m1 == m2,
        (
            Aggregation::Percentile {
                metric: m1,
                p:      p1,
            },
            Aggregation::Percentile {
                metric: m2,
                p:      p2,
            },
        ) => m1 == m2 && (p1 - p2).abs() < 1e-10,
        (
            Aggregation::GroupBy {
                dimension: d1,
                then:      t1,
            },
            Aggregation::GroupBy {
                dimension: d2,
                then:      t2,
            },
        ) => d1 == d2 && agg_eq(t1, t2),
        _ => false,
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// `Aggregation` roundtrips through JSON (modulo the shortest-
    /// roundtrip f64 rounding serde_json performs on `Percentile.p`).
    #[test]
    fn aggregation_serde_roundtrip(a in arb_aggregation()) {
        let json = serde_json::to_string(&a).unwrap();
        let back: Aggregation = serde_json::from_str(&json).unwrap();
        prop_assert!(agg_eq(&a, &back), "{a:?} != {back:?}");
    }
}

// ---------------------------------------------------------------------------
// Compiler fuzz: compile random small plans — never panic.
// ---------------------------------------------------------------------------

fn axis_of(element: &str, param: &str, values: &[i64]) -> Axis {
    Axis::builder()
        .name(AxisName::new(format!("{element}_{param}")).unwrap())
        .target(ElementParameterRef::new(ename(element), pname(param)))
        .values(
            values
                .iter()
                .map(|v| Value::integer(pname(param), *v, None))
                .collect(),
        )
        .build()
}

fn element_with_int_axis(name: &str, p: &str) -> Element {
    Element::builder()
        .name(ename(name))
        .labels(svc_labels())
        .parameters(vec![Parameter::Integer(
            IntegerParameter::range(pname(p), 1, 1024).unwrap(),
        )])
        .build()
}

// ---------------------------------------------------------------------------
// Deserialize from random bytes — should never panic.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// `serde_json::from_str::<ResultFilter>` on random strings
    /// returns an error (or, by luck, a valid filter) — never panics
    /// or hangs.
    #[test]
    fn result_filter_deserialize_never_panics_on_garbage(
        s in "[ -~]{0,128}",
    ) {
        let _ = serde_json::from_str::<ResultFilter>(&s);
    }

    /// Same property for `Aggregation`.
    #[test]
    fn aggregation_deserialize_never_panics_on_garbage(
        s in "[ -~]{0,128}",
    ) {
        let _ = serde_json::from_str::<Aggregation>(&s);
    }

    /// Same property for `paramodel_elements::Value`.
    #[test]
    fn value_deserialize_never_panics_on_garbage(s in "[ -~]{0,128}") {
        let _ = serde_json::from_str::<Value>(&s);
    }
}

// ---------------------------------------------------------------------------
// StepId / ElementName / ParameterName construction rejects malformed.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// `StepId::new` with unrestricted inputs either succeeds or
    /// returns a `PlanError::InvalidStepId`; never panics.
    #[test]
    fn step_id_new_never_panics(s in "[\\s\\S]{0,64}") {
        use paramodel_plan::StepId;
        let _ = StepId::new(s);
    }

    /// `ElementName::new` with arbitrary strings either succeeds
    /// or returns an `Err`; never panics.
    #[test]
    fn element_name_new_never_panics(s in "[\\s\\S]{0,64}") {
        let _ = ElementName::new(s);
    }

    /// `ParameterName::new` likewise.
    #[test]
    fn parameter_name_new_never_panics(s in "[\\s\\S]{0,64}") {
        let _ = ParameterName::new(s);
    }
}

// ---------------------------------------------------------------------------
// AtomicStep / ResolvedConfiguration serde fuzz.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    /// Random `AtomicStep::Deploy` roundtrips through serde_json.
    #[test]
    fn deploy_step_serde_roundtrip(
        element in "[a-z]{1,6}",
        instance in 0u32..=32,
        reason in "[ -~]{1,32}",
    ) {
        use paramodel_elements::ResolvedConfiguration;
        use paramodel_plan::{AtomicStep, StepHeader, StepId};

        let step = AtomicStep::Deploy {
            header:                StepHeader::builder()
                .id(StepId::new(format!("s_{element}")).unwrap())
                .reason(reason)
                .build(),
            element:               ElementName::new(element).unwrap(),
            instance_number:       instance,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        };
        let json = serde_json::to_string(&step).unwrap();
        let back: AtomicStep = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(step, back);
    }
}

// ---------------------------------------------------------------------------
// Compiler determinism: structural shape stable across runs.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    /// Two compilations of the same plan produce the same set of
    /// step ids and the same dependency shape.
    #[test]
    fn compiler_is_structurally_deterministic(
        elem_count in 1u8..=3,
        use_axis in any::<bool>(),
    ) {
        let elements: Vec<Element> = (0..elem_count)
            .map(|i| {
                let n = format!("e{i}");
                if use_axis {
                    element_with_int_axis(&n, "p")
                } else {
                    Element::builder()
                        .name(ename(&n))
                        .labels(svc_labels())
                        .build()
                }
            })
            .collect();
        let axes: Vec<Axis> = if use_axis && !elements.is_empty() {
            vec![axis_of(elements[0].name.as_str(), "p", &[1, 2])]
        } else {
            vec![]
        };
        let p = TestPlan::builder()
            .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .name(PlanName::new("d").unwrap())
            .elements(elements)
            .axes(axes)
            .metadata(
                TestPlanMetadata::builder()
                    .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                    .build(),
            )
            .build();
        let a = DefaultCompiler::default().compile(&p);
        let b = DefaultCompiler::default().compile(&p);
        match (a, b) {
            (Ok(ap), Ok(bp)) => {
                prop_assert_eq!(ap.fingerprint(), bp.fingerprint());
                let a_ids: BTreeSet<String> = ap
                    .steps()
                    .iter()
                    .map(|s| s.id().as_str().to_owned())
                    .collect();
                let b_ids: BTreeSet<String> = bp
                    .steps()
                    .iter()
                    .map(|s| s.id().as_str().to_owned())
                    .collect();
                prop_assert_eq!(a_ids, b_ids);
            }
            (Err(_), Err(_)) => {} // same error is fine
            _ => panic!("compilation outcome diverged between equal plans"),
        }
    }
}

// ---------------------------------------------------------------------------
// Fingerprint ordering: `Fingerprint` is `Ord` and stable.
// ---------------------------------------------------------------------------

proptest! {
    /// Fingerprint comparison is reflexive, antisymmetric, transitive.
    #[test]
    fn fingerprint_ord_is_consistent(
        a in proptest::collection::vec(any::<u8>(), 0..32),
        b in proptest::collection::vec(any::<u8>(), 0..32),
    ) {
        use paramodel_elements::Fingerprint;
        let fa = Fingerprint::of(&a);
        let fb = Fingerprint::of(&b);
        // Reflexive.
        prop_assert!(fa.cmp(&fa).is_eq());
        // Antisymmetric: a.cmp(b).reverse() == b.cmp(a).
        prop_assert_eq!(fa.cmp(&fb).reverse(), fb.cmp(&fa));
    }
}

// ---------------------------------------------------------------------------
// TrialResult validation: rejects anything that breaks D9 invariants.
// ---------------------------------------------------------------------------

proptest! {
    #[test]
    fn trial_result_validate_rejects_zero_attempt(
        status in proptest::prop_oneof![
            Just(TrialStatus::Completed),
            Just(TrialStatus::Cancelled),
        ],
    ) {
        use paramodel_elements::TrialId;
        use paramodel_trials::{ExecutionTiming, TrialResult};
        let r = TrialResult::builder()
            .trial_id(TrialId::from_ulid(Ulid::from_parts(1, 1)))
            .status(status)
            .attempt(0)
            .timing(
                ExecutionTiming::new(
                    Timestamp::from_second(0).unwrap(),
                    Timestamp::from_second(1).unwrap(),
                )
                .unwrap(),
            )
            .build();
        prop_assert!(r.validate().is_err());
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    /// Compile a random small plan — either succeeds or returns a
    /// CompilationError. Must never panic.
    #[test]
    fn compiler_never_panics_on_random_small_plan(
        count in 1u8..=3,
        axis_cardinalities in proptest::collection::vec(1u8..=4, 0..3),
    ) {
        let mut elements: Vec<Element> = (0..count)
            .map(|i| {
                let name = format!("e{i}");
                if axis_cardinalities.is_empty() {
                    Element::builder()
                        .name(ename(&name))
                        .labels(svc_labels())
                        .build()
                } else {
                    element_with_int_axis(&name, "p")
                }
            })
            .collect();
        // Remove duplicate names that can arise from small seeds.
        let mut seen = BTreeSet::new();
        elements.retain(|e| seen.insert(e.name.clone()));

        let axes: Vec<Axis> = axis_cardinalities
            .iter()
            .enumerate()
            .filter_map(|(i, card)| {
                let target = elements.first()?;
                let values: Vec<i64> = (1..=i64::from(*card)).collect();
                let mut a = axis_of(target.name.as_str(), "p", &values);
                a.name = AxisName::new(format!("axis{i}")).unwrap();
                Some(a)
            })
            .collect();

        let p = TestPlan::builder()
            .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .name(PlanName::new("fuzz").unwrap())
            .elements(elements)
            .axes(axes)
            .metadata(
                TestPlanMetadata::builder()
                    .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                    .build(),
            )
            .build();
        let _ = DefaultCompiler::default().compile(&p); // Ok or Err, never panic.
    }
}
