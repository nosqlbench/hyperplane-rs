<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0011 — Executor, Orchestrator, Scheduler

## Purpose

Define the execution-side machinery that consumes an `ExecutionPlan`
and runs it: the top-level `Executor`, the system-level `Orchestrator`
that wires `Element` declarations to concrete `ElementRuntime`s, the
`Scheduler` that picks the next ready step, the `ResourceManager`
that admits steps under capacity, the `ArtifactCollector` that
captures outputs, the `ExecutionStateManager` that tracks per-step
status, the `JournalWriter` that records transitions for resume, and
the runtime `BarrierHandle` that is the stateful counterpart to the
plan-level `Barrier` record from SRD-0009.

Every trait here is async (tokio per SRD-0003 R17), pluggable (traits
first, `DefaultExecutor` composing them), and operates on shared
immutable plans.

## Scope

**In scope.**

- `Executor` — top-level trait: run a plan, resume from a
  checkpoint, execute async with an observer, await results.
- `Orchestrator` — system runtime that pairs `Element`s with
  `ElementRuntime`s, manages per-instance state, delivers trial
  lifecycle hooks.
- `Scheduler` — picks the next ready `AtomicStep` from an
  `ExecutionGraph`; respects dependencies, concurrency
  annotations, resource admission.
- `ResourceManager` — CPU/memory/storage/network slots + custom
  resources; admission control with quotas and pools.
- `ArtifactCollector` — captures typed outputs (for `SaveOutput`
  steps) and freeform artefacts (logs, traces, dumps).
- `ExecutionStateManager` — per-step lifecycle tracking;
  recovery from a checkpoint.
- `JournalWriter` / `JournalEvent` — append-only event stream
  that makes partial runs replayable.
- `BarrierHandle` / `BarrierState` — the runtime mutable half of
  SRD-0009's barrier model.
- `Checkpoint` and `CheckpointState` — the in-memory snapshot
  shape; persistence is in SRD-0012.
- `ExecutionObserver` — non-invasive progress callback.
- `ExecutionResults` — the aggregate outcome.
- Intervention (pause/resume/stop) with the `InterventionMode`
  settings from SRD-0008.
- Retry handling — how step-level `RetryPolicy` + plan-level
  `OnFailure` compose.

**Out of scope.**

- Concrete `ElementRuntime` implementations (EC2 node, Docker
  service, diagnostic) — hyperplane-tier SRDs.
- Storage of checkpoints, journals, artefacts, and results — the
  persistence SRD (Phase 1 item 10).
- Observability backends (tracing subscriber, metric sinks) —
  SRD-0003 R/logging pinning + hyperplane observability SRD.
- The compiler — SRD-0010.

## Naming split (Runtime vs Orchestrator)

Upstream calls the system-level runtime `Runtime`, and SRD-0007
already uses that name for the per-element behavioural trait
(`ElementRuntime`). To avoid confusion, this SRD uses
**`Orchestrator`** for the system-level runtime. Concretely:

- **`ElementRuntime` (SRD-0007)** — per-element behavioural trait.
  Hyperplane provides concrete impls (EC2 node, Docker container).
  One per element instance.
- **`Orchestrator` (this SRD)** — system-wide service that
  dispatches step invocations to the right `ElementRuntime`,
  manages per-instance state, delivers trial lifecycle hooks, and
  tracks deployed instances. One per execution.

Upstream readers will recognise `Orchestrator` as upstream's
`Runtime`.

## Background

Upstream reference:
- `.../execution/Executor.java`
- `.../execution/Runtime.java`         (→ `Orchestrator` here)
- `.../execution/Scheduler.java`
- `.../execution/ResourceManager.java`
- `.../execution/ArtifactCollector.java`
- `.../execution/ExecutionStateManager.java`
- `.../execution/NoopExecutionStateManager.java`
- `.../execution/journal/JournalEvent.java`
- `links/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/execution/HyperplaneRuntime.java`

Shape observations:

1. Upstream has seven distinct execution-side interfaces plus a
   journal subpackage. The split is load-bearing: TCK tests each
   interface independently, and hyperplane's controller provides
   concrete impls for some (`HyperplaneRuntime`) while paramodel
   supplies defaults for others (`DefaultScheduler`, `DefaultResourceManager`).
   Our Rust port preserves the split.
2. Upstream's `ExecutionStateManager` has a `noop()` default
   implementation — state tracking is optional. We preserve that.
   Studies that don't need resumability can run with the noop
   manager; studies that do plug in a real one.
3. Upstream `Scheduler` has a `SchedulingPolicy` enum
   (`FIFO`, `PRIORITY`, `FAIR`, `RESOURCE_AWARE`). We port it.
4. Upstream `ResourceManager` has pools, quotas, and a
   per-pool-priority model. V1 supports the shape; real pool /
   quota enforcement can ship later as the underlying scheduler
   grows — this SRD pins the API, not the algorithm's
   completeness.
5. Upstream `Executor` exposes both blocking (`execute`) and
   async (`executeAsync`) APIs. In Rust with tokio everywhere,
   we have one async API; synchronous callers block on it from
   a blocking-pool handle.

## Execution model at a glance

![Execution model: the Executor takes an ExecutionPlan and composes a Scheduler (picks next ready step), a Runtime (invokes ElementRuntime hooks), and an Observer (emits journal events). Runtime dispatches to per-Kind ElementRuntime implementations that live in hyperplane. The Executor also writes to CheckpointStore for resume and produces ExecutionResults at the end.](diagrams/SRD-0011/executor-model.png)

Scheduler + Runtime are separate concerns:

- **Scheduler** answers "which step next?" (topological + resource-aware).
- **Runtime** answers "how to run this step?" (via ElementRuntime trait).

The `Observer` tees every transition into the journal stream that
downstream systems (hyperplane's event stream, SRD-0111) subscribe to.

## Design

All traits live in the `paramodel-executor` crate (new).
Dependencies: `paramodel-elements`, `paramodel-trials`,
`paramodel-plan`, `paramodel-compiler`.

### `Executor`

```rust
#[async_trait::async_trait]
pub trait Executor: Send + Sync + 'static {
    /// Run the plan to completion (success, failure, or cancellation).
    async fn execute(
        &self,
        plan:     &ExecutionPlan,
        observer: Option<Box<dyn ExecutionObserver>>,
    ) -> Result<ExecutionResults, ExecutionError>;

    /// Resume from a checkpoint — continue a previously suspended
    /// execution. The checkpoint's `source_plan_fingerprint` must
    /// match `plan.fingerprint()`; mismatch → `Error::PlanMismatch`.
    async fn resume(
        &self,
        plan:       &ExecutionPlan,
        checkpoint: &Checkpoint,
        observer:   Option<Box<dyn ExecutionObserver>>,
    ) -> Result<ExecutionResults, ExecutionError>;

    /// Pre-flight dry run: validate the plan against current
    /// resource availability and registered `ElementRuntime`
    /// implementations without actually executing. Returns
    /// diagnostics that surface unreachable steps, unsupported
    /// element types, or insufficient resources.
    async fn dry_run(
        &self,
        plan: &ExecutionPlan,
    ) -> Result<Vec<ExecutionDiagnostic>, ExecutionError>;

    /// Request intervention. Effect depends on the plan's
    /// `ExecutionPolicies.intervention_mode`.
    async fn pause (&self, execution: ExecutionId) -> Result<()>;
    async fn resume_execution(&self, execution: ExecutionId) -> Result<()>;
    async fn stop  (&self, execution: ExecutionId, graceful: bool) -> Result<()>;

    /// Runtime inspection.
    async fn status (&self, execution: ExecutionId) -> Option<ExecutionStatus>;
    async fn checkpoints(&self, execution: ExecutionId) -> Vec<CheckpointHandle>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExecutionId(ulid::Ulid);
```

A `DefaultExecutor` struct composes the other traits:

```rust
pub struct DefaultExecutor {
    orchestrator:      Arc<dyn Orchestrator>,
    scheduler:         Arc<dyn Scheduler>,
    resource_manager:  Arc<dyn ResourceManager>,
    artifact_collector: Arc<dyn ArtifactCollector>,
    state_manager:     Arc<dyn ExecutionStateManager>,
    journal_writer:    Arc<dyn JournalWriter>,
    config:            ExecutorConfig,
}

impl DefaultExecutor {
    pub fn builder() -> DefaultExecutorBuilder { ... }
}

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    pub max_parallel_steps:     usize,        // default: num_cpus
    pub step_timeout_default:   Option<std::time::Duration>,
    pub checkpoint_interval:    Option<std::time::Duration>,
    pub fail_fast:              bool,         // default: false
    pub collect_artifacts:      bool,         // default: true
}
```

The builder wires up dependencies; pluggability happens at the
`Arc<dyn>` boundary. A test harness can substitute a
`NoopStateManager`, `InMemoryJournal`, or `MockOrchestrator`
while leaving everything else from the default.

### `Orchestrator` — the system runtime

```rust
#[async_trait::async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    /// Prepare the orchestrator for a run: register every element
    /// from the plan, resolve `ElementRuntime` for each, acquire
    /// any long-lived resources.
    async fn prepare(&self, plan: &ExecutionPlan) -> Result<(), OrchestratorError>;

    /// Execute a single `AtomicStep` against the correct
    /// `ElementRuntime`. The orchestrator translates the step's
    /// target element + instance number into a live `ElementRuntime`
    /// handle and invokes its relevant method (materialize for
    /// Deploy, dematerialize for Teardown, await/status for
    /// Await/SaveOutput, lifecycle hooks for TrialStart/TrialEnd).
    async fn execute_step(
        &self,
        plan: &ExecutionPlan,
        step: &AtomicStep,
    ) -> Result<StepOutcome, OrchestratorError>;

    /// Await readiness of an element instance (for HealthCheck
    /// gates) with the element's `HealthCheckSpec` timing.
    async fn await_ready(
        &self,
        instance_id: &InstanceId,
        spec:        &HealthCheckSpec,
    ) -> Result<(), OrchestratorError>;

    /// Deliver a trial-start / trial-end notification to every
    /// listed non-trial element's `ElementRuntime`.
    async fn notify_trial_start(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
    ) -> Result<()>;
    async fn notify_trial_end(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
        reason:   ShutdownReason,
    ) -> Result<()>;

    /// Finalise: tear down any live instances the runtime still
    /// holds, release long-lived resources. Called after the
    /// executor reaches `End` or on an error / cancellation path.
    async fn shutdown(&self) -> Result<()>;

    /// Current operational state of an element instance (for the
    /// scheduler, observers, and UI).
    async fn instance_state(
        &self,
        instance_id: &InstanceId,
    ) -> Option<LiveStatusSummary>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StepOutcome {
    /// Step completed normally. `materialization_outputs` is
    /// non-empty only for `Deploy` steps that published typed
    /// results.
    Completed {
        duration:                 std::time::Duration,
        materialization_outputs:  MaterializationOutputs,
        metrics:                  BTreeMap<ParameterName, Value>,
        artifacts:                Vec<ArtifactRef>,
    },
    /// Step was skipped (upstream failure, policy).
    Skipped { reason: String },
    /// Step failed. The executor decides whether to retry per
    /// policy.
    Failed { error: ErrorInfo },
}
```

The `DefaultOrchestrator` composes the `ElementRuntimeRegistry`
(SRD-0007) and tracks one `Arc<dyn ElementRuntime>` per live
instance. Per-instance state transitions go through the
`OperationalState` machine on `ElementRuntime`.

### `Scheduler`

```rust
#[async_trait::async_trait]
pub trait Scheduler: Send + Sync + 'static {
    async fn initialise(&self, plan: &ExecutionPlan) -> Result<(), SchedulerError>;

    /// Return the next batch of ready steps (up to `max_count`),
    /// respecting dependencies, concurrency annotations, and
    /// current resource availability reported by the supplied
    /// `ResourceSnapshot`.
    async fn next_steps(
        &self,
        max_count: usize,
        resources: &ResourceSnapshot,
    ) -> Vec<StepId>;

    async fn mark_started  (&self, step: &StepId, at: jiff::Timestamp);
    async fn mark_completed(&self, step: &StepId, at: jiff::Timestamp);
    async fn mark_failed   (&self, step: &StepId, at: jiff::Timestamp, err: &ErrorInfo);
    async fn mark_skipped  (&self, step: &StepId, at: jiff::Timestamp, reason: &str);

    fn set_priority (&self, step: &StepId, priority: SchedulerPriority);
    fn get_priority (&self, step: &StepId) -> SchedulerPriority;

    fn is_complete(&self) -> bool;
    fn state      (&self) -> SchedulerState;
    fn statistics (&self) -> SchedulerStatistics;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulingPolicy {
    /// First-in-first-out over the ready set.
    Fifo,
    /// Critical-path first; higher priority wins.
    Priority,
    /// Round-robin across trials to avoid head-of-line blocking.
    Fair,
    /// Maximise resource utilisation (pack steps that fit
    /// remaining capacity).
    ResourceAware,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerPriority { Low, Normal, High, Critical }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchedulerState {
    pub pending_count:   u32,
    pub running_count:   u32,
    pub completed_count: u32,
    pub failed_count:    u32,
    pub skipped_count:   u32,
    pub ready_count:     u32,
}
```

A `DefaultScheduler` implements `SchedulingPolicy::Fifo` with
priority overrides. Concurrency annotations (Rule 6 from SRD-0010)
are enforced here: the scheduler tracks in-flight instance counts
per element and refuses to emit another `Deploy` for an element
already at its `max_concurrency` or `max_group_concurrency`.

Barrier satisfaction: the scheduler treats an `AtomicStep::Barrier`
as ready only when every step in `barrier.depends_on` has
terminal status (completed / skipped). Timeout and timeout-action
handling live in the `BarrierHandle` (below) and are driven by
the scheduler's enforcement loop.

### `ResourceManager`

```rust
#[async_trait::async_trait]
pub trait ResourceManager: Send + Sync + 'static {
    async fn allocate (&self, request: &ResourceRequest) -> Result<ResourceAllocation, ResourceError>;
    async fn release  (&self, allocation: ResourceAllocation);

    fn can_allocate   (&self, request: &ResourceRequest) -> bool;
    fn available      (&self) -> ResourceSnapshot;
    fn current_usage  (&self) -> ResourceUsage;

    fn create_pool    (&self, name: &str, capacity: &ResourceCapacity, priority: PoolPriority);
    fn pools          (&self) -> Vec<ResourcePool>;
    fn set_quota      (&self, quota: ResourceQuota);
    fn quota_usage    (&self, user: &str) -> QuotaUsage;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceRequest {
    pub cpu_cores:    f64,
    pub memory_mb:    u64,
    pub storage_gb:   u64,
    pub network_gbps: f64,
    pub custom:       BTreeMap<String, u64>,
    pub pool:         Option<String>,
    pub owner:        Option<String>,
    pub duration:     Option<std::time::Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceAllocation {
    pub id:           AllocationId,
    pub granted:      ResourceCapacity,
    pub pool:         Option<String>,
    pub allocated_at: jiff::Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ResourceSnapshot {
    pub cpu_cores_free:    f64,
    pub memory_mb_free:    u64,
    pub storage_gb_free:   u64,
    pub network_gbps_free: f64,
    pub custom_free:       BTreeMap<String, u64>,
}
```

V1 ships with a `DefaultResourceManager` backed by a single pool
(no sub-pools, no quotas). The pool/quota API shape is defined
here so adopting systems can plug in richer backends without
renegotiating the trait.

Resource accounting is a scheduler input, not a hard barrier: the
scheduler asks `can_allocate` before emitting a step, and the
orchestrator calls `allocate` / `release` at step entry and exit.

### `ArtifactCollector`

```rust
#[async_trait::async_trait]
pub trait ArtifactCollector: Send + Sync + 'static {
    async fn start_collection(&self, trial: &TrialId);
    async fn stop_collection (&self, trial: &TrialId) -> Result<ArtifactCollection, ArtifactError>;

    async fn collect_log     (&self, trial: &TrialId, element: &ElementName, kind: ArtifactKind, bytes: &[u8]);
    async fn collect_artifact(&self, trial: &TrialId, element: &ElementName, name: &str, content: ArtifactContent, kind: ArtifactKind);
    async fn collect_metrics (&self, trial: &TrialId, element: &ElementName, metrics: &BTreeMap<ParameterName, Value>);

    async fn artifacts       (&self, trial: &TrialId) -> Vec<ArtifactRef>;
    async fn download        (&self, artifact: &ArtifactRef) -> Result<ArtifactContent, ArtifactError>;

    async fn set_retention_policy(&self, policy: RetentionPolicy);
    async fn cleanup            (&self) -> CleanupReport;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Stdout, Stderr, Log, Metric, Trace,
    Profile, Screenshot, Video, Report,
    Result, StackTrace, CoreDump, MemoryDump,
    NetworkTrace, CpuProfile, MemoryProfile,
    Custom,
}

pub enum ArtifactContent {
    Inline(Vec<u8>),
    Stream(Box<dyn AsyncRead + Send + Unpin>),
    Uri(String),     // already stored elsewhere
}
```

`SaveOutput` steps call `collect_artifact` or `collect_metrics`
depending on the `OutputSelector`. Inline content is stored in
memory (small artefacts like metric dumps); stream/URI forms are
for large volumes that shouldn't materialise in memory.

### `ExecutionStateManager`

```rust
#[async_trait::async_trait]
pub trait ExecutionStateManager: Send + Sync + 'static {
    async fn record_event  (&self, event: JournalEvent);
    async fn checkpoint    (&self, checkpoint: &Checkpoint);

    async fn recover       (&self, execution: &ExecutionId, plan: &ExecutionPlan) -> RecoveryResult;

    async fn is_step_completed(&self, execution: &ExecutionId, step: &StepId) -> bool;
    async fn record_suspension(&self, execution: &ExecutionId, reason: &str);

    async fn save_trial_result(&self, execution: &ExecutionId, result: &TrialResult);
    async fn get_trial_result (&self, trial: &TrialId) -> Option<TrialResult>;
    async fn get_trial_results(&self, execution: &ExecutionId) -> Vec<TrialResult>;

    async fn cleanup(&self, execution: &ExecutionId);

    /// Idempotency class for a step variant. Idempotent steps
    /// can be safely retried on recovery; non-idempotent steps
    /// need state inspection first. Provided as a default because
    /// it's a pure function of the step kind.
    fn idempotency_class(&self, step: &AtomicStep) -> IdempotencyClass {
        match step {
            AtomicStep::Deploy    { .. } | AtomicStep::Await { .. }
                                         | AtomicStep::SaveOutput { .. } => IdempotencyClass::Idempotent,
            AtomicStep::Teardown   { .. }
            | AtomicStep::TrialStart { .. } | AtomicStep::TrialEnd { .. }
            | AtomicStep::Barrier    { .. } | AtomicStep::Checkpoint { .. } => IdempotencyClass::NonIdempotent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyClass {
    Idempotent,     // safe to re-run on recovery
    NonIdempotent,  // inspect state before re-running
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecoveryResult {
    pub completed_step_ids:    BTreeSet<StepId>,
    pub failed_step_ids:       BTreeSet<StepId>,
    pub skipped_step_ids:      BTreeSet<StepId>,
    pub in_flight_step_ids:    BTreeSet<StepId>,
    pub completed_trial_ids:   BTreeSet<TrialId>,
    pub in_flight_trial_ids:   BTreeSet<TrialId>,
    pub element_states:        BTreeMap<InstanceId, OperationalState>,
    pub was_clean_shutdown:    bool,
    pub in_flight_resolutions: BTreeMap<StepId, InFlightResolution>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InFlightResolution {
    /// Safe to re-run — the step is idempotent.
    Retry,
    /// Inspect the step's side effects before retrying.
    Inspect { state_query: String },
    /// Mark failed without re-running (non-idempotent + unknown
    /// outcome).
    MarkFailed,
}
```

Upstream supplies a `NoopExecutionStateManager.INSTANCE`; we
preserve the same via a `NoopStateManager` struct that satisfies
the trait with empty implementations. Plans that don't need
resume can use it.

### `JournalWriter` / `JournalEvent`

```rust
#[async_trait::async_trait]
pub trait JournalWriter: Send + Sync + 'static {
    async fn write     (&self, event: JournalEvent) -> Result<(), JournalError>;
    async fn flush     (&self) -> Result<(), JournalError>;
    async fn since     (&self, after: Option<JournalSequence>) -> Result<Vec<JournalEvent>, JournalError>;
    async fn last_event(&self, execution: &ExecutionId) -> Option<JournalEvent>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JournalEvent {
    pub sequence:      JournalSequence,
    pub execution_id:  ExecutionId,
    pub timestamp:     jiff::Timestamp,
    pub kind:          JournalEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JournalEventKind {
    ExecutionStarted  { plan_fingerprint: Fingerprint },
    ExecutionResumed  { checkpoint_id:    CheckpointId },
    ExecutionPaused   { reason:           String },
    ExecutionStopped  { reason:           String, graceful: bool },
    ExecutionCompleted{ success:          bool, duration: std::time::Duration },

    StepStarted       { step_id: StepId, attempt: u32 },
    StepCompleted     { step_id: StepId, attempt: u32, duration: std::time::Duration },
    StepFailed        { step_id: StepId, attempt: u32, error:  ErrorInfo },
    StepSkipped       { step_id: StepId, reason:  String },

    InstanceStateChange { instance_id: InstanceId, from: OperationalState, to: OperationalState },

    BarrierReached    { barrier_id: BarrierId },
    BarrierTimedOut   { barrier_id: BarrierId, waited: std::time::Duration },

    CheckpointCreated { checkpoint_id: CheckpointId, completed_step_count: u32 },

    TrialStarted      { trial_id: TrialId, trial_index: u32 },
    TrialCompleted    { trial_id: TrialId, status:      TrialStatus },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct JournalSequence(u64);
```

The journal is append-only. Sequence numbers are monotonic per
execution. A full recovery reads events in order and reconstructs
the executor's state machine; a partial recovery reads from the
last checkpoint forward. `JournalStore` (persistence) is defined
in SRD-0012; this SRD defines the event shapes.

### `BarrierHandle` and `BarrierState`

```rust
#[async_trait::async_trait]
pub trait BarrierHandle: Send + Sync + 'static {
    fn id   (&self) -> &BarrierId;
    fn state(&self) -> BarrierState;

    async fn await_satisfied(&self) -> Result<(), BarrierError>;
    async fn await_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<bool, BarrierError>;                // true iff satisfied within timeout

    /// Scheduler / state-manager call this when the set of
    /// satisfied dependencies has changed. Internally, the
    /// handle wakes any awaiters once all dependencies are met.
    async fn mark_dependency_satisfied(&self, step: &StepId);

    /// Force the barrier into Failed state; propagate to any
    /// downstream waiters.
    async fn fail(&self, reason: &str);

    /// Force the barrier into Satisfied state regardless of
    /// dependency state — used by intervention code for graceful
    /// stops.
    async fn release(&self);

    fn satisfied_dependencies(&self) -> BTreeSet<StepId>;
    fn pending_dependencies  (&self) -> BTreeSet<StepId>;

    fn wait_duration(&self) -> Option<std::time::Duration>;
    fn satisfied_at (&self) -> Option<jiff::Timestamp>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierState {
    Pending,
    Satisfied,
    Failed,
    TimedOut,
}
```

One `BarrierHandle` is created per `AtomicStep::Barrier` step at
execution start. The scheduler tracks it until every dependency
reports terminal status, then invokes `release`. Runtime code that
needs to synchronise calls `await_satisfied()` / `await_with_timeout`.

The plan-level `AtomicStep::Barrier` record (SRD-0009) carries
the configuration (`barrier_id`, `kind`, `timeout`,
`timeout_action`); the `BarrierHandle` carries the state. The
one-to-one correspondence is by `barrier_id`.

### `Checkpoint` and `CheckpointState`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Checkpoint {
    pub id:                       CheckpointId,
    pub execution_id:             ExecutionId,
    pub plan_fingerprint:         Fingerprint,
    pub created_at:               jiff::Timestamp,
    pub state:                    CheckpointState,
    pub fingerprint:              Fingerprint,   // of the state itself
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointState {
    pub completed_step_ids:  BTreeSet<StepId>,
    pub failed_step_ids:     BTreeSet<StepId>,
    pub skipped_step_ids:    BTreeSet<StepId>,
    pub in_flight_step_ids:  BTreeSet<StepId>,
    pub completed_trials:    BTreeMap<TrialId, TrialResult>,
    pub in_flight_trials:    BTreeSet<TrialId>,
    pub instance_states:     BTreeMap<InstanceId, OperationalState>,
    pub scheduler_state:     SchedulerState,
    pub resource_usage:      ResourceUsage,
    pub journal_sequence:    JournalSequence,    // high-water mark
}

pub struct CheckpointHandle {
    pub id:         CheckpointId,
    pub created_at: jiff::Timestamp,
    pub is_valid:   bool,          // fingerprint matches current plan
}
```

Resume rejects a checkpoint whose `plan_fingerprint` doesn't
match the plan passed to `Executor::resume` — per SRD-0002 R27's
immutability guarantee, a plan and its fingerprint are
inseparable.

### `ExecutionObserver`

```rust
pub trait ExecutionObserver: Send + Sync + 'static {
    fn on_execution_started  (&self, execution: &ExecutionId, plan: &ExecutionPlan) {}
    fn on_execution_completed(&self, execution: &ExecutionId, results: &ExecutionResults) {}
    fn on_execution_failed   (&self, execution: &ExecutionId, error:   &ExecutionError) {}

    fn on_step_started  (&self, execution: &ExecutionId, step: &AtomicStep, attempt: u32) {}
    fn on_step_completed(&self, execution: &ExecutionId, step: &AtomicStep, attempt: u32, elapsed: std::time::Duration) {}
    fn on_step_failed   (&self, execution: &ExecutionId, step: &AtomicStep, attempt: u32, error:  &ErrorInfo) {}
    fn on_step_skipped  (&self, execution: &ExecutionId, step: &AtomicStep, reason:  &str) {}

    fn on_barrier_reached  (&self, execution: &ExecutionId, barrier_id: &BarrierId) {}
    fn on_barrier_timed_out(&self, execution: &ExecutionId, barrier_id: &BarrierId) {}

    fn on_checkpoint_created(&self, execution: &ExecutionId, checkpoint: &CheckpointHandle) {}

    fn on_trial_started  (&self, execution: &ExecutionId, trial: &Trial) {}
    fn on_trial_completed(&self, execution: &ExecutionId, trial: &Trial, result: &TrialResult) {}

    fn on_intervention(&self, execution: &ExecutionId, kind: &InterventionKind) {}
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InterventionKind {
    Pause { reason: String },
    Resume,
    Stop  { reason: String, graceful: bool },
}
```

Observers receive events synchronously with respect to the
executor state transitions but cannot influence execution. All
methods have default empty bodies so impls override only the ones
they care about.

### `ExecutionResults`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionResults {
    pub execution_id:      ExecutionId,
    pub plan_fingerprint:  Fingerprint,
    pub final_status:      ExecutionStatus,

    pub started_at:        jiff::Timestamp,
    pub completed_at:      jiff::Timestamp,
    pub duration:          std::time::Duration,

    pub trial_results:     Vec<TrialResult>,
    pub aggregate_metrics: BTreeMap<ParameterName, Value>,

    pub total_trials:      u32,
    pub successful_trials: u32,
    pub failed_trials:     u32,
    pub skipped_trials:    u32,

    pub step_statistics:   StepStatistics,

    pub labels:            Labels,
    pub tags:              Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepStatistics {
    pub total:     u32,
    pub completed: u32,
    pub failed:    u32,
    pub skipped:   u32,
    pub retried:   u32,
    pub average_duration: Option<std::time::Duration>,
    pub critical_path_duration: Option<std::time::Duration>,
}
```

### Intervention semantics

Pause/resume/stop honour the plan's `InterventionMode` from
SRD-0008:

- `InterventionMode::Immediate` — `pause()` signals the scheduler
  to emit no new steps, cancels in-flight step *futures* where
  safe, and reports `Paused` as soon as possible. `stop(graceful: false)`
  aborts in-flight steps.
- `InterventionMode::AfterActiveTrials` — `pause()` signals the
  scheduler to emit no new trials but lets every in-flight trial's
  remaining steps complete first. `stop(graceful: true)` does the
  same then runs teardown steps.

`stop(graceful: true)` always runs teardown steps for every live
instance (via the orchestrator) before `ExecutionCompleted` is
emitted with status `Cancelled`. `stop(graceful: false)` abandons
live instances — the orchestrator's `shutdown()` is still called
but may leave external resources behind; the journal records
this case so the next run can discover and clean up.

### Retry composition

When a step fails, the executor composes two policies in order:

1. **Step-level `RetryPolicy`** on the `StepHeader.retry_policy`
   (SRD-0009). If the error type is in `retryable_errors` and
   `attempt < max_attempts`, retry after `backoff.delay_for_attempt(attempt)`.
2. **Plan-level `OnFailure`** from `TestPlan.policies.on_failure`
   (SRD-0008). Applied once the step-level retry budget is
   exhausted:
   - `OnFailure::Stop` — mark the whole execution `Failed` and
     begin graceful shutdown.
   - `OnFailure::Skip` — mark the trial containing this step
     `Skipped`, emit `TrialCompleted` with that status, and
     continue.
   - `OnFailure::Retry { attempts }` — re-run the entire trial up
     to `attempts` times from its `TrialStart`.

Retries increment `TrialResult.attempt_number` (SRD-0006) and
emit a new journal event per attempt.

### Composed wiring: `DefaultExecutor::execute`

```
1. Create ExecutionId (ULID). Emit ExecutionStarted.
2. orchestrator.prepare(plan).
3. scheduler.initialise(plan.execution_graph).
4. Loop:
   a. ready_steps = scheduler.next_steps(concurrency_budget, resource_snapshot).
   b. For each step in ready_steps:
      - Acquire resources via resource_manager.allocate.
      - Spawn a tokio task:
         - orchestrator.execute_step → StepOutcome.
         - Update artifact_collector with metrics/artifacts.
         - state_manager.record_event(StepCompleted | Failed).
         - scheduler.mark_*(step, ...).
         - resource_manager.release(allocation).
      - Emit observer callbacks.
   c. If checkpoint_interval elapsed: snapshot + state_manager.checkpoint.
   d. If intervention pending: honour per InterventionMode.
   e. If scheduler.is_complete(): break.
5. orchestrator.shutdown().
6. Build ExecutionResults. Emit ExecutionCompleted.
```

Every state change flows through `state_manager.record_event`
and corresponding `journal_writer.write`; either an event for the
journal is persisted or the execution crashes and recovery
reconstructs from the last flushed event.

## Decisions

- **D1.** Seven execution traits: `Executor`, `Orchestrator`,
  `Scheduler`, `ResourceManager`, `ArtifactCollector`,
  `ExecutionStateManager`, `JournalWriter`. Plus `BarrierHandle`
  and `ExecutionObserver`. `DefaultExecutor` composes them via
  `Arc<dyn …>` so each is swappable in tests and extensions.
- **D2.** System-level runtime is named **`Orchestrator`** to
  avoid collision with SRD-0007's `ElementRuntime`.
- **D3.** All traits are async (`tokio`). The sole sync trait is
  `ExecutionObserver` (non-invasive callback invoked from within
  the executor).
- **D4.** `ExecutionStateManager` has a `NoopStateManager`
  implementation; studies that don't need resume can run without
  persistence.
- **D5.** Idempotency class is a function of the step kind:
  `Deploy`, `Await`, `SaveOutput` are idempotent (safe to re-run
  on recovery); `Teardown`, `TrialStart`, `TrialEnd`, `Barrier`,
  `Checkpoint` are non-idempotent (inspect state before re-run).
  Default implementation supplied; impls can override per-step.
- **D6.** `Scheduler` implements `SchedulingPolicy::Fifo` with
  priority overrides by default; `Priority`, `Fair`, and
  `ResourceAware` are defined in the policy enum for future
  impls.
- **D7.** Concurrency annotations from reducto Rule 6
  (`max_concurrency`, `max_group_concurrency`) are enforced by the
  scheduler, not the graph. No structural edges — per SRD-0002
  R26 and SRD-0010.
- **D8.** `BarrierHandle` is the runtime mutable counterpart to
  `AtomicStep::Barrier`. The plan record is immutable; the handle
  carries state. One-to-one by `barrier_id`.
- **D9.** `Checkpoint` carries the plan fingerprint; resuming
  with a checkpoint whose fingerprint doesn't match the plan is
  an error. A recompiled plan cannot consume old checkpoints.
- **D10.** `JournalEvent` is an enum with one variant per
  executor-observable transition. Events are serde-able and
  monotonically sequenced per execution.
- **D11.** Intervention (pause/resume/stop) honours the plan's
  `InterventionMode`. `stop(graceful: true)` runs teardown
  steps; `stop(graceful: false)` does not.
- **D12.** Retry composition: step-level `RetryPolicy` runs
  first; plan-level `OnFailure` runs once the step-level budget
  is exhausted. `OnFailure::Retry { attempts }` re-runs whole
  trials from `TrialStart`.
- **D13.** `ExecutionResults` carries typed aggregate metrics,
  per-trial results, step statistics, and status. Labels and tags
  from the plan propagate.
- **D14.** `async-trait` crate is used for trait async dispatch
  (matching SRD-0007 D19). Migrate away when native async-fn in
  dyn trait stabilises on nightly.
