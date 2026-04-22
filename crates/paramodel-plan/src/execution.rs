// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ExecutionGraph`, `ExecutionPlan`, and associated metadata.
//!
//! Per SRD-0009: `ExecutionGraph` is a `Vec<AtomicStep>` with
//! edges implicit in `step.header.depends_on`. `ExecutionPlan` wraps
//! both graphs plus compile-time metadata and is pure data — the
//! executor trait consumes `&ExecutionPlan` without a collaborating
//! `execute()` method on the struct itself.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{ElementName, Fingerprint, Labels, Tags, TrialId};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::{PlanError, Result};
use crate::ids::TestPlanId;
use crate::instance::ElementInstanceGraph;
use crate::ordering::{OptimizationStrategy, TrialOrdering};
use crate::step::{AtomicStep, StepId};

// ---------------------------------------------------------------------------
// ResourceRequirements.
// ---------------------------------------------------------------------------

/// Typed resource ask. Attached to a single step (`StepHeader`) and
/// aggregated onto the whole plan as the peak across the graph.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ResourceRequirements {
    /// Peak CPU cores across concurrent steps.
    #[builder(default)]
    pub peak_cpu_cores: f64,
    /// Peak RSS-style memory in megabytes.
    #[builder(default)]
    pub peak_memory_mb: u64,
    /// Peak storage in gigabytes.
    #[builder(default)]
    pub peak_storage_gb: u64,
    /// Peak network throughput in gigabits per second.
    #[builder(default)]
    pub peak_network_gbps: f64,
    /// Adopter-defined resources (`"gpu"`, `"license_seats"`, …).
    #[builder(default)]
    pub custom: BTreeMap<String, u64>,
}

impl ResourceRequirements {
    /// All-zeros requirements.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// CheckpointStrategy.
// ---------------------------------------------------------------------------

/// When / how often the compiler emits `AtomicStep::Checkpoint` nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct CheckpointStrategy {
    /// Wall-clock interval between checkpoints. `None` disables
    /// interval-based checkpointing.
    pub interval: Option<Duration>,
    /// Insert a checkpoint at every barrier.
    #[builder(default)]
    pub checkpoint_on_barriers: bool,
    /// Insert a checkpoint when a step errors.
    #[builder(default)]
    pub checkpoint_on_errors: bool,
    /// Maximum checkpoints to retain. `None` = unbounded.
    pub max_checkpoints: Option<u32>,
}

// ---------------------------------------------------------------------------
// ExecutionGraph.
// ---------------------------------------------------------------------------

/// The step DAG produced by the compiler.
///
/// Order of the inner `Vec<AtomicStep>` is not semantically meaningful
/// — edges live in each step's `header.depends_on`. Accessors compute
/// traversals on demand.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionGraph {
    steps: Vec<AtomicStep>,
}

impl ExecutionGraph {
    /// Construct from a step list without validation. Intended for
    /// deserialisation; compiler-produced graphs go through
    /// [`Self::new`], which validates.
    #[must_use]
    pub const fn from_steps_unchecked(steps: Vec<AtomicStep>) -> Self {
        Self { steps }
    }

    /// Construct and validate.
    pub fn new(steps: Vec<AtomicStep>) -> Result<Self> {
        let g = Self::from_steps_unchecked(steps);
        g.validate()?;
        Ok(g)
    }

    /// Borrow the steps in their stored order.
    #[must_use]
    pub fn steps(&self) -> &[AtomicStep] {
        &self.steps
    }

    /// Look up a step by id.
    #[must_use]
    pub fn step(&self, id: &StepId) -> Option<&AtomicStep> {
        self.steps.iter().find(|s| s.id() == id)
    }

    /// Direct predecessors of `id`.
    #[must_use]
    pub fn dependencies(&self, id: &StepId) -> Vec<&AtomicStep> {
        let Some(step) = self.step(id) else {
            return Vec::new();
        };
        step.depends_on()
            .iter()
            .filter_map(|d| self.step(d))
            .collect()
    }

    /// Transitive predecessors of `id` (excluding `id` itself).
    #[must_use]
    pub fn transitive_dependencies(&self, id: &StepId) -> Vec<&AtomicStep> {
        let mut out: Vec<&AtomicStep> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<&StepId> = VecDeque::new();
        queue.push_back(id);
        seen.insert(id.as_str());
        while let Some(curr) = queue.pop_front() {
            for dep in self.dependencies(curr) {
                if seen.insert(dep.id().as_str()) {
                    out.push(dep);
                    queue.push_back(dep.id());
                }
            }
        }
        out
    }

    /// Direct dependents of `id`.
    #[must_use]
    pub fn dependents(&self, id: &StepId) -> Vec<&AtomicStep> {
        self.steps
            .iter()
            .filter(|s| s.depends_on().iter().any(|d| d == id))
            .collect()
    }

    /// Transitive dependents of `id`.
    #[must_use]
    pub fn transitive_dependents(&self, id: &StepId) -> Vec<&AtomicStep> {
        let mut out: Vec<&AtomicStep> = Vec::new();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut queue: VecDeque<&StepId> = VecDeque::new();
        queue.push_back(id);
        seen.insert(id.as_str());
        while let Some(curr) = queue.pop_front() {
            for dep in self.dependents(curr) {
                if seen.insert(dep.id().as_str()) {
                    out.push(dep);
                    queue.push_back(dep.id());
                }
            }
        }
        out
    }

    /// Kahn's-algorithm topological sort. Errors on a cycle.
    pub fn topological_sort(&self) -> Result<Vec<&AtomicStep>> {
        self.validate()?;
        let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.steps {
            indegree.insert(s.id().as_str(), 0);
        }
        for s in &self.steps {
            for d in s.depends_on() {
                if let Some(entry) = indegree.get_mut(s.id().as_str()) {
                    let _ = d; // placeholder
                    *entry += 1;
                }
            }
            // Walk dependents: each dep adds to the dependent's in-degree.
            // We already added for `s` above — correct count = number of
            // dependencies.
        }
        // The block above double-counted; rewrite cleanly:
        indegree.clear();
        for s in &self.steps {
            indegree.insert(s.id().as_str(), s.depends_on().len());
        }
        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| *k)
            .collect();
        ready.sort_unstable();
        let mut out: Vec<&AtomicStep> = Vec::with_capacity(self.steps.len());
        while let Some(k) = ready.pop() {
            if let Some(step) = self.steps.iter().find(|s| s.id().as_str() == k) {
                out.push(step);
            }
            let next: Vec<&str> = self
                .steps
                .iter()
                .filter(|s| s.depends_on().iter().any(|d| d.as_str() == k))
                .map(|s| s.id().as_str())
                .collect();
            for n in next {
                if let Some(d) = indegree.get_mut(n) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        ready.push(n);
                    }
                }
            }
        }
        if out.len() != self.steps.len() {
            return Err(PlanError::GraphHasCycle);
        }
        Ok(out)
    }

    /// Parallel waves — BFS layers by longest-path depth from
    /// source-free nodes. Returns one inner vec per wave.
    pub fn parallel_waves(&self) -> Result<Vec<Vec<&AtomicStep>>> {
        self.validate()?;
        let mut depth: BTreeMap<&str, u32> = BTreeMap::new();
        for s in &self.steps {
            depth.insert(s.id().as_str(), 0);
        }
        let sorted = self.topological_sort()?;
        for s in &sorted {
            let base = *depth.get(s.id().as_str()).unwrap_or(&0);
            let dependents = self.dependents(s.id());
            for d in dependents {
                let entry = depth.entry(d.id().as_str()).or_insert(0);
                *entry = (*entry).max(base + 1);
            }
        }
        let max_depth = *depth.values().max().unwrap_or(&0);
        let mut waves: Vec<Vec<&AtomicStep>> =
            (0..=max_depth).map(|_| Vec::new()).collect();
        for s in &self.steps {
            let lvl = *depth.get(s.id().as_str()).unwrap_or(&0);
            waves[lvl as usize].push(s);
        }
        waves.retain(|w| !w.is_empty());
        Ok(waves)
    }

    /// Longest-path (by count) from a source-free node to a
    /// sink-free one. Ties broken by authoring order.
    pub fn critical_path(&self) -> Result<Vec<&AtomicStep>> {
        let sorted = self.topological_sort()?;
        if sorted.is_empty() {
            return Ok(Vec::new());
        }
        let mut best_len: BTreeMap<&str, u32> = BTreeMap::new();
        let mut prev: BTreeMap<&str, Option<&str>> = BTreeMap::new();
        for s in &sorted {
            best_len.insert(s.id().as_str(), 1);
            prev.insert(s.id().as_str(), None);
        }
        for s in &sorted {
            let me = *best_len.get(s.id().as_str()).unwrap_or(&1);
            for dep in self.dependents(s.id()) {
                let entry = best_len.entry(dep.id().as_str()).or_insert(1);
                if me + 1 > *entry {
                    *entry = me + 1;
                    prev.insert(dep.id().as_str(), Some(s.id().as_str()));
                }
            }
        }
        let (tail, _) = best_len.iter().max_by_key(|(_, v)| **v).expect("non-empty");
        let mut path_ids: Vec<&str> = Vec::new();
        let mut cursor = Some(*tail);
        while let Some(k) = cursor {
            path_ids.push(k);
            cursor = prev.get(k).copied().flatten();
        }
        path_ids.reverse();
        let mut path = Vec::with_capacity(path_ids.len());
        for k in path_ids {
            if let Some(s) = self.steps.iter().find(|s| s.id().as_str() == k) {
                path.push(s);
            }
        }
        Ok(path)
    }

    /// Widest parallel wave.
    pub fn maximum_parallelism(&self) -> Result<usize> {
        Ok(self
            .parallel_waves()?
            .into_iter()
            .map(|w| w.len())
            .max()
            .unwrap_or(0))
    }

    /// Average wave width.
    pub fn average_parallelism(&self) -> Result<f64> {
        let waves = self.parallel_waves()?;
        if waves.is_empty() {
            return Ok(0.0);
        }
        #[allow(
            clippy::cast_precision_loss,
            reason = "graph sizes stay small enough for f64 approximation"
        )]
        let avg = self.steps.len() as f64 / waves.len() as f64;
        Ok(avg)
    }

    /// `true` when the graph contains no cycles.
    #[must_use]
    pub fn is_acyclic(&self) -> bool {
        self.topological_sort().is_ok()
    }

    /// Convenience filter for barrier steps.
    pub fn barriers(&self) -> impl Iterator<Item = &AtomicStep> {
        self.steps.iter().filter(|s| s.is_barrier())
    }

    /// Convenience filter for checkpoint steps.
    pub fn checkpoints(&self) -> impl Iterator<Item = &AtomicStep> {
        self.steps.iter().filter(|s| s.is_checkpoint())
    }

    /// Filter steps that target the given element.
    #[must_use]
    pub fn subgraph_for_element(&self, element: &ElementName) -> Self {
        Self::from_steps_unchecked(
            self.steps
                .iter()
                .filter(|s| s.element() == Some(element))
                .cloned()
                .collect(),
        )
    }

    /// Filter steps whose trial id is in `trial_ids`.
    #[must_use]
    pub fn subgraph_for_trials(&self, trial_ids: &[TrialId]) -> Self {
        Self::from_steps_unchecked(
            self.steps
                .iter()
                .filter(|s| {
                    s.trial_id()
                        .is_some_and(|t| trial_ids.iter().any(|x| x == t))
                })
                .cloned()
                .collect(),
        )
    }

    /// Aggregate statistics.
    pub fn statistics(&self) -> Result<GraphStatistics> {
        let waves = self.parallel_waves()?;
        let max_p = waves.iter().map(Vec::len).max().unwrap_or(0);
        let edge_count: usize = self.steps.iter().map(|s| s.depends_on().len()).sum();
        let max_fan_in = self.steps.iter().map(|s| s.depends_on().len()).max().unwrap_or(0);
        let max_fan_out = self
            .steps
            .iter()
            .map(|s| self.dependents(s.id()).len())
            .max()
            .unwrap_or(0);
        let max_depth = u32::try_from(waves.len().saturating_sub(1)).unwrap_or(u32::MAX);
        #[allow(
            clippy::cast_precision_loss,
            reason = "graph sizes stay small enough for f64 approximation"
        )]
        let avg_degree = if self.steps.is_empty() {
            0.0
        } else {
            (edge_count * 2) as f64 / self.steps.len() as f64
        };
        Ok(GraphStatistics {
            node_count:             u32::try_from(self.steps.len()).unwrap_or(u32::MAX),
            edge_count:             u32::try_from(edge_count).unwrap_or(u32::MAX),
            max_depth,
            max_fan_out:            u32::try_from(max_fan_out).unwrap_or(u32::MAX),
            max_fan_in:             u32::try_from(max_fan_in).unwrap_or(u32::MAX),
            average_degree:         avg_degree,
            critical_path_duration: None,
            total_duration:         None,
            maximum_parallelism:    u32::try_from(max_p).unwrap_or(u32::MAX),
            #[allow(
                clippy::cast_precision_loss,
                reason = "graph sizes stay small enough for f64 approximation"
            )]
            average_parallelism:    if waves.is_empty() {
                0.0
            } else {
                self.steps.len() as f64 / waves.len() as f64
            },
        })
    }

    /// Content fingerprint — walks steps in `StepId` order and hashes
    /// their canonical byte forms. Labels/tags/metadata stay in the
    /// step headers and contribute to the hash because they're
    /// authored into the graph.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut ordered: Vec<&AtomicStep> = self.steps.iter().collect();
        ordered.sort_by(|a, b| a.id().cmp(b.id()));
        let bytes = serde_json::to_vec(&ordered).expect("steps serialise");
        Fingerprint::of(&bytes)
    }

    /// Structural validation — unique step ids, every `depends_on`
    /// resolves, acyclic.
    pub fn validate(&self) -> Result<()> {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for s in &self.steps {
            if !seen.insert(s.id().as_str()) {
                return Err(PlanError::DuplicateStepId {
                    id: s.id().as_str().to_owned(),
                });
            }
        }
        for s in &self.steps {
            for d in s.depends_on() {
                if !seen.contains(d.as_str()) {
                    return Err(PlanError::DanglingDependency {
                        step:    s.id().as_str().to_owned(),
                        missing: d.as_str().to_owned(),
                    });
                }
            }
        }
        // Cycle detection via a local topological walk; don't recurse
        // through Self::topological_sort to avoid re-validating.
        let mut indegree: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &self.steps {
            indegree.insert(s.id().as_str(), s.depends_on().len());
        }
        let mut ready: Vec<&str> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| *k)
            .collect();
        let mut visited = 0usize;
        while let Some(k) = ready.pop() {
            visited += 1;
            for s in &self.steps {
                if s.depends_on().iter().any(|x| x.as_str() == k)
                    && let Some(d) = indegree.get_mut(s.id().as_str())
                {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        ready.push(s.id().as_str());
                    }
                }
            }
        }
        if visited != self.steps.len() {
            return Err(PlanError::GraphHasCycle);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GraphStatistics.
// ---------------------------------------------------------------------------

/// Compile-time statistics about an `ExecutionGraph`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphStatistics {
    /// Number of steps.
    pub node_count: u32,
    /// Total edge count.
    pub edge_count: u32,
    /// Longest path length in hops.
    pub max_depth: u32,
    /// Maximum out-degree.
    pub max_fan_out: u32,
    /// Maximum in-degree.
    pub max_fan_in: u32,
    /// Average total degree.
    pub average_degree: f64,
    /// Critical-path duration, when available.
    pub critical_path_duration: Option<Duration>,
    /// Sum-of-estimated-durations, when available.
    pub total_duration: Option<Duration>,
    /// Widest wave.
    pub maximum_parallelism: u32,
    /// `nodes / waves`.
    pub average_parallelism: f64,
}

// ---------------------------------------------------------------------------
// ExecutionPlanId.
// ---------------------------------------------------------------------------

/// Unique identifier for a compiled `ExecutionPlan`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ExecutionPlanId(Ulid);

impl ExecutionPlanId {
    /// From a pre-generated ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }

    /// Borrow the inner ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> &Ulid {
        &self.0
    }
}

impl std::fmt::Display for ExecutionPlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ---------------------------------------------------------------------------
// ExecutionPlanMetadata and friends.
// ---------------------------------------------------------------------------

/// Compile-time facts attached to an `ExecutionPlan`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ExecutionPlanMetadata {
    /// When the compiler produced the plan.
    pub compiled_at: Timestamp,
    /// How long compilation took.
    pub compilation_duration: Duration,
    /// Compiler version/tag.
    pub compiler_version: String,
    /// Optimisation level that was applied.
    pub optimization_level: OptimizationStrategy,

    /// Aggregate counts.
    pub trial_count: u32,
    /// Step count.
    pub step_count: u32,
    /// Barrier count.
    pub barrier_count: u32,
    /// Element-instance count.
    pub element_instance_count: u32,

    /// Plan-wide estimated duration.
    pub estimated_duration: Option<Duration>,

    /// Parallelism / critical-path metrics.
    pub performance_metrics: PerformanceMetrics,

    /// Optimisation summary for UI display.
    pub optimization_report: Option<OptimizationReport>,

    /// Adopter-defined metadata.
    #[builder(default)]
    pub custom: BTreeMap<String, String>,
}

/// Summary parallelism metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// Critical-path duration.
    pub critical_path_duration: Option<Duration>,
    /// Sum-of-estimated-durations.
    pub total_duration: Option<Duration>,
    /// Widest wave.
    pub maximum_parallelism: u32,
    /// `nodes / waves`.
    pub average_parallelism: f64,
    /// `total / critical` if both are known, else 1.0.
    pub speedup_factor: f64,
}

/// Compiler-supplied summary of the optimisations that were applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, bon::Builder)]
pub struct OptimizationReport {
    /// Per-trial steps eliminated.
    #[builder(default)]
    pub per_trial_steps_eliminated: u32,
    /// Group coalescings applied.
    #[builder(default)]
    pub group_coalescings_applied: u32,
    /// Transitive edges removed.
    #[builder(default)]
    pub transitive_edges_removed: u32,
    /// Barriers coalesced.
    #[builder(default)]
    pub barriers_coalesced: u32,
    /// Deploys saved.
    #[builder(default)]
    pub deploys_saved: u32,
    /// Free-form notes for UI display.
    #[builder(default)]
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// ExecutionPlan.
// ---------------------------------------------------------------------------

/// Compiled plan wrapping the element-instance graph and the execution
/// graph, plus compile-time metadata and provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, bon::Builder)]
pub struct ExecutionPlan {
    /// ULID identifier for this compiled plan.
    pub id: ExecutionPlanId,

    /// Source `TestPlan` fingerprint — results trace back through this.
    pub source_plan_fingerprint: Fingerprint,
    /// Source `TestPlan` id.
    pub source_plan_id: TestPlanId,

    /// The step DAG.
    pub execution_graph: ExecutionGraph,

    /// The middle graph.
    pub element_instance_graph: ElementInstanceGraph,

    /// Peak resource ask across the plan.
    #[builder(default)]
    pub resource_requirements: ResourceRequirements,

    /// When / how to checkpoint.
    pub checkpoint_strategy: Option<CheckpointStrategy>,

    /// Traversal policy carried through from the `TestPlan`.
    #[builder(default)]
    pub trial_ordering: TrialOrdering,

    /// Names of elements reducto identified as trial elements.
    #[builder(default)]
    pub trial_elements: Vec<ElementName>,

    /// Plan-wide concurrency cap. When set, feeds
    /// `AtomicStep::Deploy.max_concurrency` for steps that didn't
    /// override it.
    pub max_concurrency: Option<u32>,

    /// Compile-time metadata.
    pub metadata: ExecutionPlanMetadata,

    /// Intrinsic facts.
    #[builder(default)]
    pub labels: Labels,

    /// Organisational tags.
    #[builder(default)]
    pub tags: Tags,
}

impl ExecutionPlan {
    /// Borrow the underlying step list.
    #[must_use]
    pub fn steps(&self) -> &[AtomicStep] {
        self.execution_graph.steps()
    }

    /// Barrier convenience filter.
    pub fn barriers(&self) -> impl Iterator<Item = &AtomicStep> {
        self.execution_graph.barriers()
    }

    /// Checkpoint convenience filter.
    pub fn checkpoints(&self) -> impl Iterator<Item = &AtomicStep> {
        self.execution_graph.checkpoints()
    }

    /// Return a new plan with `max_concurrency` overridden. Other
    /// fields are untouched; the result has the same `id` as the
    /// source (caller substitutes if they want a fresh id).
    #[must_use]
    pub fn with_max_concurrency(&self, n: u32) -> Self {
        let mut next = self.clone();
        next.max_concurrency = Some(n);
        next
    }

    /// Content fingerprint.
    ///
    /// Covers `source_plan_fingerprint`, both graph fingerprints,
    /// resource requirements, checkpoint strategy, trial ordering,
    /// trial elements, and `max_concurrency`. Excludes `id`, labels,
    /// tags, and metadata.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        let mut out = Vec::new();
        out.push(EXECUTION_PLAN_TAG);
        out.extend_from_slice(self.source_plan_fingerprint.as_bytes());
        out.extend_from_slice(self.execution_graph.fingerprint().as_bytes());
        // Instance graph fingerprint uses the same pattern — JSON then hash,
        // computed inline here so instance.rs stays free of hashing concerns.
        let instance_bytes = serde_json::to_vec(&self.element_instance_graph)
            .expect("instance graph serialises");
        let instance_fp = Fingerprint::of(&instance_bytes);
        out.extend_from_slice(instance_fp.as_bytes());
        out.extend_from_slice(
            &serde_json::to_vec(&self.resource_requirements)
                .expect("resource requirements serialise"),
        );
        out.extend_from_slice(
            &serde_json::to_vec(&self.checkpoint_strategy)
                .expect("checkpoint strategy serialises"),
        );
        out.extend_from_slice(
            &serde_json::to_vec(&self.trial_ordering).expect("trial ordering serialises"),
        );
        // trial elements sorted by name for stable ordering.
        let mut sorted_elements: Vec<&ElementName> = self.trial_elements.iter().collect();
        sorted_elements.sort();
        out.extend_from_slice(
            &u32::try_from(sorted_elements.len())
                .expect("count fits")
                .to_le_bytes(),
        );
        for e in sorted_elements {
            let bytes = e.as_str().as_bytes();
            out.extend_from_slice(
                &u32::try_from(bytes.len()).expect("length fits").to_le_bytes(),
            );
            out.extend_from_slice(bytes);
        }
        match self.max_concurrency {
            Some(n) => {
                out.push(0x01);
                out.extend_from_slice(&n.to_le_bytes());
            }
            None => out.push(0x00),
        }
        Fingerprint::of(&out)
    }
}

const EXECUTION_PLAN_TAG: u8 = 0x21;

#[cfg(test)]
mod tests {
    use paramodel_elements::ResolvedConfiguration;

    use super::*;
    use crate::step::{AtomicStep, ShutdownReason, StepHeader, StepId};
    use crate::InstanceScope;

    fn sid(s: &str) -> StepId {
        StepId::new(s).unwrap()
    }
    fn tid(n: u64) -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(n, 1))
    }
    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }

    fn header(id: &str, deps: &[&str]) -> StepHeader {
        StepHeader::builder()
            .id(sid(id))
            .depends_on(deps.iter().map(|d| sid(d)).collect())
            .reason("test".to_owned())
            .build()
    }

    fn deploy(name: &str, deps: &[&str]) -> AtomicStep {
        AtomicStep::Deploy {
            header:                header(name, deps),
            element:               ename("db"),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        }
    }

    // ---------- ExecutionGraph validation ----------

    #[test]
    fn validate_accepts_linear_chain() {
        let g = ExecutionGraph::new(vec![
            deploy("a", &[]),
            deploy("b", &["a"]),
            deploy("c", &["b"]),
        ])
        .unwrap();
        assert!(g.is_acyclic());
        assert_eq!(g.steps().len(), 3);
    }

    #[test]
    fn validate_rejects_duplicate_ids() {
        let res = ExecutionGraph::new(vec![deploy("a", &[]), deploy("a", &[])]);
        assert!(matches!(res, Err(PlanError::DuplicateStepId { .. })));
    }

    #[test]
    fn validate_rejects_dangling_dependency() {
        let res = ExecutionGraph::new(vec![deploy("a", &["ghost"])]);
        assert!(matches!(res, Err(PlanError::DanglingDependency { .. })));
    }

    #[test]
    fn validate_rejects_cycle() {
        let res = ExecutionGraph::new(vec![deploy("a", &["b"]), deploy("b", &["a"])]);
        assert!(matches!(res, Err(PlanError::GraphHasCycle)));
    }

    // ---------- accessors ----------

    #[test]
    fn dependencies_and_dependents() {
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
    fn topological_sort_orders_after_dependencies() {
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
    }

    #[test]
    fn parallel_waves_group_independent_steps() {
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
    fn critical_path_is_longest() {
        let g = ExecutionGraph::new(vec![
            deploy("a", &[]),
            deploy("b", &["a"]),
            deploy("c", &["b"]),
            deploy("side", &["a"]),
        ])
        .unwrap();
        let cp = g.critical_path().unwrap();
        assert_eq!(cp.len(), 3);
    }

    // ---------- subgraph filters + barriers accessor ----------

    #[test]
    fn subgraph_for_trials_filters() {
        let steps = vec![
            AtomicStep::TrialStart {
                header:        header("ts_t0", &[]),
                trial_id:      tid(1),
                element_names: vec![],
            },
            AtomicStep::TrialEnd {
                header:          header("te_t0", &["ts_t0"]),
                trial_id:        tid(1),
                element_names:   vec![],
                shutdown_reason: ShutdownReason::Normal,
            },
        ];
        let g = ExecutionGraph::new(steps).unwrap();
        let only_one = g.subgraph_for_trials(&[tid(1)]);
        assert_eq!(only_one.steps().len(), 2);
        let none = g.subgraph_for_trials(&[tid(42)]);
        assert_eq!(none.steps().len(), 0);
    }

    // ---------- ExecutionPlan ----------

    fn minimal_plan() -> ExecutionPlan {
        let g = ExecutionGraph::new(vec![deploy("a", &[])]).unwrap();
        let instance_graph = ElementInstanceGraph::default();
        ExecutionPlan::builder()
            .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .source_plan_fingerprint(Fingerprint::of(b"src"))
            .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
            .execution_graph(g)
            .element_instance_graph(instance_graph)
            .metadata(
                ExecutionPlanMetadata::builder()
                    .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                    .compilation_duration(Duration::from_secs(1))
                    .compiler_version("0.1.0".to_owned())
                    .optimization_level(OptimizationStrategy::Basic)
                    .trial_count(0)
                    .step_count(1)
                    .barrier_count(0)
                    .element_instance_count(0)
                    .estimated_duration(Duration::from_secs(5))
                    .performance_metrics(PerformanceMetrics {
                        critical_path_duration: None,
                        total_duration:         None,
                        maximum_parallelism:    1,
                        average_parallelism:    1.0,
                        speedup_factor:         1.0,
                    })
                    .build(),
            )
            .build()
    }

    #[test]
    fn plan_accessors_forward_to_graph() {
        let p = minimal_plan();
        assert_eq!(p.steps().len(), 1);
        assert_eq!(p.barriers().count(), 0);
    }

    #[test]
    fn with_max_concurrency_returns_new_plan_with_cap() {
        let p = minimal_plan();
        assert!(p.max_concurrency.is_none());
        let capped = p.with_max_concurrency(4);
        assert_eq!(capped.max_concurrency, Some(4));
        assert!(p.max_concurrency.is_none());
    }

    #[test]
    fn plan_fingerprint_is_deterministic_and_excludes_labels() {
        let p1 = minimal_plan();
        let p2 = minimal_plan();
        let fp1 = p1.fingerprint();
        let fp2 = p2.fingerprint();
        assert_eq!(fp1, fp2);
        let mut p3 = minimal_plan();
        p3.labels.insert(
            paramodel_elements::LabelKey::new("owner").unwrap(),
            paramodel_elements::LabelValue::new("ops").unwrap(),
        );
        assert_eq!(p3.fingerprint(), fp1);
    }

    #[test]
    fn instance_scope_serde() {
        let s = InstanceScope::Trial;
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"trial\"");
    }
}
