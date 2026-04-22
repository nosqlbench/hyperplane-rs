// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionGraph` accessor coverage — `dependencies`, `dependents`,
//! `transitive_*`, `parallel_waves`, `critical_path`, `statistics`.

use paramodel_elements::{ElementName, ResolvedConfiguration};
use paramodel_plan::{
    AtomicStep, ExecutionGraph, StepHeader, StepId,
};

fn ename() -> ElementName {
    ElementName::new("db").unwrap()
}

fn deploy(id: &str, deps: &[&str]) -> AtomicStep {
    AtomicStep::Deploy {
        header:                StepHeader::builder()
            .id(StepId::new(id).unwrap())
            .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
            .reason("t".to_owned())
            .build(),
        element:               ename(),
        instance_number:       0,
        configuration:         ResolvedConfiguration::new(),
        max_concurrency:       None,
        max_group_concurrency: None,
        dedicated_to:          None,
    }
}

fn sid(s: &str) -> StepId {
    StepId::new(s).unwrap()
}

// ---------------------------------------------------------------------------
// Direct and transitive dependency / dependent traversal.
// ---------------------------------------------------------------------------

#[test]
fn dependencies_and_dependents_round_trip() {
    // Diamond: a → b → d and a → c → d.
    let g = ExecutionGraph::new(vec![
        deploy("a", &[]),
        deploy("b", &["a"]),
        deploy("c", &["a"]),
        deploy("d", &["b", "c"]),
    ])
    .unwrap();
    assert_eq!(g.dependencies(&sid("d")).len(), 2);
    assert_eq!(g.dependents(&sid("a")).len(), 2);
    assert_eq!(g.transitive_dependents(&sid("a")).len(), 3);
    assert_eq!(g.transitive_dependencies(&sid("d")).len(), 3);
}

#[test]
fn topological_sort_orders_respect_edges() {
    let g = ExecutionGraph::new(vec![
        deploy("a", &[]),
        deploy("b", &["a"]),
        deploy("c", &["a", "b"]),
    ])
    .unwrap();
    let sorted = g.topological_sort().unwrap();
    let ids: Vec<&str> = sorted.iter().map(|s| s.id().as_str()).collect();
    let pos = |x: &str| ids.iter().position(|y| *y == x).unwrap();
    assert!(pos("a") < pos("b"));
    assert!(pos("b") < pos("c"));
    assert!(pos("a") < pos("c"));
}

// ---------------------------------------------------------------------------
// Parallel waves / critical path / max-parallelism.
// ---------------------------------------------------------------------------

#[test]
fn parallel_waves_groups_independent_steps() {
    // a and b are independent; c depends on both.
    let g = ExecutionGraph::new(vec![
        deploy("a", &[]),
        deploy("b", &[]),
        deploy("c", &["a", "b"]),
    ])
    .unwrap();
    let waves = g.parallel_waves().unwrap();
    assert_eq!(waves.len(), 2);
    assert_eq!(waves[0].len(), 2);
    assert_eq!(waves[1].len(), 1);
    assert_eq!(g.maximum_parallelism().unwrap(), 2);
}

#[test]
fn critical_path_is_longest_chain() {
    // Longest: a → b → c (length 3). Off-path: a → side (length 2).
    let g = ExecutionGraph::new(vec![
        deploy("a", &[]),
        deploy("b", &["a"]),
        deploy("c", &["b"]),
        deploy("side", &["a"]),
    ])
    .unwrap();
    let cp = g.critical_path().unwrap();
    assert_eq!(cp.len(), 3);
    let ids: Vec<&str> = cp.iter().map(|s| s.id().as_str()).collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
}

// ---------------------------------------------------------------------------
// Statistics accessors.
// ---------------------------------------------------------------------------

#[test]
fn statistics_match_hand_computed() {
    let g = ExecutionGraph::new(vec![
        deploy("a", &[]),
        deploy("b", &["a"]),
        deploy("c", &["a"]),
        deploy("d", &["b", "c"]),
    ])
    .unwrap();
    let stats = g.statistics().unwrap();
    assert_eq!(stats.node_count, 4);
    // Edges: a←b, a←c, b←d, c←d = 4.
    assert_eq!(stats.edge_count, 4);
    // Deepest: a → b → d or a → c → d → 3 levels = max_depth 2
    // (0-indexed wave count − 1).
    assert_eq!(stats.max_depth, 2);
    assert_eq!(stats.maximum_parallelism, 2, "wave b,c is widest");
}

// ---------------------------------------------------------------------------
// step() accessor returns None for unknown id.
// ---------------------------------------------------------------------------

#[test]
fn step_accessor_returns_none_for_unknown() {
    let g = ExecutionGraph::new(vec![deploy("a", &[])]).unwrap();
    assert!(g.step(&sid("ghost")).is_none());
    assert!(g.step(&sid("a")).is_some());
}
