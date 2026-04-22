// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! The `Element` struct — anchor type of the whole system.
//!
//! `Element` is the declarative shape that flows through plans,
//! compilation, and persistence. Behaviour (materialize, dematerialize,
//! observe state) lives in the [`ElementRuntime`](crate::ElementRuntime)
//! trait and is implemented in the hyperplane tier.
//!
//! Construction uses [`bon::Builder`]: callers assemble field-by-field
//! and call `.build()`, then run [`Element::validate`] against an
//! [`ElementTypeDescriptorRegistry`] to enforce SRD-0007's invariants
//! (type label in registry, unique parameter names, config keys
//! reference real parameters, namespace uniqueness across tiers,
//! concurrency caps).

use std::collections::BTreeSet;

use crate::{
    Attributed, ElementName, Labels, Parameter, Plug, Pluggable, Socket, Tags,
    attributes::{label, validate_namespace},
};
use serde::{Deserialize, Serialize};

use crate::configuration::{Configuration, Exports};
use crate::dependency::Dependency;
use crate::error::ElementError;
use crate::lifecycle::{HealthCheckSpec, ShutdownSemantics};
use crate::types::{ElementTypeDescriptorRegistry, TypeId};

/// One element in the Element Graph.
///
/// Every field is serialisable so the struct round-trips through plan
/// storage and the wire unchanged. Run [`Self::validate`] after
/// construction — the builder assembles the shape, validation enforces
/// the cross-cutting invariants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct Element {
    /// Unique identifier within a test plan.
    pub name: ElementName,

    /// Intrinsic facts. Must contain a `type` entry whose value is
    /// registered in the type descriptor registry.
    #[builder(default)]
    pub labels: Labels,

    /// Extrinsic organisation (owner, priority, environment, …).
    #[builder(default)]
    pub tags: Tags,

    /// Points where this element needs upstream connections.
    #[builder(default)]
    pub plugs: Vec<Plug>,

    /// Points where downstream elements can connect to this one.
    #[builder(default)]
    pub sockets: Vec<Socket>,

    /// Configurable input dimensions.
    #[builder(default)]
    pub parameters: Vec<Parameter>,

    /// Typed output dimensions this element publishes after
    /// materialization.
    #[builder(default)]
    pub result_parameters: Vec<Parameter>,

    /// Authored parameter bindings (literal values or token
    /// references). Axis bindings override these per trial; see
    /// SRD-0007 D21.
    #[builder(default)]
    pub configuration: Configuration,

    /// Named values this element publishes to downstreams.
    #[builder(default)]
    pub exports: Exports,

    /// Typed dependency edges — the Element Graph edges.
    #[builder(default)]
    pub dependencies: Vec<Dependency>,

    /// Readiness-check timing. `None` → ready immediately after
    /// starting.
    pub health_check: Option<HealthCheckSpec>,

    /// How this element terminates.
    #[builder(default)]
    pub shutdown_semantics: ShutdownSemantics,

    /// Explicit trial-element override. `None` → auto-detect via
    /// reducto's leaf-node heuristic.
    pub trial_element: Option<bool>,

    /// Max active instances of this element globally.
    pub max_concurrency: Option<u32>,

    /// Max active instances within one coalesced group.
    pub max_group_concurrency: Option<u32>,
}

impl Element {
    /// Validate the element against a type-descriptor registry.
    ///
    /// Enforces SRD-0007 invariants: `type` label is registered,
    /// required / forbidden labels match the descriptor, parameter
    /// names are unique in each list, configuration keys reference
    /// real parameters, namespace uniqueness across labels / tags /
    /// ports / parameter names, and concurrency caps are sane.
    pub fn validate(
        &self,
        registry: &dyn ElementTypeDescriptorRegistry,
    ) -> Result<(), ElementError> {
        // 1. Type label → descriptor lookup.
        let type_key = label::r#type();
        let type_value = self
            .labels
            .get(&type_key)
            .ok_or(ElementError::MissingTypeLabel)?;
        let type_id = TypeId::new(type_value.as_str())?;
        let descriptor = registry
            .descriptor(&type_id)
            .ok_or_else(|| ElementError::UnknownElementType {
                type_id: type_value.as_str().to_owned(),
            })?;

        // 2. Descriptor-driven label requirements.
        for required in &descriptor.required_labels {
            if !self.labels.contains_key(required) {
                return Err(ElementError::MissingRequiredLabel {
                    key: required.as_str().to_owned(),
                });
            }
        }
        for (forbidden, reason) in &descriptor.forbidden_labels {
            if self.labels.contains_key(forbidden) {
                return Err(ElementError::ForbiddenLabelPresent {
                    key:    forbidden.as_str().to_owned(),
                    reason: reason.clone(),
                });
            }
        }

        // 3. Parameter-name uniqueness within each list.
        let mut seen = BTreeSet::new();
        for p in &self.parameters {
            if !seen.insert(p.name().as_str()) {
                return Err(ElementError::DuplicateParameterName {
                    name: p.name().as_str().to_owned(),
                });
            }
        }
        let mut seen_results = BTreeSet::new();
        for p in &self.result_parameters {
            if !seen_results.insert(p.name().as_str()) {
                return Err(ElementError::DuplicateResultParameterName {
                    name: p.name().as_str().to_owned(),
                });
            }
        }

        // 4. Configuration keys reference declared parameters.
        let param_names: BTreeSet<&str> =
            self.parameters.iter().map(|p| p.name().as_str()).collect();
        for key in self.configuration.keys() {
            if !param_names.contains(key.as_str()) {
                return Err(ElementError::UnknownConfigurationParameter {
                    name: key.as_str().to_owned(),
                });
            }
        }

        // 5. Namespace uniqueness across labels / tags / plugs / sockets.
        validate_namespace(&self.labels, &self.tags, &self.plugs, &self.sockets)?;

        // 6. Parameter names must not collide with any attribute /
        //    port name on the same element (SRD-0005 D5 extended to
        //    parameters per the Element invariants).
        let mut attribute_keys: BTreeSet<&str> = BTreeSet::new();
        for k in self.labels.keys() {
            attribute_keys.insert(k.as_str());
        }
        for k in self.tags.keys() {
            attribute_keys.insert(k.as_str());
        }
        for p in &self.plugs {
            attribute_keys.insert(p.name.as_str());
        }
        for s in &self.sockets {
            attribute_keys.insert(s.name.as_str());
        }
        for p in &self.parameters {
            if attribute_keys.contains(p.name().as_str()) {
                return Err(ElementError::ParameterNameCollidesWithAttribute {
                    name: p.name().as_str().to_owned(),
                });
            }
        }

        // 7. Concurrency caps.
        if let Some(mc) = self.max_concurrency
            && mc < 1
        {
            return Err(ElementError::InvalidMaxConcurrency);
        }
        if let (Some(group), Some(global)) =
            (self.max_group_concurrency, self.max_concurrency)
            && group > global
        {
            return Err(ElementError::GroupConcurrencyExceedsGlobal { group, global });
        }

        Ok(())
    }
}

impl Attributed for Element {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Pluggable for Element {
    fn plugs(&self) -> &[Plug] {
        &self.plugs
    }
    fn sockets(&self) -> &[Socket] {
        &self.sockets
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::{
        Facet, IntConstraint, IntegerParameter, LabelKey, LabelValue, ParameterName,
        PortName, TagKey, TagValue, Tier,
    };

    use super::*;
    use crate::configuration::{ConfigEntry, ExportName, TokenExpr};
    use crate::types::{ElementTypeDescriptor, OpenRegistry};

    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn lk(s: &str) -> LabelKey {
        LabelKey::new(s).unwrap()
    }
    fn lv(s: &str) -> LabelValue {
        LabelValue::new(s).unwrap()
    }

    fn element_with_type(name: &str, type_value: &str) -> Element {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv(type_value));
        Element::builder()
            .name(ename(name))
            .labels(labels)
            .build()
    }

    // ---------- Type label ----------

    #[test]
    fn validate_requires_type_label() {
        let e = Element::builder().name(ename("svc")).build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::MissingTypeLabel)
        ));
    }

    #[test]
    fn open_registry_accepts_any_type_value() {
        let e = element_with_type("svc", "whatever");
        let reg = OpenRegistry::new();
        assert!(e.validate(&reg).is_ok());
    }

    #[derive(Debug)]
    struct StrictRegistry {
        types: Vec<ElementTypeDescriptor>,
    }

    impl ElementTypeDescriptorRegistry for StrictRegistry {
        fn descriptors(&self) -> Vec<ElementTypeDescriptor> {
            self.types.clone()
        }
    }

    #[test]
    fn strict_registry_rejects_unknown_type() {
        let reg = StrictRegistry {
            types: vec![ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .build()],
        };
        let e = element_with_type("svc", "node");
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::UnknownElementType { .. })
        ));
    }

    // ---------- Required / forbidden labels ----------

    #[test]
    fn descriptor_required_labels_are_enforced() {
        let reg = StrictRegistry {
            types: vec![ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .required_labels({
                    let mut s = BTreeSet::new();
                    s.insert(lk("owner"));
                    s
                })
                .build()],
        };
        let e = element_with_type("svc", "service");
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::MissingRequiredLabel { .. })
        ));
    }

    #[test]
    fn descriptor_forbidden_labels_are_enforced() {
        let reg = StrictRegistry {
            types: vec![ElementTypeDescriptor::builder()
                .type_id(TypeId::new("service").unwrap())
                .forbidden_labels({
                    let mut m = std::collections::BTreeMap::new();
                    m.insert(lk("legacy"), "deprecated".to_owned());
                    m
                })
                .build()],
        };
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        labels.insert(lk("legacy"), lv("1"));
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .build();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::ForbiddenLabelPresent { .. })
        ));
    }

    // ---------- Parameter uniqueness ----------

    #[test]
    fn duplicate_parameter_names_rejected() {
        let p = Parameter::Integer(IntegerParameter::range(pname("n"), 1, 10).unwrap());
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .parameters(vec![p.clone(), p])
            .build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::DuplicateParameterName { .. })
        ));
    }

    // ---------- Configuration validates against parameter list ----------

    #[test]
    fn configuration_keys_must_reference_declared_parameters() {
        let p = Parameter::Integer(IntegerParameter::range(pname("n"), 1, 10).unwrap());
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        let mut cfg = Configuration::new();
        // Orphan reference — no `ghost` parameter declared.
        cfg.insert(
            pname("ghost"),
            ConfigEntry::literal(crate::Value::integer(pname("ghost"), 1, None)),
        );
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .parameters(vec![p])
            .configuration(cfg)
            .build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::UnknownConfigurationParameter { .. })
        ));
    }

    // ---------- Namespace collisions ----------

    #[test]
    fn parameter_name_colliding_with_label_is_rejected() {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        labels.insert(lk("threads"), lv("collides"));
        let p = Parameter::Integer(IntegerParameter::range(pname("threads"), 1, 10).unwrap());
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .parameters(vec![p])
            .build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::ParameterNameCollidesWithAttribute { .. })
        ));
    }

    #[test]
    fn cross_tier_duplicate_key_is_rejected() {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        labels.insert(lk("owner"), lv("ops"));
        let mut tags = Tags::new();
        tags.insert(TagKey::new("owner").unwrap(), TagValue::new("bench").unwrap());
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .tags(tags)
            .build();
        let reg = OpenRegistry::new();
        match e.validate(&reg) {
            Err(ElementError::Attribute(crate::AttributeError::DuplicateKey {
                tiers,
                ..
            })) => {
                assert!(tiers.contains(&Tier::Label));
                assert!(tiers.contains(&Tier::Tag));
            }
            other => panic!("expected cross-tier duplicate, got {other:?}"),
        }
    }

    // ---------- Concurrency caps ----------

    #[test]
    fn zero_max_concurrency_is_rejected() {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .max_concurrency(0)
            .build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::InvalidMaxConcurrency)
        ));
    }

    #[test]
    fn group_concurrency_exceeding_global_is_rejected() {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        let e = Element::builder()
            .name(ename("svc"))
            .labels(labels)
            .max_concurrency(4)
            .max_group_concurrency(8)
            .build();
        let reg = OpenRegistry::new();
        assert!(matches!(
            e.validate(&reg),
            Err(ElementError::GroupConcurrencyExceedsGlobal { .. })
        ));
    }

    // ---------- Happy-path construction ----------

    #[test]
    fn full_element_builds_and_validates() {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), lv("service"));
        labels.insert(lk("owner"), lv("bench"));

        let mut tags = Tags::new();
        tags.insert(TagKey::new("env").unwrap(), TagValue::new("staging").unwrap());

        let plug = Plug::new(
            PortName::new("upstream").unwrap(),
            {
                let mut s = BTreeSet::new();
                s.insert(Facet::new("kind", "database").unwrap());
                s
            },
        )
        .unwrap();

        let param = Parameter::Integer(
            IntegerParameter::range(pname("threads"), 1, 64)
                .unwrap()
                .with_constraint(IntConstraint::Min { n: 1 })
                .with_default(8)
                .unwrap(),
        );

        let mut cfg = Configuration::new();
        cfg.insert(
            pname("threads"),
            ConfigEntry::literal(crate::Value::integer(pname("threads"), 16, None)),
        );

        let mut exports = Exports::new();
        exports.insert(
            ExportName::new("endpoint").unwrap(),
            TokenExpr::new("${self.ip}:4567").unwrap(),
        );

        let e = Element::builder()
            .name(ename("harness"))
            .labels(labels)
            .tags(tags)
            .plugs(vec![plug])
            .parameters(vec![param])
            .configuration(cfg)
            .exports(exports)
            .dependencies(vec![Dependency::shared(ename("db"))])
            .shutdown_semantics(ShutdownSemantics::Service)
            .max_concurrency(8)
            .build();

        let reg = OpenRegistry::new();
        assert!(e.validate(&reg).is_ok());
    }

    // ---------- Attributed / Pluggable traits ----------

    #[test]
    fn attributed_and_pluggable_read_through() {
        let e = element_with_type("svc", "service");
        assert_eq!(<Element as Attributed>::labels(&e).len(), 1);
        assert!(<Element as Pluggable>::plugs(&e).is_empty());
    }

    // ---------- serde ----------

    #[test]
    fn element_serde_roundtrip() {
        let e = element_with_type("svc", "service");
        let json = serde_json::to_string(&e).unwrap();
        let back: Element = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
