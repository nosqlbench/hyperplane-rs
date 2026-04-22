// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Element::validate` coverage against strict type-descriptor
//! registries. Exercises every error variant the validator can
//! raise: missing type label, unknown type id, missing required
//! label, forbidden label, duplicate parameter name, unknown
//! configuration parameter, zero `max_concurrency`.

use std::collections::{BTreeMap, BTreeSet};

use paramodel_elements::{
    ConfigEntry, Element, ElementError, ElementName, ElementTypeDescriptor,
    ElementTypeDescriptorRegistry, IntegerParameter, LabelKey, LabelValue, Labels,
    OpenRegistry, Parameter, ParameterName, TypeId, Value, attributes::label,
};

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

#[derive(Debug)]
struct StrictRegistry {
    descriptors: Vec<ElementTypeDescriptor>,
}

impl ElementTypeDescriptorRegistry for StrictRegistry {
    fn descriptors(&self) -> Vec<ElementTypeDescriptor> {
        self.descriptors.clone()
    }
}

fn registry_with_service_requiring_name() -> StrictRegistry {
    let mut required = BTreeSet::new();
    required.insert(LabelKey::new("name").unwrap());
    StrictRegistry {
        descriptors: vec![
            ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .required_labels(required)
                .build(),
        ],
    }
}

fn registry_with_service_forbidding_internal() -> StrictRegistry {
    let mut forbidden = BTreeMap::new();
    forbidden.insert(
        LabelKey::new("internal").unwrap(),
        "internal reserved".to_owned(),
    );
    StrictRegistry {
        descriptors: vec![
            ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .forbidden_labels(forbidden)
                .build(),
        ],
    }
}

// ---------------------------------------------------------------------------

#[test]
fn missing_type_label_is_rejected() {
    let e = Element::builder()
        .name(ename("e"))
        .labels(Labels::new())
        .build();
    let err = e.validate(&OpenRegistry::new()).unwrap_err();
    assert!(matches!(err, ElementError::MissingTypeLabel));
}

#[test]
fn unknown_type_id_is_rejected_under_strict_registry() {
    let reg = StrictRegistry {
        descriptors: vec![
            ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .build(),
        ],
    };
    let mut labels = Labels::new();
    labels.insert(label::r#type(), LabelValue::new("mystery").unwrap());
    let e = Element::builder().name(ename("e")).labels(labels).build();
    let err = e.validate(&reg).unwrap_err();
    assert!(matches!(err, ElementError::UnknownElementType { .. }));
}

#[test]
fn missing_required_label_is_rejected() {
    let reg = registry_with_service_requiring_name();
    let e = Element::builder()
        .name(ename("e"))
        .labels(svc_labels())
        .build();
    let err = e.validate(&reg).unwrap_err();
    assert!(matches!(err, ElementError::MissingRequiredLabel { .. }));
}

#[test]
fn required_label_present_validates() {
    let reg = registry_with_service_requiring_name();
    let mut labels = svc_labels();
    labels.insert(
        LabelKey::new("name").unwrap(),
        LabelValue::new("db").unwrap(),
    );
    let e = Element::builder().name(ename("e")).labels(labels).build();
    assert!(e.validate(&reg).is_ok());
}

#[test]
fn forbidden_label_present_is_rejected() {
    let reg = registry_with_service_forbidding_internal();
    let mut labels = svc_labels();
    labels.insert(
        LabelKey::new("internal").unwrap(),
        LabelValue::new("yes").unwrap(),
    );
    let e = Element::builder().name(ename("e")).labels(labels).build();
    let err = e.validate(&reg).unwrap_err();
    assert!(matches!(err, ElementError::ForbiddenLabelPresent { .. }));
}

// ---------------------------------------------------------------------------
// Parameter-name uniqueness.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_parameter_name_is_rejected() {
    let p1 = Parameter::Integer(IntegerParameter::range(pname("x"), 1, 10).unwrap());
    let p2 = Parameter::Integer(IntegerParameter::range(pname("x"), 1, 20).unwrap());
    let e = Element::builder()
        .name(ename("e"))
        .labels(svc_labels())
        .parameters(vec![p1, p2])
        .build();
    let err = e.validate(&OpenRegistry::new()).unwrap_err();
    assert!(matches!(err, ElementError::DuplicateParameterName { .. }));
}

#[test]
fn unknown_configuration_parameter_is_rejected() {
    // Configuration references a parameter name that isn't declared
    // on the element.
    let mut element = Element::builder()
        .name(ename("e"))
        .labels(svc_labels())
        .build();
    element.configuration.insert(
        pname("ghost"),
        ConfigEntry::literal(Value::integer(pname("ghost"), 1, None)),
    );
    let err = element.validate(&OpenRegistry::new()).unwrap_err();
    assert!(matches!(
        err,
        ElementError::UnknownConfigurationParameter { .. }
    ));
}

// ---------------------------------------------------------------------------
// Concurrency.
// ---------------------------------------------------------------------------

#[test]
fn max_concurrency_zero_is_rejected() {
    let mut element = Element::builder()
        .name(ename("e"))
        .labels(svc_labels())
        .build();
    element.max_concurrency = Some(0);
    let err = element.validate(&OpenRegistry::new()).unwrap_err();
    assert!(matches!(err, ElementError::InvalidMaxConcurrency));
}

#[test]
fn max_group_exceeds_global_is_rejected() {
    let mut element = Element::builder()
        .name(ename("e"))
        .labels(svc_labels())
        .build();
    element.max_concurrency = Some(2);
    element.max_group_concurrency = Some(4);
    let err = element.validate(&OpenRegistry::new()).unwrap_err();
    assert!(matches!(
        err,
        ElementError::GroupConcurrencyExceedsGlobal { .. }
    ));
}

// ---------------------------------------------------------------------------
// Happy path: bare service element validates against OpenRegistry.
// ---------------------------------------------------------------------------

#[test]
fn minimal_service_element_validates() {
    let e = Element::builder()
        .name(ename("db"))
        .labels(svc_labels())
        .build();
    assert!(e.validate(&OpenRegistry::new()).is_ok());
}
