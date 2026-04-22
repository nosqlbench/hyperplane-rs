// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Binding-level computation (SRD-0010 §1.5–1.6 + S.2).
//!
//! Own binding level: for an element with K axis-bearing parameters
//! spanning ranks `R..R+K-1`, the own binding level is `R+K`. Elements
//! with no axes have own level 0.
//!
//! Effective binding level: maximum of own level and the effective
//! levels of `Shared` / `Exclusive` / `Linear` upstream dependencies
//! (per S.2 — `Linear` propagates, `Dedicated` and `Lifeline` do not).
//!
//! Group level and concrete-boundedness follow from the effective
//! level plus the N axis ranks in the plan.

use std::collections::{BTreeMap, HashMap};

use paramodel_elements::{ElementName, RelationshipType};
use paramodel_plan::TestPlan;

/// Per-element own and effective binding levels.
#[derive(Debug, Clone)]
pub struct BindingStateComputer {
    own_levels:       HashMap<ElementName, u32>,
    effective_levels: HashMap<ElementName, u32>,
    total_axes:       u32,
}

impl BindingStateComputer {
    /// Run the whole Stage-1 binding-level computation for `plan`.
    #[must_use]
    pub fn compute(plan: &TestPlan) -> Self {
        let own_levels = compute_own_levels(plan);
        let total_axes = u32::try_from(plan.axes.len()).unwrap_or(u32::MAX);
        let effective_levels = compute_effective_levels(plan, &own_levels);
        Self {
            own_levels,
            effective_levels,
            total_axes,
        }
    }

    /// Own binding level for `element`. Elements not in the plan
    /// return `0`.
    #[must_use]
    pub fn own_level(&self, element: &ElementName) -> u32 {
        self.own_levels.get(element).copied().unwrap_or(0)
    }

    /// Effective binding level (own plus propagated upstream).
    #[must_use]
    pub fn effective_level(&self, element: &ElementName) -> u32 {
        self.effective_levels.get(element).copied().unwrap_or(0)
    }

    /// Total axis count across the plan.
    #[must_use]
    pub const fn total_axes(&self) -> u32 {
        self.total_axes
    }

    /// `true` iff `x` and `y` land in the same configuration group.
    /// Two elements share a group when they have the same effective
    /// binding level — at any trial the axes at ranks
    /// `[0..level)` hold the same offset values for both, so both
    /// coalesce and activate together.
    ///
    /// Used by SRD-0010 §S.1 for the Linear edge check and by
    /// Rule 2 Exclusive for serialisation-target matching.
    #[must_use]
    pub fn same_group_for_elements(
        &self,
        x: &ElementName,
        y: &ElementName,
    ) -> bool {
        self.effective_level(x) == self.effective_level(y)
    }
}

fn compute_own_levels(plan: &TestPlan) -> HashMap<ElementName, u32> {
    let mut out = HashMap::new();
    let mut rank = 0u32;
    for element in &plan.elements {
        let axes_on_element = plan
            .axes
            .iter()
            .filter(|a| a.target.element == element.name)
            .count();
        let k = u32::try_from(axes_on_element).unwrap_or(u32::MAX);
        out.insert(element.name.clone(), rank + k);
        rank += k;
    }
    out
}

const fn propagates(relationship: RelationshipType) -> bool {
    matches!(
        relationship,
        RelationshipType::Shared | RelationshipType::Exclusive | RelationshipType::Linear
    )
}

fn compute_effective_levels(
    plan:       &TestPlan,
    own_levels: &HashMap<ElementName, u32>,
) -> HashMap<ElementName, u32> {
    // Fixed-point iteration — element graph is a DAG and the number
    // of levels is bounded by `total_axes`, so this converges in at
    // most `elements.len()` passes.
    let mut out: HashMap<ElementName, u32> = own_levels.clone();
    let mut changed = true;
    let mut guard = 0usize;
    let max_passes = plan.elements.len() + 1;
    while changed && guard < max_passes {
        changed = false;
        guard += 1;
        let snapshot: BTreeMap<ElementName, u32> =
            out.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for element in &plan.elements {
            let mut best = snapshot.get(&element.name).copied().unwrap_or(0);
            for dep in &element.dependencies {
                if !propagates(dep.relationship) {
                    continue;
                }
                if let Some(tl) = snapshot.get(&dep.target) {
                    best = best.max(*tl);
                }
            }
            if Some(&best) != snapshot.get(&element.name) {
                out.insert(element.name.clone(), best);
                changed = true;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use paramodel_elements::{
        Element, ElementName, IntegerParameter, LabelValue, Labels, Parameter, ParameterName, Value,
        attributes::label,
    };
    use paramodel_elements::Dependency;
    use paramodel_plan::{
        Axis, AxisName, ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata,
    };
    use ulid::Ulid;

    use super::*;

    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }
    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn element_with_param(name: &str, param: &str) -> Element {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), LabelValue::new("service").unwrap());
        Element::builder()
            .name(ename(name))
            .labels(labels)
            .parameters(vec![Parameter::Integer(
                IntegerParameter::range(pname(param), 1, 64).unwrap(),
            )])
            .build()
    }

    fn plain_element(name: &str) -> Element {
        let mut labels = Labels::new();
        labels.insert(label::r#type(), LabelValue::new("service").unwrap());
        Element::builder().name(ename(name)).labels(labels).build()
    }

    fn axis_on(element: &str, param: &str, values: Vec<i64>) -> Axis {
        Axis::builder()
            .name(AxisName::new(format!("{element}_{param}_axis")).unwrap())
            .target(ElementParameterRef::new(ename(element), pname(param)))
            .values(
                values
                    .into_iter()
                    .map(|v| Value::integer(pname(param), v, None))
                    .collect(),
            )
            .build()
    }

    fn plan(elements: Vec<Element>, axes: Vec<Axis>) -> TestPlan {
        TestPlan::builder()
            .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .name(PlanName::new("p").unwrap())
            .elements(elements)
            .axes(axes)
            .metadata(
                TestPlanMetadata::builder()
                    .created_at(Timestamp::from_second(0).unwrap())
                    .build(),
            )
            .build()
    }

    // ---------- own levels ----------

    #[test]
    fn own_level_matches_cumulative_axis_count() {
        let p = plan(
            vec![
                element_with_param("a", "x"),
                element_with_param("b", "y"),
                plain_element("c"),
            ],
            vec![
                axis_on("a", "x", vec![1, 2]),
                axis_on("b", "y", vec![1, 2, 3]),
            ],
        );
        let bsc = BindingStateComputer::compute(&p);
        // `a` has 1 axis, ranks 0..0, own_level = 1.
        // `b` has 1 axis, ranks 1..1, own_level = 2.
        // `c` has 0 axes, own_level = 2 (cumulative rank = 2).
        assert_eq!(bsc.own_level(&ename("a")), 1);
        assert_eq!(bsc.own_level(&ename("b")), 2);
        assert_eq!(bsc.own_level(&ename("c")), 2);
        assert_eq!(bsc.total_axes(), 2);
    }

    // ---------- effective levels propagate ----------

    #[test]
    fn shared_propagates_effective_level() {
        let mut client = plain_element("client");
        client.dependencies.push(Dependency::shared(ename("db")));
        let p = plan(
            vec![element_with_param("db", "x"), client],
            vec![axis_on("db", "x", vec![1, 2, 3])],
        );
        let bsc = BindingStateComputer::compute(&p);
        assert_eq!(bsc.effective_level(&ename("db")), 1);
        // client's own level is 1 (rank cursor = 1 after db's axis);
        // propagation doesn't lift it further because db's effective
        // level is also 1. Both are 1.
        assert_eq!(bsc.effective_level(&ename("client")), 1);
    }

    #[test]
    fn lifeline_does_not_propagate() {
        let mut x = plain_element("x");
        x.dependencies.push(Dependency::lifeline(ename("y")));
        let p = plan(
            vec![element_with_param("y", "p"), x],
            vec![axis_on("y", "p", vec![1, 2])],
        );
        let bsc = BindingStateComputer::compute(&p);
        assert_eq!(bsc.effective_level(&ename("y")), 1);
        // x has no own axes and a LIFELINE dep; LIFELINE does not
        // propagate (D3), so x's effective level stays at its own
        // level, which is the rank cursor after y's axis = 1.
        // However since x has no axes, effective stays at own=1,
        // not pulled higher by y.
        assert_eq!(bsc.effective_level(&ename("x")), 1);
    }

    #[test]
    fn dedicated_does_not_propagate() {
        let mut x = plain_element("x");
        x.dependencies.push(Dependency::dedicated(ename("y")));
        let p = plan(
            vec![element_with_param("y", "p"), x],
            vec![axis_on("y", "p", vec![1, 2])],
        );
        let bsc = BindingStateComputer::compute(&p);
        assert_eq!(bsc.effective_level(&ename("x")), 1);
    }
}
