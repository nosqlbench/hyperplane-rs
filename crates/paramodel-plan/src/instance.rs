// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ElementInstanceGraph` — the middle graph from SRD-0002 §6.4.
//!
//! One node per element instance (after mixed-radix trial enumeration
//! and reducto group coalescing); edges are typed
//! `InstanceDependency`s carrying the authored `RelationshipType`. This
//! is the layer that answers "how many instances of each element
//! exist, how is each one bound, and which instances connect to
//! which?" — distinct from both the authored element graph (SRD-0007)
//! and the downstream `ExecutionGraph` (this crate).

use std::collections::{BTreeMap, BTreeSet};

use paramodel_elements::{ElementName, ParameterName, RelationshipType, Value};
use serde::{Deserialize, Serialize};

use crate::error::PlanError;

// ---------------------------------------------------------------------------
// InstanceId.
// ---------------------------------------------------------------------------

/// Human-readable instance identifier of the form
/// `"{element}:{instance_number}"`.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(String);

impl InstanceId {
    /// Build from its parts. The resulting id is always
    /// `"{element}:{instance_number}"`.
    #[must_use]
    pub fn from_parts(element: &ElementName, instance_number: u32) -> Self {
        Self(format!("{}:{instance_number}", element.as_str()))
    }

    /// Construct from a pre-built string. Validates shape.
    pub fn new(s: impl Into<String>) -> std::result::Result<Self, PlanError> {
        let s = s.into();
        if s.is_empty() {
            return Err(PlanError::InvalidStepId {
                reason: "instance id must not be empty".to_owned(),
            });
        }
        if !s.contains(':') {
            return Err(PlanError::InvalidStepId {
                reason: format!("instance id '{s}' must contain a ':' separator"),
            });
        }
        Ok(Self(s))
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Debug for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InstanceId({:?})", self.0)
    }
}

impl Serialize for InstanceId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for InstanceId {
    fn deserialize<D: serde::Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// InstanceScope.
// ---------------------------------------------------------------------------

/// The scope label that describes the lifetime of an instance.
/// Derived by the compiler from reducto's group-level classification
/// (SRD-0002 §6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceScope {
    /// One instance shared across the entire study run.
    Study,
    /// One instance per trial.
    Trial,
    /// One instance per invocation within a trial.
    Invocation,
}

// ---------------------------------------------------------------------------
// ElementInstance.
// ---------------------------------------------------------------------------

/// One element instance as derived by the compiler's phase-1 output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ElementInstance {
    /// Deterministic id.
    pub id: InstanceId,

    /// The element the instance is of.
    pub element: ElementName,

    /// Per-element instance number.
    pub instance_number: u32,

    /// Specific parameter bindings for this instance.
    #[builder(default)]
    pub bindings: BTreeMap<ParameterName, Value>,

    /// Reducto group level at which the instance was realised.
    pub group_level: u32,

    /// Reducto trial-code prefix (for trial-scoped instances).
    pub trial_code: Option<String>,

    /// Resolved scope label per SRD-0002 §6.5.
    pub scope: InstanceScope,
}

// ---------------------------------------------------------------------------
// InstanceDependency.
// ---------------------------------------------------------------------------

/// One directed edge in the element-instance graph. Carries the
/// authored `RelationshipType` unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceDependency {
    /// Dependent end.
    pub source:       InstanceId,
    /// Target end.
    pub target:       InstanceId,
    /// Authored relationship.
    pub relationship: RelationshipType,
}

// ---------------------------------------------------------------------------
// ElementInstanceGraph.
// ---------------------------------------------------------------------------

/// The element-instance graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ElementInstanceGraph {
    /// Nodes.
    #[builder(default)]
    pub instances: Vec<ElementInstance>,
    /// Directed edges.
    #[builder(default)]
    pub edges: Vec<InstanceDependency>,
}

impl ElementInstanceGraph {
    /// Look up an instance by id.
    #[must_use]
    pub fn instance(&self, id: &InstanceId) -> Option<&ElementInstance> {
        self.instances.iter().find(|i| &i.id == id)
    }

    /// Every instance for a given element.
    #[must_use]
    pub fn instances_of(&self, element: &ElementName) -> Vec<&ElementInstance> {
        self.instances
            .iter()
            .filter(|i| &i.element == element)
            .collect()
    }

    /// Edges leaving `id`.
    #[must_use]
    pub fn edges_from(&self, id: &InstanceId) -> Vec<&InstanceDependency> {
        self.edges.iter().filter(|e| &e.source == id).collect()
    }

    /// Edges entering `id`.
    #[must_use]
    pub fn edges_to(&self, id: &InstanceId) -> Vec<&InstanceDependency> {
        self.edges.iter().filter(|e| &e.target == id).collect()
    }

    /// Instance count.
    #[must_use]
    pub const fn total_instances(&self) -> usize {
        self.instances.len()
    }

    /// Instances of one element.
    #[must_use]
    pub fn instance_count(&self, element: &ElementName) -> usize {
        self.instances.iter().filter(|i| &i.element == element).count()
    }

    /// Structural validation — unique instance ids, every edge's
    /// endpoints exist.
    pub fn validate(&self) -> std::result::Result<(), PlanError> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for i in &self.instances {
            if !seen.insert(i.id.as_str()) {
                return Err(PlanError::DuplicateInstance {
                    id: i.id.as_str().to_owned(),
                });
            }
        }
        for e in &self.edges {
            if !seen.contains(e.source.as_str()) {
                return Err(PlanError::UnknownInstance {
                    id: e.source.as_str().to_owned(),
                });
            }
            if !seen.contains(e.target.as_str()) {
                return Err(PlanError::UnknownInstance {
                    id: e.target.as_str().to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Topological order using Kahn's algorithm.
    pub fn topological_order(&self) -> std::result::Result<Vec<&ElementInstance>, PlanError> {
        self.validate()?;
        let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
        for i in &self.instances {
            indegree.insert(i.id.as_str(), 0);
        }
        for e in &self.edges {
            *indegree.entry(e.target.as_str()).or_insert(0) += 1;
        }
        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| *k)
            .collect();
        ready.sort_unstable();
        let mut out: Vec<&ElementInstance> = Vec::with_capacity(self.instances.len());
        while let Some(k) = ready.pop() {
            if let Some(i) = self.instances.iter().find(|i| i.id.as_str() == k) {
                out.push(i);
            }
            let edges_from_k: Vec<&str> = self
                .edges
                .iter()
                .filter(|e| e.source.as_str() == k)
                .map(|e| e.target.as_str())
                .collect();
            for tgt in edges_from_k {
                if let Some(d) = indegree.get_mut(tgt) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        ready.push(tgt);
                    }
                }
            }
        }
        if out.len() != self.instances.len() {
            return Err(PlanError::GraphHasCycle);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }

    fn iid(e: &str, n: u32) -> InstanceId {
        InstanceId::from_parts(&ename(e), n)
    }

    fn instance(element: &str, n: u32, scope: InstanceScope) -> ElementInstance {
        ElementInstance::builder()
            .id(iid(element, n))
            .element(ename(element))
            .instance_number(n)
            .group_level(0)
            .scope(scope)
            .build()
    }

    #[test]
    fn instance_id_from_parts_formats_correctly() {
        let id = InstanceId::from_parts(&ename("db"), 3);
        assert_eq!(id.as_str(), "db:3");
    }

    #[test]
    fn instance_id_new_requires_colon() {
        assert!(InstanceId::new("no_colon").is_err());
        assert!(InstanceId::new("db:0").is_ok());
    }

    #[test]
    fn lookup_and_counting() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![
                instance("db", 0, InstanceScope::Study),
                instance("db", 1, InstanceScope::Trial),
                instance("client", 0, InstanceScope::Trial),
            ])
            .build();
        assert_eq!(g.total_instances(), 3);
        assert_eq!(g.instance_count(&ename("db")), 2);
        assert_eq!(g.instances_of(&ename("client")).len(), 1);
        assert!(g.instance(&iid("db", 0)).is_some());
        assert!(g.instance(&iid("missing", 0)).is_none());
    }

    #[test]
    fn validate_rejects_duplicate_instance() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![
                instance("db", 0, InstanceScope::Study),
                instance("db", 0, InstanceScope::Study),
            ])
            .build();
        assert!(matches!(
            g.validate(),
            Err(PlanError::DuplicateInstance { .. })
        ));
    }

    #[test]
    fn validate_rejects_edge_to_unknown() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![instance("db", 0, InstanceScope::Study)])
            .edges(vec![InstanceDependency {
                source:       iid("db", 0),
                target:       iid("ghost", 0),
                relationship: RelationshipType::Shared,
            }])
            .build();
        assert!(matches!(g.validate(), Err(PlanError::UnknownInstance { .. })));
    }

    #[test]
    fn topological_order_respects_edges() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![
                instance("db", 0, InstanceScope::Study),
                instance("client", 0, InstanceScope::Trial),
            ])
            .edges(vec![InstanceDependency {
                source:       iid("client", 0),
                target:       iid("db", 0),
                relationship: RelationshipType::Shared,
            }])
            .build();
        let order = g.topological_order().unwrap();
        // db (in-degree 1 after the edge) is visited before client? Edge goes
        // client → db (target db), so db has in-degree 1 initially. Ready set
        // is {client}. After client, db becomes ready.
        assert_eq!(order[0].element.as_str(), "client");
        assert_eq!(order[1].element.as_str(), "db");
    }

    #[test]
    fn topological_order_detects_cycle() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![
                instance("a", 0, InstanceScope::Study),
                instance("b", 0, InstanceScope::Study),
            ])
            .edges(vec![
                InstanceDependency {
                    source:       iid("a", 0),
                    target:       iid("b", 0),
                    relationship: RelationshipType::Shared,
                },
                InstanceDependency {
                    source:       iid("b", 0),
                    target:       iid("a", 0),
                    relationship: RelationshipType::Shared,
                },
            ])
            .build();
        assert!(matches!(
            g.topological_order(),
            Err(PlanError::GraphHasCycle)
        ));
    }

    #[test]
    fn serde_roundtrip() {
        let g = ElementInstanceGraph::builder()
            .instances(vec![instance("db", 0, InstanceScope::Study)])
            .build();
        let json = serde_json::to_string(&g).unwrap();
        let back: ElementInstanceGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }
}
