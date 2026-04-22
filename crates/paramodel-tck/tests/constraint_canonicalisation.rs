// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::items_after_statements,
    reason = "nested `use` inside test for locality; fine"
)]

//! Constraint canonicalisation properties.
//!
//! `Constraint::canonicalize` is idempotent and produces the same
//! canonical bytes / fingerprint for logically-equivalent
//! constraints regardless of authoring order.

use paramodel_elements::{Constraint, IntConstraint};

const fn c(int: IntConstraint) -> Constraint {
    Constraint::Integer(int)
}

// ---------------------------------------------------------------------------

#[test]
fn canonicalise_is_idempotent() {
    let once = c(IntConstraint::And {
        children: vec![
            IntConstraint::Min { n: 1 },
            IntConstraint::Max { n: 10 },
        ],
    })
    .canonicalize();
    let twice = once.clone().canonicalize();
    assert_eq!(once.canonical_bytes(), twice.canonical_bytes());
    assert_eq!(once.fingerprint(), twice.fingerprint());
}

#[test]
fn canonicalise_deduplicates_equal_and_children() {
    // And[a, a] canonicalises equal to And[a].
    let dup = c(IntConstraint::And {
        children: vec![
            IntConstraint::Min { n: 1 },
            IntConstraint::Min { n: 1 },
        ],
    })
    .canonicalize();
    let single = c(IntConstraint::And {
        children: vec![IntConstraint::Min { n: 1 }],
    })
    .canonicalize();
    assert_eq!(dup.canonical_bytes(), single.canonical_bytes());
}

#[test]
fn canonicalise_preserves_semantics_on_min_max_bracket() {
    // Min{1} AND Max{10} at probe 5 → true; at probe 100 → false.
    let bracket = c(IntConstraint::And {
        children: vec![
            IntConstraint::Min { n: 1 },
            IntConstraint::Max { n: 10 },
        ],
    });
    let canon = bracket.clone().canonicalize();
    use paramodel_elements::{ParameterName, Value};
    let pname = ParameterName::new("v").unwrap();
    for n in &[-1i64, 0, 1, 5, 10, 11, 100] {
        let v = Value::integer(pname.clone(), *n, None);
        assert_eq!(bracket.test(&v), canon.test(&v), "probe {n}");
    }
}

#[test]
fn fingerprint_independent_of_authoring_order_for_commutative_and() {
    // And[Min(1), Max(10)] vs And[Max(10), Min(1)] canonicalise to
    // the same fingerprint — order doesn't matter inside And.
    let a = c(IntConstraint::And {
        children: vec![
            IntConstraint::Min { n: 1 },
            IntConstraint::Max { n: 10 },
        ],
    })
    .canonicalize();
    let b = c(IntConstraint::And {
        children: vec![
            IntConstraint::Max { n: 10 },
            IntConstraint::Min { n: 1 },
        ],
    })
    .canonicalize();
    assert_eq!(a.fingerprint(), b.fingerprint());
}

#[test]
fn fingerprint_independent_of_authoring_order_for_commutative_or() {
    let a = c(IntConstraint::Or {
        children: vec![
            IntConstraint::Min { n: 5 },
            IntConstraint::Max { n: 2 },
        ],
    })
    .canonicalize();
    let b = c(IntConstraint::Or {
        children: vec![
            IntConstraint::Max { n: 2 },
            IntConstraint::Min { n: 5 },
        ],
    })
    .canonicalize();
    assert_eq!(a.fingerprint(), b.fingerprint());
}
