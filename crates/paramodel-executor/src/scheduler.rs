// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Scheduler trait + `DefaultScheduler` (FIFO).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use async_trait::async_trait;
use jiff::Timestamp;
use paramodel_elements::TrialId;
use paramodel_plan::{ExecutionPlan, ResourceRequirements, StepId};
use paramodel_trials::ErrorInfo;
use serde::{Deserialize, Serialize};

use crate::error::SchedulerError;
use crate::resources::ResourceSnapshot;

/// Per-step priority. Lexicographic over `Low < Normal < High <
/// Critical`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerPriority {
    /// Background work.
    Low,
    /// Normal priority.
    #[default]
    Normal,
    /// Elevated priority.
    High,
    /// System-critical.
    Critical,
}

/// High-level scheduling policy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingPolicy {
    /// First-in-first-out over the ready set.
    Fifo,
    /// Critical-path first; higher priority wins.
    Priority,
    /// Round-robin across trials.
    Fair,
    /// Maximise resource utilisation.
    ResourceAware,
}

/// Scheduler snapshot — returned by `Scheduler::state`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerState {
    /// Pending.
    pub pending_count:   u32,
    /// Currently running.
    pub running_count:   u32,
    /// Terminal-success.
    pub completed_count: u32,
    /// Terminal-failure.
    pub failed_count:    u32,
    /// Terminal-skipped.
    pub skipped_count:   u32,
    /// Ready-to-run subset of pending.
    pub ready_count:     u32,
}

/// Aggregated counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchedulerStatistics {
    /// Total steps ever scheduled.
    pub total_dispatched: u32,
    /// Steps that ran to completion.
    pub total_completed:  u32,
    /// Steps that failed terminally.
    pub total_failed:     u32,
    /// Steps that were skipped.
    pub total_skipped:    u32,
}

// ---------------------------------------------------------------------------
// Scheduler trait.
// ---------------------------------------------------------------------------

/// Picks the next ready steps.
#[async_trait]
pub trait Scheduler: Send + Sync + 'static {
    /// Initialise against a plan.
    async fn initialise(&self, plan: &ExecutionPlan) -> Result<(), SchedulerError>;

    /// Drain up to `max_count` ready steps, honouring resources.
    async fn next_steps(
        &self,
        max_count: usize,
        resources: &ResourceSnapshot,
    ) -> Vec<StepId>;

    /// Step began execution.
    async fn mark_started(&self, step: &StepId, at: Timestamp);
    /// Step terminated successfully.
    async fn mark_completed(&self, step: &StepId, at: Timestamp);
    /// Step terminated unsuccessfully.
    ///
    /// Returns the step ids that cascaded into a terminal skipped
    /// state as a direct consequence — i.e. transitive dependents of
    /// `step` that were still pending/ready. Callers (the executor)
    /// can emit `StepSkipped` journal events for each.
    async fn mark_failed(
        &self,
        step: &StepId,
        at:   Timestamp,
        err:  &ErrorInfo,
    ) -> Vec<StepId>;
    /// Step was skipped.
    async fn mark_skipped(&self, step: &StepId, at: Timestamp, reason: &str);

    /// Override per-step priority.
    fn set_priority(&self, step: &StepId, priority: SchedulerPriority);
    /// Current priority.
    fn get_priority(&self, step: &StepId) -> SchedulerPriority;

    /// `true` when every step has reached a terminal state.
    fn is_complete(&self) -> bool;

    /// Snapshot counts.
    fn state(&self) -> SchedulerState;

    /// Aggregate totals.
    fn statistics(&self) -> SchedulerStatistics;
}

// ---------------------------------------------------------------------------
// DefaultScheduler — simple FIFO over topological order.
// ---------------------------------------------------------------------------

/// Reference scheduler with dependency tracking.
///
/// Supports [`SchedulingPolicy::Fifo`] (default) and
/// [`SchedulingPolicy::Priority`]; `Fair` and `ResourceAware`
/// degrade to FIFO for now.
///
/// Ignores the `resources` snapshot (every step is admitted if its
/// dependencies are satisfied). Rule 6 concurrency annotations are
/// checked but not enforced globally.
#[derive(Debug)]
pub struct DefaultScheduler {
    state:  Mutex<InnerState>,
    policy: SchedulingPolicy,
}

impl Default for DefaultScheduler {
    fn default() -> Self {
        Self {
            state:  Mutex::new(InnerState::default()),
            policy: SchedulingPolicy::Fifo,
        }
    }
}

#[derive(Debug, Default)]
struct InnerState {
    pending:      BTreeSet<StepId>,
    ready:        BTreeSet<StepId>,
    running:      BTreeSet<StepId>,
    completed:    BTreeSet<StepId>,
    failed:       BTreeSet<StepId>,
    skipped:      BTreeSet<StepId>,
    deps:         BTreeMap<StepId, Vec<StepId>>,
    dependents:   BTreeMap<StepId, Vec<StepId>>,
    priorities:   BTreeMap<StepId, SchedulerPriority>,
    /// Per-step resource ask, captured at `initialise`. `next_steps`
    /// consults this against the caller's `ResourceSnapshot` to hold
    /// back steps that don't currently fit.
    requirements: BTreeMap<StepId, ResourceRequirements>,
    /// Per-step trial id, captured at `initialise`. Used by the
    /// `Fair` policy to group ready steps by trial for round-robin
    /// picking.
    trial_ids:    BTreeMap<StepId, Option<TrialId>>,
    /// Ordered list of trial ids seen at initialisation, used as the
    /// rotation order for the `Fair` policy.
    trial_order:  Vec<Option<TrialId>>,
    /// Next trial index to pick from under `Fair`; rotates through
    /// `trial_order` to give each trial a turn.
    fair_cursor:  usize,
    stats:        SchedulerStatistics,
}

impl DefaultScheduler {
    /// Construct with the default FIFO policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with an explicit scheduling policy.
    #[must_use]
    pub fn with_policy(policy: SchedulingPolicy) -> Self {
        Self {
            state: Mutex::new(InnerState::default()),
            policy,
        }
    }
}

#[async_trait]
impl Scheduler for DefaultScheduler {
    async fn initialise(&self, plan: &ExecutionPlan) -> Result<(), SchedulerError> {
        let mut inner = self.state.lock().expect("poisoned");
        *inner = InnerState::default();
        for step in plan.steps() {
            inner.pending.insert(step.id().clone());
            inner
                .deps
                .insert(step.id().clone(), step.depends_on().to_vec());
            inner.requirements.insert(
                step.id().clone(),
                step.header().resource_requirements.clone(),
            );
            let tid = step.trial_id().cloned();
            inner.trial_ids.insert(step.id().clone(), tid.clone());
            if !inner.trial_order.contains(&tid) {
                inner.trial_order.push(tid);
            }
            for d in step.depends_on() {
                inner
                    .dependents
                    .entry(d.clone())
                    .or_default()
                    .push(step.id().clone());
            }
        }
        // Ready set = pending with no outstanding deps.
        for step in plan.steps() {
            if step.depends_on().is_empty() {
                inner.ready.insert(step.id().clone());
            }
        }
        Ok(())
    }

    async fn next_steps(
        &self,
        max_count: usize,
        resources: &ResourceSnapshot,
    ) -> Vec<StepId> {
        let mut inner = self.state.lock().expect("poisoned");
        let mut picked = Vec::with_capacity(max_count);
        let mut ids: Vec<StepId> = inner.ready.iter().cloned().collect();
        // Order ready steps per the configured policy. BTreeSet
        // already sorts by id (FIFO on a deterministic ordering);
        // Priority sorts by `SchedulerPriority::Critical > High >
        // Normal > Low`, falling back to id for ties.
        match self.policy {
            SchedulingPolicy::Priority => {
                let priorities = inner.priorities.clone();
                ids.sort_by(|a, b| {
                    let pa = priorities.get(a).copied().unwrap_or_default();
                    let pb = priorities.get(b).copied().unwrap_or_default();
                    // Higher priority first (reverse), then id for
                    // stable ordering.
                    pb.cmp(&pa).then_with(|| a.cmp(b))
                });
            }
            SchedulingPolicy::Fair => {
                // Round-robin across trials: pick one step per trial
                // in rotation before taking a second from any trial.
                // The `fair_cursor` advances by `max_count` between
                // calls so successive `next_steps` calls don't all
                // start at trial 0.
                let trial_ids = inner.trial_ids.clone();
                let order = inner.trial_order.clone();
                let mut groups: BTreeMap<Option<TrialId>, Vec<StepId>> = BTreeMap::new();
                for sid in &ids {
                    let tid = trial_ids.get(sid).cloned().unwrap_or(None);
                    groups.entry(tid).or_default().push(sid.clone());
                }
                // Sort descending so `pop()` yields the smallest
                // id first — deterministic within a trial.
                for grp in groups.values_mut() {
                    grp.sort();
                    grp.reverse();
                }
                let n = order.len().max(1);
                let start_cursor = inner.fair_cursor % n;
                let mut cursor = start_cursor;
                let mut rotated: Vec<StepId> = Vec::with_capacity(ids.len());
                let mut empty_rounds: usize = 0;
                while rotated.len() < ids.len() && empty_rounds < n {
                    let key = &order[cursor];
                    if let Some(grp) = groups.get_mut(key)
                        && let Some(sid) = grp.pop()
                    {
                        rotated.push(sid);
                        empty_rounds = 0;
                    } else {
                        empty_rounds = empty_rounds.saturating_add(1);
                    }
                    cursor = (cursor + 1) % n;
                }
                // Advance the persistent cursor by max_count so the
                // next call starts from the next trial in rotation.
                inner.fair_cursor = (start_cursor + max_count) % n;
                ids = rotated;
            }
            SchedulingPolicy::ResourceAware => {
                // First-fit-decreasing on CPU: sort ready steps by
                // peak CPU ask (descending), ties broken by memory
                // then id. Combined with the `fits` gate below, this
                // picks the biggest job that fits first, reducing
                // leftover capacity and fragmentation.
                let requirements = inner.requirements.clone();
                ids.sort_by(|a, b| {
                    let ra = requirements.get(a).cloned().unwrap_or_default();
                    let rb = requirements.get(b).cloned().unwrap_or_default();
                    rb.peak_cpu_cores
                        .partial_cmp(&ra.peak_cpu_cores)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| rb.peak_memory_mb.cmp(&ra.peak_memory_mb))
                        .then_with(|| a.cmp(b))
                });
            }
            SchedulingPolicy::Fifo => {
                // BTreeSet iteration gives deterministic id order —
                // FIFO in spirit.
            }
        }
        // Track a running copy of the snapshot so sequential picks
        // don't over-subscribe the caller's budget.
        let mut remaining = resources.clone();
        for id in ids.into_iter() {
            if picked.len() >= max_count {
                break;
            }
            let req = inner
                .requirements
                .get(&id)
                .cloned()
                .unwrap_or_default();
            if !fits(&req, &remaining) {
                continue;
            }
            deduct(&mut remaining, &req);
            inner.ready.remove(&id);
            inner.pending.remove(&id);
            inner.running.insert(id.clone());
            picked.push(id);
        }
        inner.stats.total_dispatched = inner
            .stats
            .total_dispatched
            .saturating_add(u32::try_from(picked.len()).unwrap_or(u32::MAX));
        picked
    }

    async fn mark_started(&self, _step: &StepId, _at: Timestamp) {}

    async fn mark_completed(&self, step: &StepId, _at: Timestamp) {
        let mut inner = self.state.lock().expect("poisoned");
        inner.running.remove(step);
        // Defensive: a step restored from a checkpoint is seeded into
        // `completed` without first passing through `next_steps`, so
        // it may still be in `pending` / `ready`. Remove to keep the
        // sets disjoint.
        inner.ready.remove(step);
        inner.pending.remove(step);
        if inner.completed.insert(step.clone()) {
            inner.stats.total_completed = inner.stats.total_completed.saturating_add(1);
        }
        promote_ready_for(&mut inner, step);
    }

    async fn mark_failed(
        &self,
        step: &StepId,
        _at:  Timestamp,
        _err: &ErrorInfo,
    ) -> Vec<StepId> {
        let mut inner = self.state.lock().expect("poisoned");
        inner.running.remove(step);
        inner.ready.remove(step);
        inner.pending.remove(step);
        if inner.failed.insert(step.clone()) {
            inner.stats.total_failed = inner.stats.total_failed.saturating_add(1);
        }
        // Downstreams of a failed step become terminally skipped.
        let mut cascaded: Vec<StepId> = Vec::new();
        if let Some(children) = inner.dependents.clone().get(step) {
            let mut stack = children.clone();
            while let Some(c) = stack.pop() {
                if inner.pending.remove(&c) || inner.ready.remove(&c) {
                    inner.skipped.insert(c.clone());
                    inner.stats.total_skipped = inner.stats.total_skipped.saturating_add(1);
                    if let Some(grand) = inner.dependents.get(&c) {
                        stack.extend(grand.clone());
                    }
                    cascaded.push(c);
                }
            }
        }
        cascaded
    }

    async fn mark_skipped(&self, step: &StepId, _at: Timestamp, _reason: &str) {
        let mut inner = self.state.lock().expect("poisoned");
        inner.running.remove(step);
        inner.ready.remove(step);
        inner.pending.remove(step);
        if inner.skipped.insert(step.clone()) {
            inner.stats.total_skipped = inner.stats.total_skipped.saturating_add(1);
        }
        promote_ready_for(&mut inner, step);
    }

    fn set_priority(&self, step: &StepId, priority: SchedulerPriority) {
        self.state
            .lock()
            .expect("poisoned")
            .priorities
            .insert(step.clone(), priority);
    }

    fn get_priority(&self, step: &StepId) -> SchedulerPriority {
        self.state
            .lock()
            .expect("poisoned")
            .priorities
            .get(step)
            .copied()
            .unwrap_or_default()
    }

    fn is_complete(&self) -> bool {
        let inner = self.state.lock().expect("poisoned");
        inner.pending.is_empty() && inner.ready.is_empty() && inner.running.is_empty()
    }

    fn state(&self) -> SchedulerState {
        let inner = self.state.lock().expect("poisoned");
        SchedulerState {
            pending_count:   u32::try_from(inner.pending.len()).unwrap_or(u32::MAX),
            running_count:   u32::try_from(inner.running.len()).unwrap_or(u32::MAX),
            completed_count: u32::try_from(inner.completed.len()).unwrap_or(u32::MAX),
            failed_count:    u32::try_from(inner.failed.len()).unwrap_or(u32::MAX),
            skipped_count:   u32::try_from(inner.skipped.len()).unwrap_or(u32::MAX),
            ready_count:     u32::try_from(inner.ready.len()).unwrap_or(u32::MAX),
        }
    }

    fn statistics(&self) -> SchedulerStatistics {
        self.state.lock().expect("poisoned").stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use paramodel_plan::{ExecutionGraph, StepHeader};
    use ulid::Ulid;

    use super::*;
    use paramodel_elements::{
        Element, ElementName, Fingerprint, LabelValue, Labels, ResolvedConfiguration,
        attributes::label,
    };
    use paramodel_plan::{
        AtomicStep, ElementInstanceGraph, ExecutionPlan, ExecutionPlanId,
        ExecutionPlanMetadata, OptimizationStrategy, PerformanceMetrics, TestPlanId,
    };
    use std::time::Duration;

    fn svc() -> Labels {
        let mut l = Labels::new();
        l.insert(label::r#type(), LabelValue::new("service").unwrap());
        l
    }

    fn deploy(id: &str, deps: &[&str]) -> AtomicStep {
        AtomicStep::Deploy {
            header:                StepHeader::builder()
                .id(StepId::new(id).unwrap())
                .depends_on(deps.iter().map(|d| StepId::new(*d).unwrap()).collect())
                .reason("t".to_owned())
                .build(),
            element:               ElementName::new("db").unwrap(),
            instance_number:       0,
            configuration:         ResolvedConfiguration::new(),
            max_concurrency:       None,
            max_group_concurrency: None,
            dedicated_to:          None,
        }
    }

    fn plan(steps: Vec<AtomicStep>) -> ExecutionPlan {
        let _ = Element::builder()
            .name(ElementName::new("db").unwrap())
            .labels(svc())
            .build();
        ExecutionPlan::builder()
            .id(ExecutionPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .source_plan_fingerprint(Fingerprint::of(b"src"))
            .source_plan_id(TestPlanId::from_ulid(Ulid::from_parts(2, 1)))
            .execution_graph(ExecutionGraph::new(steps).unwrap())
            .element_instance_graph(ElementInstanceGraph::default())
            .metadata(
                ExecutionPlanMetadata::builder()
                    .compiled_at(Timestamp::from_second(1_700_000_000).unwrap())
                    .compilation_duration(Duration::from_secs(0))
                    .compiler_version("test".to_owned())
                    .optimization_level(OptimizationStrategy::Basic)
                    .trial_count(0)
                    .step_count(0)
                    .barrier_count(0)
                    .element_instance_count(0)
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

    #[tokio::test]
    async fn priority_policy_picks_critical_first() {
        let sched = DefaultScheduler::with_policy(SchedulingPolicy::Priority);
        let p = plan(vec![deploy("a", &[]), deploy("b", &[]), deploy("c", &[])]);
        sched.initialise(&p).await.unwrap();
        sched.set_priority(&StepId::new("a").unwrap(), SchedulerPriority::Low);
        sched.set_priority(&StepId::new("b").unwrap(), SchedulerPriority::Critical);
        sched.set_priority(&StepId::new("c").unwrap(), SchedulerPriority::Normal);

        let picked = sched.next_steps(3, &ResourceSnapshot::default()).await;
        assert_eq!(picked[0].as_str(), "b");
        assert_eq!(picked[1].as_str(), "c");
        assert_eq!(picked[2].as_str(), "a");
    }
}

#[allow(
    clippy::items_after_test_module,
    reason = "promotion helper lives next to scheduler trait impl; tests module was inlined earlier"
)]
/// True if `req` fits entirely within `snap`'s remaining capacity.
/// Treats `ResourceRequirements::none()` (all-zeros) as always-fits,
/// so steps with no declared ask never block on resources.
fn fits(req: &ResourceRequirements, snap: &ResourceSnapshot) -> bool {
    if req.peak_cpu_cores > snap.cpu_cores_free {
        return false;
    }
    if req.peak_memory_mb > snap.memory_mb_free {
        return false;
    }
    if req.peak_storage_gb > snap.storage_gb_free {
        return false;
    }
    if req.peak_network_gbps > snap.network_gbps_free {
        return false;
    }
    for (k, need) in &req.custom {
        let have = snap.custom_free.get(k).copied().unwrap_or(0);
        if have < *need {
            return false;
        }
    }
    true
}

/// Subtract `req` from `snap`. Callers gate on `fits` first, so
/// subtractions don't underflow for declared fields. `saturating_sub`
/// still guards against pathological inputs.
fn deduct(snap: &mut ResourceSnapshot, req: &ResourceRequirements) {
    snap.cpu_cores_free = (snap.cpu_cores_free - req.peak_cpu_cores).max(0.0);
    snap.memory_mb_free = snap.memory_mb_free.saturating_sub(req.peak_memory_mb);
    snap.storage_gb_free = snap.storage_gb_free.saturating_sub(req.peak_storage_gb);
    snap.network_gbps_free =
        (snap.network_gbps_free - req.peak_network_gbps).max(0.0);
    for (k, need) in &req.custom {
        let entry = snap.custom_free.entry(k.clone()).or_insert(0);
        *entry = entry.saturating_sub(*need);
    }
}

fn promote_ready_for(inner: &mut InnerState, settled: &StepId) {
    // Any step whose deps are all in a terminal state becomes ready.
    let terminal_now: BTreeSet<StepId> = inner
        .completed
        .union(&inner.skipped)
        .cloned()
        .collect::<BTreeSet<_>>();
    let dependents = inner.dependents.get(settled).cloned().unwrap_or_default();
    for d in dependents {
        if inner.pending.contains(&d) {
            let deps = inner.deps.get(&d).cloned().unwrap_or_default();
            if deps.iter().all(|x| terminal_now.contains(x)) {
                inner.pending.remove(&d);
                inner.ready.insert(d);
            }
        }
    }
}
