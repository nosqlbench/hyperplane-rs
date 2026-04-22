// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Element type descriptors and the registry that dispatches by type.
//!
//! Per SRD-0007 D15, `ElementTypeDescriptor` carries the minimal
//! metadata needed to validate element construction: which labels are
//! required, forbidden, or warning-worthy for elements of this type,
//! plus the infrastructure flag the planner uses.
//!
//! `ElementTypeDescriptorRegistry` is the host-provided trait;
//! embedding systems (hyperplane and peers) implement it to expose
//! their type catalogues. [`OpenRegistry`] is a permissive default that
//! accepts any type id — useful in tests.

use std::collections::{BTreeMap, BTreeSet};

use crate::{LabelKey, name_type};

name_type! {
    /// Canonical identifier for an element type (e.g. `"service"`,
    /// `"command"`, `"node"`).
    pub struct TypeId { kind: "TypeId" }
}

/// Metadata the compiler uses to validate an element of this type.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ElementTypeDescriptor {
    /// Canonical type id.
    pub type_id: TypeId,

    /// Labels that elements of this type *must* carry.
    #[builder(default)]
    pub required_labels: BTreeSet<LabelKey>,

    /// Labels that are forbidden on elements of this type, keyed by
    /// label with a message explaining why.
    #[builder(default)]
    pub forbidden_labels: BTreeMap<LabelKey, String>,

    /// Labels whose presence triggers a warning (not an error).
    #[builder(default)]
    pub label_warnings: BTreeMap<LabelKey, String>,

    /// `true` if this type represents infrastructure (a node).
    #[builder(default)]
    pub provides_infrastructure: bool,
}

/// Host-provided catalogue of element-type descriptors.
///
/// Only place in the elements layer where `dyn Trait` appears; it's an
/// embedding-system service, not part of the element algebra itself.
pub trait ElementTypeDescriptorRegistry: Send + Sync + std::fmt::Debug + 'static {
    /// Every descriptor the registry knows about.
    fn descriptors(&self) -> Vec<ElementTypeDescriptor>;

    /// Type aliases that resolve to canonical type ids. Default empty.
    fn type_aliases(&self) -> BTreeMap<TypeId, TypeId> {
        BTreeMap::new()
    }

    /// Set of valid canonical type ids (derived from [`Self::descriptors`]).
    fn valid_type_ids(&self) -> BTreeSet<TypeId> {
        self.descriptors()
            .into_iter()
            .map(|d| d.type_id)
            .collect()
    }

    /// Look up one descriptor by canonical type id.
    fn descriptor(&self, type_id: &TypeId) -> Option<ElementTypeDescriptor> {
        self.descriptors()
            .into_iter()
            .find(|d| &d.type_id == type_id)
    }

    /// `true` when any registered descriptor carries
    /// `provides_infrastructure`.
    fn has_infrastructure_type(&self) -> bool {
        self.descriptors()
            .iter()
            .any(|d| d.provides_infrastructure)
    }
}

/// Permissive default registry — accepts any type id as valid.
///
/// Intended for tests and mock hosts. Real hosts supply their own
/// implementation that catalogues the types they can materialise.
#[derive(Debug, Default)]
pub struct OpenRegistry;

impl OpenRegistry {
    /// Construct a new open registry.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ElementTypeDescriptorRegistry for OpenRegistry {
    fn descriptors(&self) -> Vec<ElementTypeDescriptor> {
        // The open registry reports no descriptors. `descriptor` below
        // is overridden to synthesise one on demand so any incoming
        // type id validates.
        Vec::new()
    }

    fn valid_type_ids(&self) -> BTreeSet<TypeId> {
        BTreeSet::new()
    }

    fn descriptor(&self, type_id: &TypeId) -> Option<ElementTypeDescriptor> {
        Some(
            ElementTypeDescriptor::builder()
                .type_id(type_id.clone())
                .build(),
        )
    }

    fn has_infrastructure_type(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_id_validates() {
        TypeId::new("service").unwrap();
        assert!(TypeId::new("").is_err());
    }

    #[test]
    fn descriptor_builder_default_fields() {
        let d = ElementTypeDescriptor::builder()
            .type_id(TypeId::new("service").unwrap())
            .build();
        assert!(d.required_labels.is_empty());
        assert!(d.forbidden_labels.is_empty());
        assert!(d.label_warnings.is_empty());
        assert!(!d.provides_infrastructure);
    }

    #[test]
    fn open_registry_accepts_any_type() {
        let r = OpenRegistry::new();
        let id = TypeId::new("whatever").unwrap();
        let d = r.descriptor(&id).unwrap();
        assert_eq!(d.type_id, id);
        assert!(!r.has_infrastructure_type());
    }

    #[derive(Debug)]
    struct FixedRegistry {
        descriptors: Vec<ElementTypeDescriptor>,
    }

    impl ElementTypeDescriptorRegistry for FixedRegistry {
        fn descriptors(&self) -> Vec<ElementTypeDescriptor> {
            self.descriptors.clone()
        }
    }

    #[test]
    fn custom_registry_dispatches_by_type_id() {
        let reg = FixedRegistry {
            descriptors: vec![
                ElementTypeDescriptor::builder()
                    .type_id(TypeId::new("service").unwrap())
                    .provides_infrastructure(false)
                    .build(),
                ElementTypeDescriptor::builder()
                    .type_id(TypeId::new("node").unwrap())
                    .provides_infrastructure(true)
                    .build(),
            ],
        };
        assert!(reg.descriptor(&TypeId::new("service").unwrap()).is_some());
        assert!(reg.descriptor(&TypeId::new("node").unwrap()).is_some());
        assert!(reg.descriptor(&TypeId::new("absent").unwrap()).is_none());
        assert!(reg.has_infrastructure_type());
    }

    #[test]
    fn descriptor_serde_roundtrip() {
        let d = ElementTypeDescriptor::builder()
            .type_id(TypeId::new("service").unwrap())
            .required_labels(std::iter::once(LabelKey::new("name").unwrap()).collect())
            .build();
        let json = serde_json::to_string(&d).unwrap();
        let back: ElementTypeDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }
}
