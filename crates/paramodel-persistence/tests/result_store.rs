// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `ResultFilter`, `Aggregation`,
//! `AggregateResult`, and the supporting leaf types.

use paramodel_elements::{ParameterName, Value};
use paramodel_persistence::{
    AggregateResult, Aggregation, Comparison, GroupDimension, ResultFilter,
    TrialCodePattern,
};
use paramodel_trials::TrialStatus;

#[test]
fn filter_combinators_nest_correctly() {
    let f = ResultFilter::Status {
        status: TrialStatus::Completed,
    }
    .and(ResultFilter::AttemptNumber {
        cmp:   Comparison::Eq,
        value: 1,
    })
    .or(ResultFilter::Any.negate());
    assert!(matches!(f, ResultFilter::Or { .. }));
}

#[test]
fn trial_code_pattern_serde_roundtrip() {
    let p = TrialCodePattern::Prefix {
        prefix: "0x00".to_owned(),
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: TrialCodePattern = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn aggregation_group_by_constructs() {
    let a = Aggregation::GroupBy {
        dimension: GroupDimension::Status,
        then:      Box::new(Aggregation::Count),
    };
    assert!(matches!(a, Aggregation::GroupBy { .. }));
}

#[test]
fn result_filter_serde_roundtrip() {
    let f = ResultFilter::Status {
        status: TrialStatus::Completed,
    }
    .and(ResultFilter::AttemptNumber {
        cmp:   Comparison::Ge,
        value: 2,
    })
    .negate();
    let json = serde_json::to_string(&f).expect("serialise");
    let back: ResultFilter = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(f, back);
}

#[test]
fn aggregation_serde_roundtrip() {
    let a = Aggregation::GroupBy {
        dimension: GroupDimension::Status,
        then:      Box::new(Aggregation::Count),
    };
    let json = serde_json::to_string(&a).expect("serialise");
    let back: Aggregation = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(a, back);
}

#[test]
fn aggregate_result_serde_roundtrip() {
    let pname = ParameterName::new("latency_ms").unwrap();
    let r = AggregateResult::Grouped {
        groups: vec![
            (
                Value::string(
                    ParameterName::new("status").unwrap(),
                    "completed".to_owned(),
                    None,
                ),
                Box::new(AggregateResult::Count { n: 3 }),
            ),
            (
                Value::string(
                    ParameterName::new("status").unwrap(),
                    "failed".to_owned(),
                    None,
                ),
                Box::new(AggregateResult::Scalar {
                    value: Value::integer(pname, 42, None),
                }),
            ),
        ],
    };
    let json = serde_json::to_string(&r).expect("serialise");
    let back: AggregateResult = serde_json::from_str(&json).expect("deserialise");
    // `Value` provenance carries a generation timestamp — equality
    // via derive includes it, so compare the serialized JSON instead
    // (the stable shape).
    let back_json = serde_json::to_string(&back).expect("re-serialise");
    assert_eq!(json, back_json);
}

