// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `MixedRadixEnumerator` invariants — trial-count, stride, digit
//! width, and trial-code format.

use paramodel_compiler::MixedRadixEnumerator;
use paramodel_elements::{ElementName, ParameterName, Value};
use paramodel_plan::{Axis, AxisName, ElementParameterRef};

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

fn axis_with_card(name: &str, cardinality: u32) -> Axis {
    Axis::builder()
        .name(AxisName::new(name).unwrap())
        .target(ElementParameterRef::new(
            ElementName::new("e").unwrap(),
            pname("p"),
        ))
        .values(
            (0..cardinality)
                .map(|i| Value::integer(pname("p"), i64::from(i), None))
                .collect(),
        )
        .build()
}

// ---------------------------------------------------------------------------

#[test]
fn zero_axes_yields_single_trial() {
    let e = MixedRadixEnumerator::new(&[]);
    assert_eq!(e.trial_count(), 1);
    assert_eq!(e.axis_count(), 0);
}

#[test]
fn trial_count_is_product_of_cardinalities() {
    let axes = vec![
        axis_with_card("a", 2),
        axis_with_card("b", 3),
        axis_with_card("c", 4),
    ];
    let e = MixedRadixEnumerator::new(&axes);
    assert_eq!(e.trial_count(), 2 * 3 * 4);
}

#[test]
fn digit_width_is_4_bits_when_max_cardinality_small() {
    // All cardinalities ≤ 16 → 4-bit digits.
    let axes = vec![axis_with_card("a", 16), axis_with_card("b", 2)];
    let e = MixedRadixEnumerator::new(&axes);
    assert_eq!(e.digit_bits(), 4);
}

#[test]
fn digit_width_switches_to_8_bits_above_threshold() {
    let axes = vec![axis_with_card("a", 32)];
    let e = MixedRadixEnumerator::new(&axes);
    assert_eq!(e.digit_bits(), 8);
}

#[test]
fn trial_code_format_matches_digit_width() {
    // 4-bit: two single-char hex digits.
    let axes = vec![axis_with_card("a", 3), axis_with_card("b", 5)];
    let e = MixedRadixEnumerator::new(&axes);
    let code0 = e.trial_code(0);
    assert_eq!(code0, "0x00");
    let code_last = e.trial_code(e.trial_count() - 1);
    // Two characters.
    assert!(code_last.starts_with("0x"));
    assert_eq!(code_last.len(), 4);
}

#[test]
fn trial_code_8bit_format_has_two_chars_per_digit() {
    let axes = vec![axis_with_card("a", 32)];
    let e = MixedRadixEnumerator::new(&axes);
    let code = e.trial_code(17);
    // 0x<2 hex chars per digit><...>
    assert!(code.starts_with("0x"));
    assert_eq!(code.len(), 2 + 2, "one digit at 8 bits → 2 hex chars");
}

#[test]
fn trial_index_offsets_roundtrip_for_every_trial() {
    let axes = vec![
        axis_with_card("a", 2),
        axis_with_card("b", 5),
        axis_with_card("c", 3),
    ];
    let e = MixedRadixEnumerator::new(&axes);
    for t in 0..e.trial_count() {
        let offsets = e.offsets(t);
        assert_eq!(e.trial_index(&offsets), t);
    }
}

#[test]
fn offsets_each_bounded_by_its_axis_cardinality() {
    let axes = vec![axis_with_card("a", 4), axis_with_card("b", 7)];
    let e = MixedRadixEnumerator::new(&axes);
    for t in 0..e.trial_count() {
        let offsets = e.offsets(t);
        assert!(offsets[0] < 4);
        assert!(offsets[1] < 7);
    }
}
