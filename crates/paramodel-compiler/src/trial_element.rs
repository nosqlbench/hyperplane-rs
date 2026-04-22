// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Trial-element identification (SRD-0010 §Trial-element identification).
//!
//! Leaf-style selection with explicit overrides and a floating-element
//! carve-out. Runs before Rule 1 and hands the result into the
//! `RuleContext` shared across rules.

use std::collections::{BTreeSet, HashMap};

use paramodel_elements::ElementName;
use paramodel_plan::TestPlan;

use crate::binding::BindingStateComputer;

/// Identify the plan's trial elements.
#[must_use]
pub fn identify_trial_elements(
    plan: &TestPlan,
    bsc:  &BindingStateComputer,
) -> BTreeSet<ElementName> {
    let mut forced_on:  BTreeSet<ElementName> = BTreeSet::new();
    let mut forced_off: BTreeSet<ElementName> = BTreeSet::new();
    for e in &plan.elements {
        match e.trial_element {
            Some(true) => {
                forced_on.insert(e.name.clone());
            }
            Some(false) => {
                forced_off.insert(e.name.clone());
            }
            None => {}
        }
    }

    // Candidate pool per §Trial-element identification step 2.
    let any_trial_axis =
        plan.axes.iter().any(|a| bsc.own_level(&a.target.element) > 0);

    let candidate_pool: BTreeSet<ElementName> = if any_trial_axis {
        plan.elements
            .iter()
            .filter(|e| bsc.effective_level(&e.name) > 0)
            .map(|e| e.name.clone())
            .collect()
    } else {
        // No axes — all non-floating elements are candidates.
        // A "floating" element has no outgoing deps AND no element
        // depends on it.
        let incoming: HashMap<&ElementName, usize> = {
            let mut m: HashMap<&ElementName, usize> = HashMap::new();
            for e in &plan.elements {
                for d in &e.dependencies {
                    *m.entry(&d.target).or_insert(0) += 1;
                }
            }
            m
        };
        plan.elements
            .iter()
            .filter(|e| !e.dependencies.is_empty() || incoming.contains_key(&e.name))
            .map(|e| e.name.clone())
            .collect()
    };

    // Leaf selection — within the candidate pool, pick elements that
    // nothing else in the pool depends on.
    let depended_on: BTreeSet<&ElementName> = plan
        .elements
        .iter()
        .filter(|e| candidate_pool.contains(&e.name))
        .flat_map(|e| e.dependencies.iter().map(|d| &d.target))
        .collect();

    let leaves: BTreeSet<ElementName> = candidate_pool
        .iter()
        .filter(|name| !depended_on.contains(name))
        .cloned()
        .collect();

    let mut out = &leaves | &forced_on;
    for f in &forced_off {
        out.remove(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use paramodel_elements::{
        Element, ElementName, IntegerParameter, LabelValue, Labels, Parameter, ParameterName,
        Value, attributes::label,
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

    fn svc_labels() -> Labels {
        let mut l = Labels::new();
        l.insert(label::r#type(), LabelValue::new("service").unwrap());
        l
    }

    fn element(name: &str) -> Element {
        Element::builder().name(ename(name)).labels(svc_labels()).build()
    }

    fn element_with_axis_param(name: &str, p: &str) -> Element {
        Element::builder()
            .name(ename(name))
            .labels(svc_labels())
            .parameters(vec![Parameter::Integer(
                IntegerParameter::range(pname(p), 1, 64).unwrap(),
            )])
            .build()
    }

    fn axis_on(element: &str, p: &str, values: Vec<i64>) -> Axis {
        Axis::builder()
            .name(AxisName::new(format!("{element}_{p}_axis")).unwrap())
            .target(ElementParameterRef::new(ename(element), pname(p)))
            .values(
                values
                    .into_iter()
                    .map(|v| Value::integer(pname(p), v, None))
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

    // ---------- leaf selection ----------

    #[test]
    fn client_is_leaf_when_nothing_depends_on_it() {
        let mut client = element_with_axis_param("client", "threads");
        client.dependencies.push(Dependency::shared(ename("db")));
        let p = plan(
            vec![element("db"), client],
            vec![axis_on("client", "threads", vec![1, 2, 4])],
        );
        let bsc = BindingStateComputer::compute(&p);
        let trial = identify_trial_elements(&p, &bsc);
        // `client` has an axis and is the leaf — it's the trial
        // element. `db` has no axis and others depend on it.
        assert!(trial.contains(&ename("client")));
        assert!(!trial.contains(&ename("db")));
    }

    #[test]
    fn forced_on_override_is_honoured() {
        let mut db = element_with_axis_param("db", "x");
        db.trial_element = Some(true);
        let mut client = element_with_axis_param("client", "y");
        client.dependencies.push(Dependency::shared(ename("db")));
        let p = plan(
            vec![db, client],
            vec![
                axis_on("db", "x", vec![1, 2]),
                axis_on("client", "y", vec![1, 2]),
            ],
        );
        let bsc = BindingStateComputer::compute(&p);
        let trial = identify_trial_elements(&p, &bsc);
        assert!(trial.contains(&ename("db"))); // forced on
        assert!(trial.contains(&ename("client"))); // also a leaf
    }

    #[test]
    fn forced_off_override_excludes() {
        let mut client = element_with_axis_param("client", "y");
        client.trial_element = Some(false);
        let p = plan(
            vec![client],
            vec![axis_on("client", "y", vec![1, 2])],
        );
        let bsc = BindingStateComputer::compute(&p);
        let trial = identify_trial_elements(&p, &bsc);
        // Even though `client` would otherwise be a leaf, the override
        // excludes it.
        assert!(!trial.contains(&ename("client")));
    }

    #[test]
    fn no_axes_floating_element_is_excluded() {
        // `floater` has no deps and nothing depends on it — excluded.
        let p = plan(vec![element("floater"), element("db")], vec![]);
        let bsc = BindingStateComputer::compute(&p);
        let trial = identify_trial_elements(&p, &bsc);
        assert!(!trial.contains(&ename("floater")));
    }
}
