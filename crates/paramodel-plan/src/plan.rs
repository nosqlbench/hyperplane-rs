// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `TestPlan` — the authored composition of elements, axes, bindings,
//! and execution policies.
//!
//! Per SRD-0008 D1 the plan is an immutable struct built via
//! [`TestPlan::builder`] with a separate [`TestPlan::validate`] step
//! that enforces structural invariants (D12). Semantic checks (cycle
//! detection, schedulability) belong to the compiler and are out of
//! scope here.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use paramodel_elements::{
    Element, ElementName, Fingerprint, Labels, Parameter, ParameterName, Tags,
};
use serde::{Deserialize, Serialize};

use crate::axis::Axis;
use crate::bindings::PlanBindings;
use crate::error::{PlanError, Result};
use crate::ids::{AxisName, ElementParameterRef, PlanName, TestPlanId};
use crate::ordering::{OptimizationStrategy, TrialOrdering};
use crate::policies::ExecutionPolicies;

// ---------------------------------------------------------------------------
// TestPlanMetadata.
// ---------------------------------------------------------------------------

/// Creation / revision metadata carried on a `TestPlan`.
///
/// Not in the plan fingerprint (SRD-0008 D11) — metadata fields are
/// for traceability, not identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct TestPlanMetadata {
    /// When the plan was authored.
    pub created_at: Timestamp,
    /// Author (optional).
    pub created_by: Option<String>,
    /// Free-form description.
    pub description: Option<String>,
    /// Semantic version string, if the author tracks revisions.
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// TestPlan.
// ---------------------------------------------------------------------------

/// Immutable composition of elements, axes, plan-level bindings, and
/// execution policies.
///
/// Construction goes through [`bon::Builder`]; call
/// [`TestPlan::validate`] after `build()` to enforce SRD-0008 D12
/// structural invariants. Fingerprint is computed lazily and cached on
/// first [`TestPlan::fingerprint`] call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct TestPlan {
    /// Plan identifier — a ULID, independent of content hash.
    pub id:   TestPlanId,
    /// Human-authored plan name.
    pub name: PlanName,

    /// Elements participating in the study, in authoring order.
    #[builder(default)]
    pub elements: Vec<Element>,

    /// Axes that vary specific element parameters. Ordering is
    /// major → minor for reducto's mixed-radix rank assignment.
    #[builder(default)]
    pub axes: Vec<Axis>,

    /// Plan-level parameter bindings — sits between axes and an
    /// element's own `Configuration` in the resolution chain
    /// (SRD-0008 D6).
    #[builder(default)]
    pub bindings: PlanBindings,

    /// Execution policies.
    #[builder(default)]
    pub policies: ExecutionPolicies,

    /// Compiler-side optimisation level.
    #[builder(default)]
    pub optimization_strategy: OptimizationStrategy,

    /// Trial-space traversal policy.
    #[builder(default)]
    pub trial_ordering: TrialOrdering,

    /// Intrinsic facts attached to the plan.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,

    /// Creation/revision metadata.
    pub metadata: TestPlanMetadata,
}

impl TestPlan {
    // ---- accessors --------------------------------------------------

    /// Multiplicative size of the trial space — the product of every
    /// axis's cardinality. Returns `1` when there are no axes (a
    /// plan with zero axes has exactly one "trial" — the base
    /// configuration).
    #[must_use]
    pub fn trial_space_size(&self) -> u64 {
        self.axes
            .iter()
            .map(|a| a.cardinality() as u64)
            .product::<u64>()
            .max(1)
    }

    /// Look up an element by name.
    #[must_use]
    pub fn element(&self, name: &ElementName) -> Option<&Element> {
        self.elements.iter().find(|e| &e.name == name)
    }

    /// Look up an axis by name.
    #[must_use]
    pub fn axis(&self, name: &AxisName) -> Option<&Axis> {
        self.axes.iter().find(|a| &a.name == name)
    }

    /// Look up a plan-level binding.
    #[must_use]
    pub fn binding(
        &self,
        coord: &ElementParameterRef,
    ) -> Option<&paramodel_elements::ConfigEntry> {
        self.bindings.get(coord)
    }

    // ---- validation -------------------------------------------------

    /// Enforce SRD-0008 D12 structural invariants.
    pub fn validate(&self) -> Result<()> {
        // 1. Element names unique.
        let mut element_names: BTreeSet<&str> = BTreeSet::new();
        for e in &self.elements {
            if !element_names.insert(e.name.as_str()) {
                return Err(PlanError::DuplicateElement {
                    name: e.name.as_str().to_owned(),
                });
            }
        }

        // 2. Policies are internally consistent.
        self.policies.validate()?;

        // 3. Each axis self-validates, plus cross-plan checks.
        let mut axis_names: BTreeSet<&str> = BTreeSet::new();
        let mut axis_targets: BTreeMap<ElementParameterRef, &str> = BTreeMap::new();
        for axis in &self.axes {
            axis.validate()?;

            if !axis_names.insert(axis.name.as_str()) {
                return Err(PlanError::DuplicateAxisName {
                    name: axis.name.as_str().to_owned(),
                });
            }

            let element = self.element(&axis.target.element).ok_or_else(|| {
                PlanError::AxisTargetElementNotFound {
                    axis:    axis.name.as_str().to_owned(),
                    element: axis.target.element.as_str().to_owned(),
                }
            })?;

            let param = find_parameter(element, &axis.target.parameter).ok_or_else(|| {
                PlanError::AxisTargetParameterNotFound {
                    axis:      axis.name.as_str().to_owned(),
                    element:   axis.target.element.as_str().to_owned(),
                    parameter: axis.target.parameter.as_str().to_owned(),
                }
            })?;

            if let Parameter::Derived(_) = param {
                return Err(PlanError::DerivedParameterCannotBeAxis {
                    axis:      axis.name.as_str().to_owned(),
                    element:   axis.target.element.as_str().to_owned(),
                    parameter: axis.target.parameter.as_str().to_owned(),
                });
            }

            let expected_kind = param.kind();
            for (index, v) in axis.values.iter().enumerate() {
                if v.kind() != expected_kind {
                    return Err(PlanError::AxisValueKindMismatch {
                        axis:     axis.name.as_str().to_owned(),
                        index,
                        expected: expected_kind,
                        actual:   v.kind(),
                    });
                }
            }

            if let Some(prior) =
                axis_targets.insert(axis.target.clone(), axis.name.as_str())
            {
                return Err(PlanError::DuplicateAxisTarget {
                    axis_a:    prior.to_owned(),
                    axis_b:    axis.name.as_str().to_owned(),
                    element:   axis.target.element.as_str().to_owned(),
                    parameter: axis.target.parameter.as_str().to_owned(),
                });
            }
        }

        // 4. Plan-binding targets are real and don't overlap axes.
        for coord in self.bindings.keys() {
            let element = self.element(&coord.element).ok_or_else(|| {
                PlanError::BindingTargetElementNotFound {
                    element: coord.element.as_str().to_owned(),
                }
            })?;
            if find_parameter(element, &coord.parameter).is_none() {
                return Err(PlanError::BindingTargetParameterNotFound {
                    element:   coord.element.as_str().to_owned(),
                    parameter: coord.parameter.as_str().to_owned(),
                });
            }
            if let Some(axis_name) = axis_targets.get(coord) {
                return Err(PlanError::BindingOverlapsAxis {
                    element:   coord.element.as_str().to_owned(),
                    parameter: coord.parameter.as_str().to_owned(),
                    axis:      (*axis_name).to_owned(),
                });
            }
        }

        Ok(())
    }

    // ---- derivation / transformation --------------------------------

    /// Return a new plan with the axes permuted to the given name
    /// order. The result has a fresh `id` (caller supplies via the
    /// `new_id` argument) and a fresh fingerprint.
    ///
    /// `order` must be a permutation of the plan's existing axis
    /// names — missing names, duplicates, or unknown names are all
    /// [`PlanError::InvalidAxisOrder`].
    pub fn with_axes_reordered(
        &self,
        order: &[AxisName],
        new_id: TestPlanId,
    ) -> Result<Self> {
        if order.len() != self.axes.len() {
            return Err(PlanError::InvalidAxisOrder {
                reason: format!(
                    "order has {} axes; plan has {}",
                    order.len(),
                    self.axes.len()
                ),
            });
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for name in order {
            if !seen.insert(name.as_str()) {
                return Err(PlanError::InvalidAxisOrder {
                    reason: format!("duplicate axis name '{name}' in order"),
                });
            }
            if !self.axes.iter().any(|a| &a.name == name) {
                return Err(PlanError::InvalidAxisOrder {
                    reason: format!("unknown axis name '{name}' in order"),
                });
            }
        }
        let mut reordered = Vec::with_capacity(self.axes.len());
        for name in order {
            // Presence was verified in the loop above.
            let picked = self
                .axes
                .iter()
                .find(|a| &a.name == name)
                .expect("checked present")
                .clone();
            reordered.push(picked);
        }
        let mut next = self.clone();
        next.id = new_id;
        next.axes = reordered;
        Ok(next)
    }

    // ---- fingerprint ------------------------------------------------

    /// Content fingerprint of the plan.
    ///
    /// Excludes `id`, `labels`, `tags`, and `metadata` per SRD-0008
    /// D11. Computed by walking fingerprint-eligible fields and
    /// emitting a stable byte encoding, then BLAKE3-hashing.
    ///
    /// Embedded `Value`s contribute their 32-byte content fingerprint
    /// (SRD-0004 §Fingerprinting) rather than their full serde form,
    /// so provenance timestamps and generator tags — which are
    /// creation-time metadata, not identity — don't leak into the
    /// plan hash.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut out = Vec::new();
        out.push(PLAN_TAG);

        write_str(&mut out, self.name.as_str());

        write_u32(&mut out, self.elements.len());
        for e in &self.elements {
            write_element(&mut out, e);
        }

        write_u32(&mut out, self.axes.len());
        for a in &self.axes {
            write_axis(&mut out, a);
        }

        write_u32(&mut out, self.bindings.len());
        for (coord, entry) in self.bindings.iter() {
            write_str(&mut out, coord.element.as_str());
            write_str(&mut out, coord.parameter.as_str());
            write_config_entry(&mut out, entry);
        }

        // Policies / optimisation / ordering carry no `Value`s, so
        // serde_json is a stable encoding for them.
        out.extend_from_slice(
            &serde_json::to_vec(&self.policies).expect("serialize policies"),
        );
        out.extend_from_slice(
            &serde_json::to_vec(&self.optimization_strategy)
                .expect("serialize optimization strategy"),
        );
        out.extend_from_slice(
            &serde_json::to_vec(&self.trial_ordering).expect("serialize trial ordering"),
        );

        Fingerprint::of(&out)
    }
}

// ---------------------------------------------------------------------------
// Canonical byte encoding.
// ---------------------------------------------------------------------------

const PLAN_TAG:    u8 = 0x20;
const ELEMENT_TAG: u8 = 0x21;
const AXIS_TAG:    u8 = 0x22;
const CONFIG_LITERAL_TAG: u8 = 0x01;
const CONFIG_TOKEN_TAG:   u8 = 0x02;

fn write_u32(out: &mut Vec<u8>, n: usize) {
    let v = u32::try_from(n).expect("plan-size fits in u32");
    out.extend_from_slice(&v.to_le_bytes());
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_u32(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn write_config_entry(out: &mut Vec<u8>, e: &paramodel_elements::ConfigEntry) {
    use paramodel_elements::ConfigEntry;
    match e {
        ConfigEntry::Literal { value } => {
            out.push(CONFIG_LITERAL_TAG);
            out.extend_from_slice(value.fingerprint().as_bytes());
        }
        ConfigEntry::Token { expr } => {
            out.push(CONFIG_TOKEN_TAG);
            write_str(out, expr.as_str());
        }
    }
}

// Every non-`Value`-bearing Element field rides through serde_json —
// Parameter defaults are native types (i64/f64/bool/String/IndexSet),
// Plug/Socket/Dependency/lifecycle fields carry no Values.
#[derive(Serialize)]
struct ElementNoConfig<'a> {
    name:                  &'a ElementName,
    labels:                &'a paramodel_elements::Labels,
    tags:                  &'a paramodel_elements::Tags,
    plugs:                 &'a Vec<paramodel_elements::Plug>,
    sockets:               &'a Vec<paramodel_elements::Socket>,
    parameters:            &'a Vec<Parameter>,
    result_parameters:     &'a Vec<Parameter>,
    exports:               &'a paramodel_elements::Exports,
    dependencies:          &'a Vec<paramodel_elements::Dependency>,
    health_check:          &'a Option<paramodel_elements::HealthCheckSpec>,
    shutdown_semantics:    &'a paramodel_elements::ShutdownSemantics,
    trial_element:         &'a Option<bool>,
    max_concurrency:       &'a Option<u32>,
    max_group_concurrency: &'a Option<u32>,
}

fn write_element(out: &mut Vec<u8>, e: &Element) {
    out.push(ELEMENT_TAG);

    let nc = ElementNoConfig {
        name:                  &e.name,
        labels:                &e.labels,
        tags:                  &e.tags,
        plugs:                 &e.plugs,
        sockets:               &e.sockets,
        parameters:            &e.parameters,
        result_parameters:     &e.result_parameters,
        exports:               &e.exports,
        dependencies:          &e.dependencies,
        health_check:          &e.health_check,
        shutdown_semantics:    &e.shutdown_semantics,
        trial_element:         &e.trial_element,
        max_concurrency:       &e.max_concurrency,
        max_group_concurrency: &e.max_group_concurrency,
    };
    out.extend_from_slice(&serde_json::to_vec(&nc).expect("serialize element"));

    // Configuration contains `Value`s — encode manually via
    // `value.fingerprint()`.
    write_u32(out, e.configuration.len());
    for (k, entry) in e.configuration.iter() {
        write_str(out, k.as_str());
        write_config_entry(out, entry);
    }
}

fn write_axis(out: &mut Vec<u8>, a: &Axis) {
    out.push(AXIS_TAG);
    write_str(out, a.name.as_str());
    write_str(out, a.target.element.as_str());
    write_str(out, a.target.parameter.as_str());

    write_u32(out, a.values.len());
    for v in &a.values {
        out.extend_from_slice(v.fingerprint().as_bytes());
    }

    match &a.boundary_values {
        Some(bvs) => {
            out.push(0x01);
            write_u32(out, bvs.len());
            for v in bvs {
                out.extend_from_slice(v.fingerprint().as_bytes());
            }
        }
        None => out.push(0x00),
    }

    if let Some(d) = &a.description {
        out.push(0x01);
        write_str(out, d);
    } else {
        out.push(0x00);
    }

    // Labels / tags carry no Values; serde_json is fine.
    out.extend_from_slice(&serde_json::to_vec(&a.labels).expect("serialize axis labels"));
    out.extend_from_slice(&serde_json::to_vec(&a.tags).expect("serialize axis tags"));
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

fn find_parameter<'a>(element: &'a Element, name: &ParameterName) -> Option<&'a Parameter> {
    element.parameters.iter().find(|p| p.name() == name)
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use paramodel_elements::{
        ConfigEntry, IntegerParameter, LabelKey, LabelValue, Labels, OpenRegistry,
        Parameter, ParameterName, Value, attributes::label,
    };
    use ulid::Ulid;

    use super::*;

    fn eid(n: u64) -> TestPlanId {
        TestPlanId::from_ulid(Ulid::from_parts(n, 1))
    }
    fn pname(s: &str) -> PlanName {
        PlanName::new(s).unwrap()
    }
    fn axname(s: &str) -> AxisName {
        AxisName::new(s).unwrap()
    }
    fn elname(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }
    fn parname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }
    fn ivalue(p: &str, v: i64) -> Value {
        Value::integer(parname(p), v, None)
    }
    fn coord(e: &str, p: &str) -> ElementParameterRef {
        ElementParameterRef::new(elname(e), parname(p))
    }

    fn db_element() -> Element {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), LabelValue::new("service").unwrap());
        let p = Parameter::Integer(
            IntegerParameter::range(parname("threads"), 1, 64).unwrap(),
        );
        Element::builder()
            .name(elname("db"))
            .labels(labels)
            .parameters(vec![p])
            .build()
    }

    fn metadata() -> TestPlanMetadata {
        TestPlanMetadata::builder()
            .created_at(Timestamp::from_second(1_700_000_000).unwrap())
            .build()
    }

    fn threads_axis() -> Axis {
        Axis::builder()
            .name(axname("threads_axis"))
            .target(coord("db", "threads"))
            .values(vec![ivalue("threads", 1), ivalue("threads", 8)])
            .build()
    }

    fn canonical_plan() -> TestPlan {
        TestPlan::builder()
            .id(eid(1))
            .name(pname("vectorbench"))
            .elements(vec![db_element()])
            .axes(vec![threads_axis()])
            .metadata(metadata())
            .build()
    }

    // ---------- Accessors / trial_space_size ----------

    #[test]
    fn trial_space_size_multiplies_axis_cardinalities() {
        let mut p = canonical_plan();
        p.axes.push(
            Axis::builder()
                .name(axname("threads_axis_b"))
                .target(coord("db", "threads"))
                .values(vec![
                    ivalue("threads", 4),
                    ivalue("threads", 16),
                    ivalue("threads", 32),
                ])
                .build(),
        );
        assert_eq!(p.trial_space_size(), 2 * 3);
    }

    #[test]
    fn trial_space_size_is_one_when_no_axes() {
        let mut p = canonical_plan();
        p.axes.clear();
        assert_eq!(p.trial_space_size(), 1);
    }

    #[test]
    fn element_and_axis_lookup_work() {
        let p = canonical_plan();
        assert!(p.element(&elname("db")).is_some());
        assert!(p.element(&elname("missing")).is_none());
        assert!(p.axis(&axname("threads_axis")).is_some());
        assert!(p.axis(&axname("missing")).is_none());
    }

    #[test]
    fn binding_lookup_works() {
        let mut p = canonical_plan();
        p.bindings.insert(
            coord("db", "threads"),
            ConfigEntry::literal(ivalue("threads", 16)),
        );
        // Would conflict with the axis; that's handled by validate(),
        // not by `binding()` itself.
        assert!(p.binding(&coord("db", "threads")).is_some());
    }

    // ---------- validate: happy path ----------

    #[test]
    fn canonical_plan_validates_and_elements_do_too() {
        let p = canonical_plan();
        p.validate().expect("plan valid");
        // Element itself is still valid per its registry.
        p.elements[0]
            .validate(&OpenRegistry::new())
            .expect("element valid");
    }

    // ---------- validate: structural errors ----------

    #[test]
    fn duplicate_element_name_rejected() {
        let mut p = canonical_plan();
        p.elements.push(db_element());
        assert!(matches!(
            p.validate(),
            Err(PlanError::DuplicateElement { .. })
        ));
    }

    #[test]
    fn axis_target_element_not_found() {
        let mut p = canonical_plan();
        p.axes[0].target.element = elname("missing");
        assert!(matches!(
            p.validate(),
            Err(PlanError::AxisTargetElementNotFound { .. })
        ));
    }

    #[test]
    fn axis_target_parameter_not_found() {
        let mut p = canonical_plan();
        p.axes[0].target.parameter = parname("ghost");
        assert!(matches!(
            p.validate(),
            Err(PlanError::AxisTargetParameterNotFound { .. })
        ));
    }

    #[test]
    fn axis_value_kind_mismatch_rejected() {
        let mut p = canonical_plan();
        // Integer parameter; push a boolean value.
        p.axes[0]
            .values
            .push(Value::boolean(parname("threads"), true, None));
        assert!(matches!(
            p.validate(),
            Err(PlanError::AxisValueKindMismatch { .. })
        ));
    }

    #[test]
    fn axis_on_derived_parameter_rejected() {
        use paramodel_elements::{DerivedParameter, Expression, Literal, ValueKind};
        let derived = Parameter::Derived(
            DerivedParameter::new(
                parname("doubled"),
                ValueKind::Integer,
                Expression::literal(Literal::Integer { value: 2 }),
            )
            .unwrap(),
        );
        let mut labels = Labels::new();
        labels.insert(label::r#type(), LabelValue::new("service").unwrap());
        let element = Element::builder()
            .name(elname("db"))
            .labels(labels)
            .parameters(vec![derived])
            .build();
        let axis = Axis::builder()
            .name(axname("doubled_axis"))
            .target(coord("db", "doubled"))
            .values(vec![ivalue("doubled", 1)])
            .build();
        let p = TestPlan::builder()
            .id(eid(1))
            .name(pname("vectorbench"))
            .elements(vec![element])
            .axes(vec![axis])
            .metadata(metadata())
            .build();
        assert!(matches!(
            p.validate(),
            Err(PlanError::DerivedParameterCannotBeAxis { .. })
        ));
    }

    #[test]
    fn duplicate_axis_name_rejected() {
        let mut p = canonical_plan();
        p.axes.push(threads_axis());
        assert!(matches!(
            p.validate(),
            Err(PlanError::DuplicateAxisName { .. } | PlanError::DuplicateAxisTarget { .. })
        ));
    }

    #[test]
    fn duplicate_axis_target_rejected() {
        let mut p = canonical_plan();
        p.axes.push(
            Axis::builder()
                .name(axname("threads_axis_b"))
                .target(coord("db", "threads"))
                .values(vec![ivalue("threads", 2)])
                .build(),
        );
        assert!(matches!(
            p.validate(),
            Err(PlanError::DuplicateAxisTarget { .. })
        ));
    }

    #[test]
    fn binding_target_element_not_found() {
        let mut p = canonical_plan();
        p.bindings.insert(
            coord("missing", "threads"),
            ConfigEntry::literal(ivalue("threads", 1)),
        );
        assert!(matches!(
            p.validate(),
            Err(PlanError::BindingTargetElementNotFound { .. })
        ));
    }

    #[test]
    fn binding_target_parameter_not_found() {
        let mut p = canonical_plan();
        p.bindings.insert(
            coord("db", "ghost"),
            ConfigEntry::literal(ivalue("ghost", 1)),
        );
        assert!(matches!(
            p.validate(),
            Err(PlanError::BindingTargetParameterNotFound { .. })
        ));
    }

    #[test]
    fn binding_overlapping_axis_rejected() {
        let mut p = canonical_plan();
        p.bindings.insert(
            coord("db", "threads"),
            ConfigEntry::literal(ivalue("threads", 16)),
        );
        assert!(matches!(
            p.validate(),
            Err(PlanError::BindingOverlapsAxis { .. })
        ));
    }

    // ---------- with_axes_reordered ----------

    #[test]
    fn with_axes_reordered_happy_path() {
        let mut p = canonical_plan();
        p.axes.push(
            Axis::builder()
                .name(axname("other_axis"))
                .target(coord("db", "threads"))
                .values(vec![ivalue("threads", 4)])
                .build(),
        );
        // Ordering is [threads_axis, other_axis]; flip it.
        let reordered = p
            .with_axes_reordered(
                &[axname("other_axis"), axname("threads_axis")],
                eid(2),
            )
            .unwrap();
        assert_eq!(reordered.id, eid(2));
        assert_eq!(reordered.axes[0].name.as_str(), "other_axis");
        assert_eq!(reordered.axes[1].name.as_str(), "threads_axis");
        // Fingerprint changes.
        assert_ne!(p.fingerprint(), reordered.fingerprint());
    }

    #[test]
    fn with_axes_reordered_length_mismatch_rejected() {
        let p = canonical_plan();
        assert!(matches!(
            p.with_axes_reordered(&[], eid(2)),
            Err(PlanError::InvalidAxisOrder { .. })
        ));
    }

    #[test]
    fn with_axes_reordered_unknown_axis_rejected() {
        let p = canonical_plan();
        assert!(matches!(
            p.with_axes_reordered(&[axname("ghost")], eid(2)),
            Err(PlanError::InvalidAxisOrder { .. })
        ));
    }

    // ---------- Fingerprint ----------

    #[test]
    fn fingerprint_is_deterministic() {
        let a = canonical_plan().fingerprint();
        let b = canonical_plan().fingerprint();
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_excludes_id_labels_tags_metadata() {
        let mut p = canonical_plan();
        let fp_before = p.fingerprint();
        p.id = eid(999);
        p.labels
            .insert(LabelKey::new("owner").unwrap(), LabelValue::new("ops").unwrap());
        p.metadata = TestPlanMetadata::builder()
            .created_at(Timestamp::from_second(2_000_000_000).unwrap())
            .description("changed".to_owned())
            .build();
        let fp_after = p.fingerprint();
        assert_eq!(fp_before, fp_after);
    }

    #[test]
    fn fingerprint_includes_elements() {
        let a = canonical_plan().fingerprint();
        let mut p2 = canonical_plan();
        p2.elements[0].labels.insert(
            LabelKey::new("extra").unwrap(),
            LabelValue::new("x").unwrap(),
        );
        assert_ne!(a, p2.fingerprint());
    }

    #[test]
    fn fingerprint_includes_axes() {
        let a = canonical_plan().fingerprint();
        let mut p2 = canonical_plan();
        p2.axes[0].values.push(ivalue("threads", 16));
        assert_ne!(a, p2.fingerprint());
    }

    #[test]
    fn fingerprint_includes_trial_ordering() {
        let a = canonical_plan().fingerprint();
        let mut p2 = canonical_plan();
        p2.trial_ordering = TrialOrdering::Shuffled { seed: 42 };
        assert_ne!(a, p2.fingerprint());
    }

    #[test]
    fn plan_labels_and_tags_behave_as_maps() {
        let mut p = canonical_plan();
        p.labels.insert(
            LabelKey::new("owner").unwrap(),
            LabelValue::new("bench").unwrap(),
        );
        assert_eq!(p.labels.len(), 1);
        assert!(p.tags.is_empty());
    }

    // ---------- serde ----------

    #[test]
    fn plan_serde_roundtrip() {
        let p = canonical_plan();
        let json = serde_json::to_string(&p).unwrap();
        let back: TestPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
