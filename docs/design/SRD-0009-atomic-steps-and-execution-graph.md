<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0009 — Atomic Steps and Execution Graph

## Purpose

Define the compiled, immutable artefacts the compiler produces and the
executor consumes: `AtomicStep` (the indivisible unit of work),
`ExecutionGraph` (the DAG of those steps), `ElementInstanceGraph`
(the element-instance intermediate layer), and `ExecutionPlan` (the
wrapper around both graphs plus compile-time metadata).

Per SRD-0002 §6.4, the compiler produces two derived graphs in
sequence:
1. The **Element Instance Graph** (element instances + typed
   dependency edges) — computed in compiler phase 1.
2. The **Execution Graph** (AtomicStep DAG) — computed in phase 2.

This SRD defines the shapes of both. The rules that produce them
live in the compilation SRD (Phase 1 item 8 — reducto port).

## Scope

**In scope.**

- `AtomicStep` — the unified enum with 8 variants: `Deploy`,
  `Teardown`, `TrialStart`, `TrialEnd`, `Await`, `SaveOutput`,
  `Barrier`, `Checkpoint`.
- `StepHeader` — the common prefix every variant carries.
- `StepId` — a deterministic, human-readable identifier.
- `ExecutionGraph` — ordered step list + derived accessors
  (topological sort, critical path, parallel waves, subgraphs).
- `ElementInstanceGraph` — instances + instance-to-instance edges.
- `ElementInstance` and `InstanceDependency`.
- `ExecutionPlan` — top-level immutable record that wraps both
  graphs plus metadata.
- `ResourceRequirements`, `CheckpointStrategy`,
  `ExecutionPlanMetadata`.
- Barrier types (`BarrierKind`, `TimeoutAction`) and their role
  inside `AtomicStep::Barrier`.
- Per-step `reason` field matching reducto's human-readable step
  annotations.

**Out of scope.**

- The rules that produce these graphs from a `TestPlan` —
  compilation SRD.
- The executor loop that consumes the graphs — executor SRD
  (Phase 1 item 9).
- The runtime `BarrierHandle` / `Checkpoint` state machines —
  executor SRD.
- Persistence of plans, checkpoints, and results — persistence
  SRD.

## Background

Upstream reference:
- `.../plan/AtomicStep.java` (sealed interface + 8 records)
- `.../plan/Barrier.java`
- `.../plan/ExecutionGraph.java`
- `.../plan/ExecutionPlan.java`
- `.../plan/ExecutionPlanMetadata.java`
- `.../plan/ElementInstanceGraph.java`
- Reducto Stage Four (Graph Linearization) and the mapping table
  from `ReductoNodeType` to `AtomicStep` subtypes.

Key shape observations:

1. **Upstream has 8 AtomicStep variants.** We keep 8, with two
   swaps: drop `TrialStep`, add `SaveOutput`. Rationale:
   - Upstream's `TrialStep` is the "operative action" of a trial
     element. Reducto's own docs say this is redundant: for a
     service trial element the operative span is
     `Deploy → Teardown`; for a command trial element it's
     `Deploy → Await`. No extra step needed between them.
   - The hyperplane study system distinguishes `SaveOutput` as
     a first-class action (fetch output-volume contents from a
     completed command container). We promote it to paramodel so
     the compiler and executor share one vocabulary across tiers.
2. **Each upstream AtomicStep record duplicates a large common
   header** (id, dependencies, estimatedDuration, resourceRequirements,
   retryPolicy, metadata). In Rust we factor that out into a
   `StepHeader` struct and attach it to each variant.
3. **Upstream `Barrier` is both a plan record and a runtime state
   machine.** SRD-0002 R5 splits these. This SRD keeps only the
   plan record (inside `AtomicStep::Barrier`); the runtime handle
   is in the executor SRD.
4. **Upstream `ExecutionPlan.steps()` and
   `ExecutionPlan.barriers()` are separate collections.** We drop
   the parallel `barriers()` collection; barriers are already in
   the step list as `AtomicStep::Barrier` variants. An accessor
   `plan.barriers()` filters the step list for them.
5. **Upstream `ExecutionGraph.Edge`** is a record carrying source,
   target, and weight. We don't carry separate edge records:
   edges are implicit in `step.header.depends_on`. Graph
   traversal and critical-path analysis compute from the step
   list directly.
6. **Upstream `ExecutionPlan.execute()`** collapses plan and
   executor into one interface. We separate them: `ExecutionPlan`
   is pure data (serde-able, fingerprintable); the `Executor`
   trait in SRD Phase 1 item 9 consumes `&ExecutionPlan` and
   drives execution.
7. **`ElementInstanceGraph` in upstream is already a separate
   interface** (`.../plan/ElementInstanceGraph.java`). We port it
   and extend the node type with the typed `Assignments` (per
   SRD-0006) the instance was bound with.

## Design

All types live in the `paramodel-plan` crate (added in SRD-0008)
or a new `paramodel-execution-plan` crate — the exact split will
be decided in the crate-layout follow-up. For this SRD, treat
these as module-internal types of the plan layer.

### `StepId`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct StepId(String);

impl StepId {
    pub fn new(s: impl Into<String>) -> Result<Self>;    // validates
    pub fn as_str(&self) -> &str;
}
```

Step IDs are human-readable, deterministic strings. Reducto's
naming convention is adopted:

- `activate_{element}_t{trial_index}` — per-trial deploy
- `activate_{element}_g{group_index}` — group-coalesced deploy
- `deactivate_{element}_t{trial_index}` — per-trial teardown
- `deactivate_{element}_g{group_index}` — group-coalesced teardown
- `await_{element}_t{trial_index}`
- `save_output_{element}_t{trial_index}`
- `notify_trial_start_t{trial_index}`
- `notify_trial_end_t{trial_index}`
- `barrier_{barrier_id}`
- `checkpoint_{checkpoint_id}`
- `start`, `end` (sentinels)

Deterministic IDs make plans diffable: regenerating a plan from
the same `TestPlan` produces byte-identical step IDs, so checkpoints
survive recompilation as long as the plan structure is unchanged.

Validation: non-empty, ≤ 256 bytes, ASCII with `_`, `-`, `.`, and
`:` allowed.

### `StepHeader`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct StepHeader {
    pub id:                    StepId,
    pub depends_on:            Vec<StepId>,

    /// Human-readable explanation of why this step exists.
    /// Matches reducto's `reason` field on plan steps — populated
    /// by the compiler, not the author. Typical values:
    /// "initial deploy", "parameter change", "trial cleanup",
    /// "RESET_EACH dependency", "IDLE (no remaining dependents)",
    /// "IDLE (quantity excess)", "final cleanup".
    pub reason:                String,

    /// Trial index this step belongs to. `None` for steps outside
    /// any trial (study-scope deploys, start/end sentinels).
    pub trial_index:           Option<u32>,

    /// Trial code (mixed-radix human-readable digit string from
    /// reducto). `None` if step isn't trial-scoped.
    pub trial_code:            Option<String>,

    pub estimated_duration:    Option<std::time::Duration>,
    pub resource_requirements: ResourceRequirements,
    pub retry_policy:          Option<RetryPolicy>,

    pub labels:                Labels,
    pub tags:                  Tags,
}
```

`RetryPolicy` is the same struct defined in SRD-0008 —
`{ max_attempts, backoff, retryable_errors }`. The step can
override the plan-level retry policy for a specific step.

### `AtomicStep`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AtomicStep {
    /// Provision an element instance with a resolved configuration.
    /// For node-type elements: EC2 provision + agent deploy.
    /// For service-type elements: pull image + docker run (detached).
    /// For command-type elements: pull image + docker run (foreground).
    Deploy {
        header:          StepHeader,
        element:         ElementName,
        instance_number: u32,
        configuration:   ResolvedConfiguration,     // fully token-expanded
        max_concurrency: Option<u32>,               // per SRD-0002 R26
    },

    /// Release the resources owned by an element instance.
    /// For node-type: terminate EC2 instance.
    /// For service-type: docker stop + docker rm.
    Teardown {
        header:            StepHeader,
        element:           ElementName,
        instance_number:   u32,
        collect_artifacts: bool,                    // whether to run SaveOutput first
    },

    /// Notify non-trial elements that a trial is about to begin.
    TrialStart {
        header:        StepHeader,
        trial_id:      TrialId,
        element_names: Vec<ElementName>,            // non-trial elements to notify
    },

    /// Notify non-trial elements that a trial has concluded.
    TrialEnd {
        header:          StepHeader,
        trial_id:        TrialId,
        element_names:   Vec<ElementName>,
        shutdown_reason: ShutdownReason,
    },

    /// Wait for a command-mode element to self-terminate.
    Await {
        header:          StepHeader,
        element:         ElementName,
        instance_number: u32,
        trial_id:        TrialId,
    },

    /// Fetch typed output from an element instance (typically a
    /// completed command container's output volume).
    SaveOutput {
        header:          StepHeader,
        element:         ElementName,
        instance_number: u32,
        trial_id:        TrialId,

        /// The `resultParameters` output being captured, or a
        /// special sentinel for whole-volume capture.
        output:          OutputSelector,
    },

    /// Synchronisation point. The barrier is ready when every
    /// `header.depends_on` step has completed.
    Barrier {
        header:         StepHeader,
        barrier_id:     BarrierId,
        kind:           BarrierKind,
        timeout:        Option<std::time::Duration>,
        timeout_action: TimeoutAction,
    },

    /// Persist execution state mid-run for resume.
    Checkpoint {
        header:        StepHeader,
        checkpoint_id: CheckpointId,
    },
}

impl AtomicStep {
    pub fn header(&self) -> &StepHeader;
    pub fn id(&self)     -> &StepId          { &self.header().id }
    pub fn depends_on(&self) -> &[StepId]    { &self.header().depends_on }

    pub fn element(&self) -> Option<&ElementName>;    // Deploy/Teardown/Await/SaveOutput
    pub fn trial_id(&self) -> Option<&TrialId>;       // all trial-scoped variants
}
```

Step variant accessors (`element`, `trial_id`) return `None` for
variants that don't apply, rather than forcing pattern-matching at
every call site.

`ShutdownReason`, `BarrierKind`, `TimeoutAction`, and
`OutputSelector` are all enums:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    Normal,          // trial completed as expected
    Cancelled,       // user-initiated
    Timeout,         // exceeded trial_timeout
    UpstreamFailed,  // a dependency failed
    Planned,         // planner-emitted (e.g. partial run boundary)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierKind {
    /// Wait until all dependencies have activated an element.
    ElementReady,
    /// Wait until all trials using an element have completed.
    ElementScopeEnd,
    /// Group trials for a checkpoint boundary.
    TrialBatch,
    /// Force state persistence before proceeding.
    CheckpointBoundary,
    /// Adopter-defined synchronisation point.
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    /// Fail immediately when the barrier times out.
    FailFast,
    /// Skip any dependent steps and proceed.
    SkipDependent,
    /// Wait indefinitely (no timeout enforcement).
    WaitForever,
    /// Retry the barrier wait per the step's RetryPolicy.
    Retry,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSelector {
    /// A specific declared result parameter (by name) — the value
    /// is placed in the trial's metrics under
    /// `(element, result_parameter)`.
    ResultParameter { parameter: ParameterName },

    /// Capture the whole output volume of a command container as
    /// an artifact. The artifact reference is placed in the
    /// trial's artifacts list.
    Volume { mount: String },
}
```

### `ExecutionGraph`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExecutionGraph {
    steps: Vec<AtomicStep>,       // order not semantically meaningful (ids are)
}

impl ExecutionGraph {
    pub fn steps(&self) -> &[AtomicStep];
    pub fn step(&self, id: &StepId) -> Option<&AtomicStep>;

    pub fn dependencies          (&self, step: &StepId) -> Vec<&AtomicStep>;
    pub fn transitive_dependencies(&self, step: &StepId) -> Vec<&AtomicStep>;
    pub fn dependents            (&self, step: &StepId) -> Vec<&AtomicStep>;
    pub fn transitive_dependents (&self, step: &StepId) -> Vec<&AtomicStep>;

    pub fn topological_sort(&self) -> Result<Vec<&AtomicStep>>;
    pub fn critical_path   (&self) -> Vec<&AtomicStep>;
    pub fn parallel_waves  (&self) -> Vec<Vec<&AtomicStep>>;
    pub fn maximum_parallelism(&self) -> usize;
    pub fn average_parallelism(&self) -> f64;

    pub fn is_acyclic(&self) -> bool;

    pub fn subgraph_for_element(&self, element: &ElementName) -> ExecutionGraph;
    pub fn subgraph_for_trials (&self, trial_ids: &[TrialId]) -> ExecutionGraph;

    /// Convenience filter: all `AtomicStep::Barrier` steps.
    pub fn barriers(&self) -> impl Iterator<Item = &AtomicStep>;

    pub fn statistics(&self) -> GraphStatistics;

    pub fn fingerprint(&self) -> Fingerprint;   // cached on first call
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
pub struct GraphStatistics {
    pub node_count:            u32,
    pub edge_count:            u32,
    pub max_depth:             u32,
    pub max_fan_out:           u32,
    pub max_fan_in:            u32,
    pub average_degree:        f64,
    pub critical_path_duration: Option<std::time::Duration>,
    pub total_duration:        Option<std::time::Duration>,
    pub maximum_parallelism:   u32,
    pub average_parallelism:   f64,
}
```

Edges are implicit in each step's `header.depends_on`; the graph
doesn't carry a separate edge list. Accessors materialise the
edge set on demand.

Invariants (enforced at construction by the compiler):

- Every `StepId` referenced in any step's `depends_on` exists in
  the graph.
- No cycles (`is_acyclic() == true`).
- Step IDs are unique within the graph.
- Exactly one `start` sentinel and one `end` sentinel.

### `ElementInstanceGraph`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ElementInstanceGraph {
    pub instances: Vec<ElementInstance>,
    pub edges:     Vec<InstanceDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ElementInstance {
    pub id:              InstanceId,
    pub element:         ElementName,
    pub instance_number: u32,

    /// The specific parameter bindings for this instance.
    /// Two-level map per SRD-0006 `Assignments`, but inner is
    /// only this element's parameters.
    pub bindings:        BTreeMap<ParameterName, Value>,

    /// Fingerprint-based group level at which this instance was
    /// realised (SRD-0002 R26, reducto Stage Three / Rule 3).
    pub group_level:     u32,

    /// Reducto trial-code prefix this instance spans (for a
    /// trial-scoped instance) or `None` (for run-scoped).
    pub trial_code:      Option<String>,

    /// Resolved scope label (SRD-0002 §6.5): `Study`, `Trial`,
    /// or `Invocation`. Derived from group level and element type.
    pub scope:           InstanceScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceScope {
    Study,
    Trial,
    Invocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct InstanceId(String);   // format: "{element}:{instance_number}"

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
pub struct InstanceDependency {
    pub source:       InstanceId,
    pub target:       InstanceId,
    pub relationship: RelationshipType,          // from SRD-0007
}

impl ElementInstanceGraph {
    pub fn instance(&self, id: &InstanceId)       -> Option<&ElementInstance>;
    pub fn instances_of(&self, element: &ElementName) -> Vec<&ElementInstance>;
    pub fn edges_from(&self, id: &InstanceId)     -> Vec<&InstanceDependency>;
    pub fn edges_to  (&self, id: &InstanceId)     -> Vec<&InstanceDependency>;
    pub fn topological_order(&self)               -> Result<Vec<&ElementInstance>>;
    pub fn total_instances(&self)                 -> usize;
    pub fn instance_count(&self, element: &ElementName) -> usize;
    pub fn fingerprint(&self) -> Fingerprint;
}
```

This is the middle graph from SRD-0002 §6.4 — derived, but
authoritative for "how many instances and how are they wired." It
surfaces on `ExecutionPlan::element_instance_graph()` for UI,
tooling, and executor consumption (instance selection).

### `ExecutionPlan`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ExecutionPlan {
    pub id:                       ExecutionPlanId,

    /// Back-reference to the plan that produced this.
    pub source_plan_fingerprint:  Fingerprint,
    pub source_plan_id:           TestPlanId,

    pub execution_graph:          ExecutionGraph,
    pub element_instance_graph:   ElementInstanceGraph,

    pub resource_requirements:    ResourceRequirements,
    pub checkpoint_strategy:      Option<CheckpointStrategy>,

    pub trial_ordering:           TrialOrdering,         // from SRD-0008
    pub trial_elements:           Vec<ElementName>,      // identified by reducto
    pub max_concurrency:          Option<u32>,           // plan-wide cap, optional

    pub metadata:                 ExecutionPlanMetadata,
    pub labels:                   Labels,
    pub tags:                     Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExecutionPlanId(ulid::Ulid);

impl ExecutionPlan {
    pub fn fingerprint(&self) -> Fingerprint;

    pub fn steps(&self)       -> &[AtomicStep]  { self.execution_graph.steps() }
    pub fn barriers(&self)    -> impl Iterator<Item = &AtomicStep>;
    pub fn checkpoints(&self) -> impl Iterator<Item = &AtomicStep>;

    pub fn with_max_concurrency(&self, n: u32) -> ExecutionPlan;

    /// Produce a continuation plan starting from the given
    /// checkpoint. Resume behaviour is defined in the executor SRD.
    pub fn resume_from(&self, checkpoint: &CheckpointState) -> Result<ExecutionPlan>;
}
```

`ExecutionPlan` is pure data. It has no `execute()` method; the
executor SRD defines a trait that consumes `&ExecutionPlan`.

`with_max_concurrency(n)` returns a new plan with the plan-wide
concurrency cap overridden; per SRD-0002 R26, this propagates as
metadata on `Deploy` steps that don't have their own cap.

### `ResourceRequirements`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ResourceRequirements {
    pub peak_cpu_cores:    f64,
    pub peak_memory_mb:    u64,
    pub peak_storage_gb:   u64,
    pub peak_network_gbps: f64,

    /// Adopter-defined resources (e.g. "gpu", "fpga", "license_seats").
    pub custom: BTreeMap<String, u64>,
}

impl ResourceRequirements {
    pub fn none() -> Self { /* all zero */ }
}
```

Each step has its own requirements (in `StepHeader`); the plan-
level requirements are the peak across the whole execution graph,
computed by the compiler.

### `CheckpointStrategy`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct CheckpointStrategy {
    pub interval:              Option<std::time::Duration>,
    pub checkpoint_on_barriers: bool,
    pub checkpoint_on_errors:   bool,
    pub max_checkpoints:        Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct CheckpointId(String);
```

The compiler emits `AtomicStep::Checkpoint` nodes into the graph
according to this strategy. The executor writes actual checkpoint
state; that's the executor SRD's concern.

### `ExecutionPlanMetadata`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ExecutionPlanMetadata {
    pub compiled_at:           jiff::Timestamp,
    pub compilation_duration:  std::time::Duration,
    pub compiler_version:      String,
    pub optimization_level:    OptimizationStrategy,     // from SRD-0008

    pub trial_count:             u32,
    pub step_count:              u32,
    pub barrier_count:           u32,
    pub element_instance_count:  u32,

    pub estimated_duration:      Option<std::time::Duration>,
    pub performance_metrics:     PerformanceMetrics,
    pub optimization_report:     Option<OptimizationReport>,

    pub custom: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
pub struct PerformanceMetrics {
    pub critical_path_duration: Option<std::time::Duration>,
    pub total_duration:         Option<std::time::Duration>,
    pub maximum_parallelism:    u32,
    pub average_parallelism:    f64,
    pub speedup_factor:         f64,               // total / critical
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
pub struct OptimizationReport {
    pub per_trial_steps_eliminated:     u32,
    pub group_coalescings_applied:      u32,
    pub transitive_edges_removed:       u32,
    pub barriers_coalesced:             u32,
    pub deploys_saved:                  u32,
    /// Free-form notes from the compiler about optimizations
    /// applied, for UI display.
    pub notes: Vec<String>,
}
```

Plan fingerprint excludes metadata (timestamps, versions, and
optimisation reports don't define the plan's content). The plan
structure — graph + instance graph + resource reqs + checkpoint
strategy + trial ordering + trial elements + max concurrency +
source plan fingerprint — is what the fingerprint hashes.

### Plan fingerprint

```
PLAN_TAG (0x21)
  ‖ source_plan_fingerprint (32 bytes)
  ‖ execution_graph.fingerprint()  (32 bytes)
  ‖ element_instance_graph.fingerprint()  (32 bytes)
  ‖ resource_requirements.fingerprint()
  ‖ canonical_bytes(checkpoint_strategy)
  ‖ canonical_bytes(trial_ordering)
  ‖ u32_le(trial_elements.len()) ‖ sorted utf8_len_prefixed(names)
  ‖ Option<u32_le(max_concurrency)>
```

`ExecutionGraph::fingerprint()` canonicalises steps in `StepId`
order and hashes each step's canonical form (variant tag, header,
variant-specific fields) recursively. Labels and tags on the plan
itself — like on `TestPlan` — are not in the fingerprint.

## Decisions

- **D1.** `AtomicStep` is a Rust enum with struct variants, eight
  of them: `Deploy`, `Teardown`, `TrialStart`, `TrialEnd`, `Await`,
  `SaveOutput`, `Barrier`, `Checkpoint`. Upstream's `TrialStep`
  is dropped (its span is already captured between `Deploy` and
  `Teardown`/`Await`); `SaveOutput` is added as a first-class
  step.
- **D2.** Every variant begins with a `StepHeader` struct carrying
  the common fields (`id`, `depends_on`, `reason`, trial context,
  timing, resources, retry, metadata). Variant-specific fields
  follow.
- **D3.** `StepId` is a validated human-readable string following
  reducto's naming convention (e.g. `activate_jvector_t3`). Step
  IDs are deterministic — regenerating a plan from the same
  `TestPlan` produces byte-identical IDs, enabling diffable plans
  and checkpoint reuse.
- **D4.** `ExecutionGraph` is a `Vec<AtomicStep>` — edges are
  implicit in `step.header.depends_on`. Derived accessors
  (topological sort, critical path, parallel waves, subgraphs)
  compute from the step list; no separate edge record type.
- **D5.** Barriers are represented only as `AtomicStep::Barrier`
  variants. No parallel `Vec<Barrier>` on `ExecutionPlan` — the
  `barriers()` convenience accessor filters the step list.
- **D6.** Runtime barrier state (wait / release / fail) is **not**
  in this SRD. Only the plan record. The runtime `BarrierHandle`
  is in the executor SRD.
- **D7.** `ElementInstanceGraph` is a struct with
  `Vec<ElementInstance>` and `Vec<InstanceDependency>`. Each
  `ElementInstance` carries the element name, instance number,
  the specific parameter bindings, the group level, the optional
  trial code, and the resolved `InstanceScope` label
  (`Study`/`Trial`/`Invocation`, per SRD-0002 §6.5).
- **D8.** `InstanceId` is a string of form `"{element}:{instance_number}"`
  — deterministic and human-readable, same philosophy as `StepId`.
- **D9.** `ExecutionPlan` wraps both graphs plus `ResourceRequirements`,
  `CheckpointStrategy`, `TrialOrdering`, `trial_elements`,
  `max_concurrency`, and `ExecutionPlanMetadata`. It also carries
  the `source_plan_fingerprint` so results trace back.
- **D10.** `ExecutionPlan` is pure data. No `execute()` method.
  The executor SRD defines the trait that consumes it.
- **D11.** `ExecutionPlan.with_max_concurrency(n)` returns a new
  plan with the plan-wide cap overridden; propagates to `Deploy`
  step metadata where not otherwise set.
- **D12.** `ExecutionPlan.resume_from(&checkpoint)` returns a
  continuation plan. Details of resume logic are in the executor
  SRD; this SRD commits only to the method's existence and its
  non-mutating semantics.
- **D13.** `ResourceRequirements` is a struct with typed peak
  fields plus a `custom: BTreeMap<String, u64>` escape hatch.
  Both step-level and plan-level `ResourceRequirements` use the
  same shape.
- **D14.** `CheckpointStrategy` is a struct with interval, on-
  barriers, on-errors, and max-checkpoints fields. All optional.
- **D15.** `ExecutionPlanMetadata` carries compile-time facts
  (timestamps, durations, compiler version), aggregate counts
  (trial/step/barrier/instance), `PerformanceMetrics`, and an
  optional `OptimizationReport`.
- **D16.** Plan fingerprint is BLAKE3 over a canonical byte form
  that includes the source-plan fingerprint and both graphs'
  fingerprints. Labels, tags, ids, and metadata are not in the
  fingerprint.
- **D17.** `SaveOutput` is a first-class `AtomicStep` variant in
  paramodel, not a hyperplane-only extension. `OutputSelector`
  distinguishes typed-parameter capture (goes to
  `TrialResult.metrics`) from whole-volume capture (goes to
  `TrialResult.artifacts`).
- **D18.** `ShutdownReason`, `BarrierKind`, `TimeoutAction`, and
  `OutputSelector` are all serde-able enums with fixed variant
  sets; extensions are additive future variants.
