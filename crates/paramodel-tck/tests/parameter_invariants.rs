// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Negative-path + invariant coverage for the parameter / domain /
//! value algebra. Targets SRD-0004 shape checks.

use paramodel_elements::{
    BooleanParameter, DoubleParameter, IntegerParameter, ParameterName,
    SelectionItem, SelectionParameter, StringParameter, Value,
};

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

#[test]
fn integer_range_min_equal_max_is_singleton() {
    let p = IntegerParameter::range(pname("v"), 7, 7).unwrap();
    assert!(p.domain.contains_native(7));
    assert!(!p.domain.contains_native(6));
    assert!(!p.domain.contains_native(8));
}

#[test]
fn integer_of_rejects_empty_set() {
    let empty: std::collections::BTreeSet<i64> = std::collections::BTreeSet::new();
    let err = IntegerParameter::of(pname("v"), empty).unwrap_err();
    let _ = err;
}

#[test]
fn double_range_construction_accepts_nonsense_ordering_if_permitted() {
    // min > max must be rejected; min == max must be accepted.
    assert!(DoubleParameter::range(pname("v"), 1.0, 0.0).is_err());
    assert!(DoubleParameter::range(pname("v"), 1.0, 1.0).is_ok());
}

#[test]
fn boolean_parameter_construction_succeeds() {
    // BooleanParameter::of is infallible and doesn't expose a
    // domain field — the name and trivial membership are enough.
    let p = BooleanParameter::of(pname("v"));
    assert_eq!(p.name.as_str(), "v");
}

#[test]
fn string_regex_rejects_invalid_pattern() {
    let err = StringParameter::regex(pname("v"), "[unclosed").unwrap_err();
    let _ = err;
}

#[test]
fn selection_parameter_with_empty_set_rejected() {
    let empty = indexmap::IndexSet::new();
    let err = SelectionParameter::of(pname("v"), empty, 1);
    assert!(err.is_err());
}

#[test]
fn selection_parameter_rejects_zero_max_selections() {
    let mut set = indexmap::IndexSet::new();
    set.insert(SelectionItem::new("a").unwrap());
    // `max_selections == 0` is nonsensical — no selection can satisfy
    // the parameter, so the constructor must reject it.
    let err = SelectionParameter::of(pname("v"), set, 0);
    assert!(err.is_err());
}

#[test]
fn value_integer_and_double_are_distinguishable_by_kind() {
    let i = Value::integer(pname("v"), 42, None);
    let d = Value::double(pname("v"), 42.0, None);
    assert_ne!(i.kind(), d.kind());
}

#[test]
fn value_boolean_kind_is_boolean() {
    use paramodel_elements::ValueKind;
    let b = Value::boolean(pname("v"), true, None);
    assert_eq!(b.kind(), ValueKind::Boolean);
}
