// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Plan-level parameter bindings.
//!
//! A plan binding overrides an element's own `Configuration` for the
//! duration of the plan. Per SRD-0008 D6 the resolution chain is
//! `axis → plan binding → element configuration → parameter default →
//! error`; plan bindings sit in slot 2.

use std::collections::BTreeMap;

use paramodel_elements::ConfigEntry;
use serde::{Deserialize, Serialize};

use crate::ids::ElementParameterRef;

/// Plan-level parameter bindings — `(element, parameter)` → entry.
///
/// `BTreeMap` storage keeps iteration in deterministic key order.
/// The wire format is a sorted array of `[coord, entry]` pairs
/// because JSON map keys must be strings and our coordinate is a
/// two-field struct.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanBindings(BTreeMap<ElementParameterRef, ConfigEntry>);

impl Serialize for PlanBindings {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for pair in &self.0 {
            seq.serialize_element(&pair)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for PlanBindings {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Self, D::Error> {
        let pairs: Vec<(ElementParameterRef, ConfigEntry)> = Vec::deserialize(d)?;
        Ok(Self(pairs.into_iter().collect()))
    }
}

impl PlanBindings {
    /// Empty bindings map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a binding. Returns the previous entry, if any.
    pub fn insert(
        &mut self,
        coord: ElementParameterRef,
        entry: ConfigEntry,
    ) -> Option<ConfigEntry> {
        self.0.insert(coord, entry)
    }

    /// Look up a binding.
    #[must_use]
    pub fn get(&self, coord: &ElementParameterRef) -> Option<&ConfigEntry> {
        self.0.get(coord)
    }

    /// `true` when the coordinate is already bound.
    #[must_use]
    pub fn contains_key(&self, coord: &ElementParameterRef) -> bool {
        self.0.contains_key(coord)
    }

    /// Sorted-by-coord iterator.
    pub fn iter(&self) -> impl Iterator<Item = (&ElementParameterRef, &ConfigEntry)> {
        self.0.iter()
    }

    /// Sorted coord iterator.
    pub fn keys(&self) -> impl Iterator<Item = &ElementParameterRef> {
        self.0.keys()
    }

    /// Binding count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no bindings are set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(ElementParameterRef, ConfigEntry)> for PlanBindings {
    fn from_iter<I: IntoIterator<Item = (ElementParameterRef, ConfigEntry)>>(
        iter: I,
    ) -> Self {
        Self(iter.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use paramodel_elements::{ElementName, ParameterName, Value};

    use super::*;

    fn coord(e: &str, p: &str) -> ElementParameterRef {
        ElementParameterRef::new(
            ElementName::new(e).unwrap(),
            ParameterName::new(p).unwrap(),
        )
    }

    #[test]
    fn insert_and_lookup() {
        let mut b = PlanBindings::new();
        let c = coord("db", "threads");
        let entry = ConfigEntry::literal(Value::integer(
            ParameterName::new("threads").unwrap(),
            16,
            None,
        ));
        b.insert(c.clone(), entry.clone());
        assert_eq!(b.len(), 1);
        assert!(b.contains_key(&c));
        assert_eq!(b.get(&c), Some(&entry));
    }

    #[test]
    fn iter_is_sorted_by_coord() {
        let mut b = PlanBindings::new();
        let v = Value::integer(ParameterName::new("x").unwrap(), 1, None);
        b.insert(
            coord("zebra", "x"),
            ConfigEntry::literal(v.clone()),
        );
        b.insert(coord("apple", "x"), ConfigEntry::literal(v));
        let elements: Vec<&str> = b.keys().map(|c| c.element.as_str()).collect();
        assert_eq!(elements, vec!["apple", "zebra"]);
    }

    #[test]
    fn serde_roundtrip() {
        let mut b = PlanBindings::new();
        b.insert(
            coord("db", "threads"),
            ConfigEntry::literal(Value::integer(
                ParameterName::new("threads").unwrap(),
                16,
                None,
            )),
        );
        let json = serde_json::to_string(&b).unwrap();
        let back: PlanBindings = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }
}
