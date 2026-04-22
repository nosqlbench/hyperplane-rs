// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

#![allow(
    clippy::items_after_statements,
    clippy::too_many_lines,
    reason = "proptest shrinker wants everything in one callable"
)]

//! Integration-level proptest fuzz: generate random plans, compile
//! them, then execute end-to-end through `DefaultExecutor`. Catches
//! corner-case interactions between compiler rules + runtime wiring
//! that unit-level fuzz can't see.
//!
//! Key property: no matter what plan shape proptest generates, the
//! pipeline must terminate — either cleanly with `Ok(results)` at a
//! terminal status, or `Err(e)` in a known variant. Never panic,
//! never hang.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_compiler::{Compiler, DefaultCompiler};
use paramodel_elements::{
    Dependency, Element, ElementName, ElementRuntime, IntegerParameter, LabelValue,
    Labels, LiveStatusSummary, MaterializationOutputs, OperationalState, Parameter,
    ParameterName, RelationshipType, ResolvedConfiguration, StateObservation,
    StateTransitionListener, Value, attributes::label,
};
use paramodel_executor::{
    DefaultExecutor, DefaultOrchestrator, DefaultScheduler, ExecutionStatus, Executor,
    InMemoryJournalWriter, NoopObserver, NoopStateManager,
};
use paramodel_plan::{
    Axis, AxisName, ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata,
};
use proptest::prelude::*;
use ulid::Ulid;

// ---------------------------------------------------------------------------
// Runtime that always succeeds.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct AlwaysOkRuntime;

#[derive(Debug)]
struct NoopObservation;
impl StateObservation for NoopObservation {
    fn cancel(&self) {}
}

#[async_trait]
impl ElementRuntime for AlwaysOkRuntime {
    async fn materialize(
        &self,
        _: &ResolvedConfiguration,
    ) -> paramodel_elements::Result<MaterializationOutputs> {
        Ok(MaterializationOutputs::new())
    }
    async fn dematerialize(&self) -> paramodel_elements::Result<()> {
        Ok(())
    }
    async fn status_check(&self) -> LiveStatusSummary {
        LiveStatusSummary {
            state:   OperationalState::Ready,
            summary: "ok".to_owned(),
        }
    }
    fn observe_state(
        &self,
        _l: StateTransitionListener,
    ) -> Box<dyn StateObservation> {
        Box::new(NoopObservation)
    }
}

// ---------------------------------------------------------------------------
// Plan generators.
// ---------------------------------------------------------------------------

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

/// Spec for one generated element.
#[derive(Debug, Clone)]
struct ElementSpec {
    name:       String,
    axis_card:  Option<u32>, // Some(n) → element has an axis with n values
    deps:       Vec<(String, RelationshipType)>,
}

fn arb_relationship() -> impl Strategy<Value = RelationshipType> {
    prop_oneof![
        Just(RelationshipType::Shared),
        Just(RelationshipType::Linear),
        Just(RelationshipType::Lifeline),
    ]
}

/// Generate N elements with random axis cardinalities + random
/// acyclic Shared/Linear/Lifeline deps (target index < source
/// index, guaranteeing no cycles).
fn arb_element_specs(max_n: u8) -> impl Strategy<Value = Vec<ElementSpec>> {
    (1u8..=max_n).prop_flat_map(|n| {
        let cards =
            proptest::collection::vec(prop::option::of(1u32..=3), n as usize);
        let deps_per_element = proptest::collection::vec(
            proptest::collection::vec(
                (arb_relationship(), 0u8..=255),
                0..=3,
            ),
            n as usize,
        );
        (cards, deps_per_element).prop_map(move |(cards, deps_per_element)| {
            let mut specs: Vec<ElementSpec> = (0..n)
                .map(|i| ElementSpec {
                    name:      format!("e{i}"),
                    axis_card: None,
                    deps:      Vec::new(),
                })
                .collect();
            for (i, card) in cards.iter().enumerate() {
                specs[i].axis_card = *card;
            }
            for (i, deps) in deps_per_element.into_iter().enumerate() {
                if i == 0 {
                    continue;
                }
                for (rel, target_slot) in deps {
                    let target = (target_slot as usize) % i;
                    specs[i].deps.push((format!("e{target}"), rel));
                }
            }
            specs
        })
    })
}

fn build_plan(specs: &[ElementSpec]) -> TestPlan {
    let mut elements: Vec<Element> = Vec::with_capacity(specs.len());
    let mut axes: Vec<Axis> = Vec::new();
    for spec in specs {
        let parameters = if spec.axis_card.is_some() {
            vec![Parameter::Integer(
                IntegerParameter::range(pname("p"), 1, 1024).unwrap(),
            )]
        } else {
            Vec::new()
        };
        let mut built = Element::builder()
            .name(ename(&spec.name))
            .labels(svc_labels())
            .parameters(parameters)
            .build();
        for (target, rel) in &spec.deps {
            let dep = match rel {
                RelationshipType::Shared => Dependency::shared(ename(target)),
                RelationshipType::Linear => Dependency::linear(ename(target)),
                RelationshipType::Lifeline => Dependency::lifeline(ename(target)),
                RelationshipType::Exclusive => Dependency::exclusive(ename(target)),
                RelationshipType::Dedicated => Dependency::dedicated(ename(target)),
            };
            built.dependencies.push(dep);
        }
        elements.push(built);
    }
    for (i, spec) in specs.iter().enumerate() {
        if let Some(card) = spec.axis_card {
            let mut axis_name = format!("axis{i}");
            // Ensure unique axis names by suffixing the element.
            axis_name.push_str(&spec.name);
            let values: Vec<Value> = (1..=i64::from(card))
                .map(|v| Value::integer(pname("p"), v, None))
                .collect();
            axes.push(
                Axis::builder()
                    .name(AxisName::new(axis_name).unwrap())
                    .target(ElementParameterRef::new(
                        ename(&spec.name),
                        pname("p"),
                    ))
                    .values(values)
                    .build(),
            );
        }
    }
    TestPlan::builder()
        .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
        .name(PlanName::new("fuzz").unwrap())
        .elements(elements)
        .axes(axes)
        .metadata(
            TestPlanMetadata::builder()
                .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                .build(),
        )
        .build()
}

// ---------------------------------------------------------------------------
// Proptest: random plan compiles + executes, never panics.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    /// Compile → execute on a random small plan. Either succeeds
    /// with a terminal status or errors cleanly — never panics.
    #[test]
    fn random_plan_compiles_and_executes_without_panic(
        specs in arb_element_specs(4),
    ) {
        let plan = build_plan(&specs);
        let Ok(compiled) = DefaultCompiler::default().compile(&plan) else {
            return Ok(()); // rejection is acceptable
        };
        // Build an orchestrator with a runtime for every declared
        // element.
        let mut runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>> =
            BTreeMap::new();
        for spec in &specs {
            runtimes.insert(
                ename(&spec.name),
                Arc::new(AlwaysOkRuntime) as Arc<dyn ElementRuntime>,
            );
        }
        let exec = DefaultExecutor::new(
            Arc::new(DefaultOrchestrator::new(runtimes)),
            Arc::new(DefaultScheduler::new()),
            Arc::new(NoopStateManager::new()),
            Arc::new(InMemoryJournalWriter::new()),
            Arc::new(NoopObserver),
        );

        // tokio runtime to drive the async execute.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt.block_on(exec.execute(&compiled));
        if let Ok(results) = outcome {
            prop_assert!(matches!(
                results.status,
                ExecutionStatus::Succeeded
                    | ExecutionStatus::PartialFailure
                    | ExecutionStatus::Halted
                    | ExecutionStatus::Cancelled,
            ));
        }
        // Err variants are fine — only panic/hang would fail the test.
    }
}
