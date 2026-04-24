<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0010 — Compilation Pipeline (Reducto Port)

## Purpose

Specify the compilation pipeline that turns a `TestPlan` into an
`ExecutionPlan`. This is the **load-bearing SRD** of the paramodel
tier, per SRD-0002 R27: we retain reducto's full rule set verbatim
(in Rust terms) and walk it for corner cases before accepting.

Inputs: `&TestPlan` (SRD-0008).
Outputs: `Result<ExecutionPlan>` (SRD-0009) with its
`ElementInstanceGraph` and `ExecutionGraph`.

Upstream reference (authoritative):
`links/paramodel/paramodel-engine/src/main/java/io/nosqlbench/paramodel/engine/planners/reducto/reducto.md`
and the accompanying `rules/Rule*_*.java` files.

## Scope

**In scope.**

- The four stages of compilation: enumeration, seeding, structuring,
  linearisation.
- The 8 reduction rules inside Stage Three, verbatim: lifecycle
  expansion, dependency edges, group coalescing, trial
  notifications, health gates, concurrency annotations, start/end
  materialisation, transitive reduction.
- Mixed-radix trial enumeration and trial codes.
- Binding-state computation and propagation (SHARED/EXCLUSIVE only).
- Trial-element identification algorithm.
- The 25-row relationship-type composition table.
- The warnings catalogue (W001–).
- Lifecycle and coalescing-congruence invariants.
- A scrutiny pass that walks each rule for edge cases,
  inconsistencies, and known upstream bugs, and records our
  resolution.
- `Compiler` surface — the async-or-sync function that turns
  `&TestPlan` into `ExecutionPlan`.

**Out of scope.**

- Executing the plan — SRD Phase 1 item 9.
- Persisting plans/checkpoints — SRD Phase 1 item 10.
- Token-expression grammar — deferred per SRD-0007 D22 and
  SRD-0008.
- TCK conformance tests for the compiler — SRD Phase 1 item 11.

## Background

Reducto is upstream's production step planner. Its design is
thorough and our port preserves the whole thing. The Java
implementation leaves some rules partially stubbed
(`UnsupportedOperationException`) and the markdown notes
explicitly state that certain behaviours are not yet enforced in
code. Our Rust port implements everything reducto's spec
describes, with the scrutiny section below calling out where we
diverge or fix known bugs.

Reducto produces an `ExecutionGraph` and an `ElementInstanceGraph`
side by side. Both shapes are defined by SRD-0009; this SRD
defines how they are built.

## Pipeline at a glance

![Four-stage compilation pipeline: Stage 1 trial enumeration and binding state, producing the Element Instance Graph; Stage 2 naive graph seeding per-instance steps; Stage 3 the eight reducto rules (lifecycle expansion, relationship projection, group coalescing, dedicated propagation, serialization barriers, lifeline collapse, trial barriers, trial-element mark); Stage 4 graph linearisation. Input is TestPlan + Element Graph; output is ExecutionPlan.](diagrams/SRD-0010/pipeline.png)

The compiler is *deterministic* — same TestPlan + element graph
produces the same ExecutionPlan every time (fingerprint-stable
per the invariants below).

## The Compiler surface

```rust
pub trait Compiler: Send + Sync + 'static {
    fn compile(&self, plan: &TestPlan) -> Result<ExecutionPlan, CompilationError>;

    fn compile_incremental(
        &self,
        plan: &TestPlan,
        previous: &ExecutionPlan,
    ) -> Result<ExecutionPlan, CompilationError>;

    fn validate(&self, plan: &TestPlan) -> Vec<CompilationDiagnostic>;

    fn version(&self) -> &str;
    fn options(&self) -> &CompilerOptions;
}

pub struct DefaultCompiler { /* holds CompilerOptions + CustomOrderingRegistry */ }
impl Compiler for DefaultCompiler { /* pipeline described below */ }

#[derive(Debug, Clone)]
pub struct CompilerOptions {
    pub include_health_gates:         bool,  // default true
    pub include_transitive_reduction: bool,  // default true
    pub include_checkpoints:          bool,  // default true
    pub max_warnings:                 Option<u32>,
    pub fail_on_warning_codes:        BTreeSet<WarningCode>,
    pub custom_ordering_registry:     CustomOrderingRegistry,
}

pub struct CompilationDiagnostic {
    pub severity: Severity,         // Info | Warn | Error
    pub code:     WarningCode,      // W001..; "E001" for errors
    pub message:  String,
    pub location: DiagnosticLocation,
    pub suggestion: Option<String>,
}
```

Compilation is synchronous — it's CPU-bound, deterministic, and
operates on in-memory data. The `Compiler` trait is `Send + Sync`
so callers in async contexts can offload it to a blocking pool.

A `CompilationError` aggregates every `Error`-severity diagnostic
produced during compilation; the compiler reports all it finds
before bailing, rather than failing at the first.

## Stage One — Trial enumeration and binding state

### 1.1 Trial space

Given a `TestPlan` with axes `A₀, A₁, …, A_{N-1}` (in authored
order), the total trial count is the product of the axis
cardinalities:

```
trial_count = ∏  A_i.values.len()
              i=0..N-1
```

Parameters that do not appear as axes (fixed parameters) are
bound by the plan's or element's `Configuration` / defaults and
contribute a single constant value to every trial — they neither
have a rank nor affect trial count. Two element prototypes that
differ only in fixed parameter values are **distinct** prototypes
in the element stack. Fixed parameters are always "bound" for the
purposes of binding-state computation.

### 1.2 Axis ranks

Parameters that appear on axes are ranked depth-first across the
ordered element stack. For element `E_j` with axis-bearing
parameters `P_{j,0}, P_{j,1}, …` (in declaration order on the
element), the rank assignment is:

```
rank(E_j.P_{j,k}) = Σ   |axes_on(E_m)|   +   k
                   m<j
```

The rank determines the parameter's position in the mixed-radix
trial-number encoding; rank 0 is the most significant digit
(changes least often), rank N-1 is the least significant (changes
every trial).

### 1.3 Mixed-radix decomposition

Let `C_i` be the cardinality of the axis at rank `i`, and let
`stride[i]` be the product of all more-significant cardinalities
beyond `i` — that is, `stride[i] = C_{i+1} × C_{i+2} × … × C_{N-1}`,
with `stride[N-1] = 1`.

Given a trial number `T ∈ [0, trial_count)`, the parameter offset
vector is:

```
offset[i] = (T / stride[i]) mod C_i
```

The reverse mapping is:

```
T = Σ  offset[i] × stride[i]
    i
```

Both directions are bijections. Offsets and trial numbers are
therefore interconvertible without enumerating the parameter
space.

### 1.4 Trial codes

A **trial code** is a human-readable encoding of the offset
vector. The format is a hex-like string prefixed `0x`:

- If `max_i C_i ≤ 16`, each digit is one hex character (4 bits)
  encoding the offset for that rank.
- Otherwise, each digit is two hex characters (8 bits), i.e. the
  whole code widens to give every axis 16-bit resolution.

Ranks that correspond to fixed-value axes (cardinality 1) are
included as `0`, preserving positional alignment.

Worked examples (from reducto):

```
axes = a:[1,2,3]  b:[asm,dra,ghi]  c:[yo]
  Trial 4 → offsets [1, 1, 0] → code "0x110"  (4-bit digits)

axes = v1:[a,b,c]  v2:[u,v]  v3:[w,x,y,z]
  Trial 10 → offsets [1, 0, 2] → code "0x102"  (4-bit digits)

axes = a:[0..16]  b:[what, up]
  (17 values on a → 16-bit digits)
  Trial 37 → offsets [2, 0] → code "0x0200"
```

Trial codes are stamped onto graph nodes as `trial_code` label
metadata (see SRD-0002 §6.5 scope-name pinning) and are carried
through linearisation into `NotifyTrialStart` / `NotifyTrialEnd`
as `StepHeader.trial_code`.

### 1.5 Binding levels per element

An element `E` with K axis-bearing parameters spanning ranks
`R..R+K-1` has **own binding level** `R+K`: it becomes concretely
bound once all K of its own axis-bearing parameters have been
chosen, which happens at the R+K-th digit in the mixed-radix
enumeration.

Group levels 0..N:

| Level | Meaning                                                                   |
|-------|---------------------------------------------------------------------------|
| 0     | Run-scope. Exactly one instance spans all trials.                         |
| K..N  | One instance per unique fingerprint of the K most-significant axis values; persists across contiguous trial blocks where those K values are unchanged. |
| N     | Trial-scope. One instance per trial (deepest level).                      |

An element is **concretely bound** at group level L iff all the
element's axis-bearing parameter ranks are `< L`. It is
**unbound** at levels below that — there is no intermediate
"partially bound" state because an element's axis-bearing
parameters occupy contiguous ranks.

### 1.6 Binding-level propagation

An element's *effective* binding level is the maximum of its own
binding level and the effective binding levels of its upstream
dependencies for `SHARED` and `EXCLUSIVE` edges only:

```
effective(E) = max(
    own_binding_level(E),
    max { effective(D.target) | D ∈ E.dependencies, D.relationship ∈ {Shared, Exclusive} }
)
```

`DEDICATED` and `LIFELINE` edges do **not** participate in this
propagation:

- `DEDICATED` targets coalesce with their owner separately (see
  Rule 3).
- `LIFELINE` targets are subsumed by their source's deactivation
  (see Rule 2) and do not influence binding scope.

`LINEAR` edges: reducto's markdown is silent on propagation for
LINEAR. The existing Java `BindingStateComputer` treats LINEAR
the same as SHARED/EXCLUSIVE for propagation. Our port matches
that (see Scrutiny §S.3).

### 1.7 Computed artefacts from Stage One

After Stage One, the compiler holds:

- `trial_count: u64`
- For each trial `T ∈ [0, trial_count)`: offset vector + trial
  code.
- For each element `E`: own binding level and effective binding
  level.
- For each group level `L` and each element `E` with effective
  binding level `≤ L`: the set of contiguous trial ranges in
  which `E`'s bound parameter values are constant.

These artefacts feed Stages Two and Three.

## Stage Two — Naive graph seeding

`GraphSeeder` creates one `TRIAL_SEED` node per trial. No edges.
The node carries the trial index; downstream rules expand it
into element lifecycle nodes (Rule 1).

```rust
pub(crate) enum ReductoNode {
    TrialSeed        { trial_index: u32, trial_code: String },
    Activate         { element: ElementName, instance: InstanceId, trial_index: u32, group_index: Option<u32> },
    Deactivate       { element: ElementName, instance: InstanceId, trial_index: Option<u32>, group_index: Option<u32> },
    Await            { element: ElementName, instance: InstanceId, trial_index: u32 },
    SaveOutput       { element: ElementName, instance: InstanceId, trial_index: u32, selector: OutputSelector },
    NotifyTrialStart { trial_index: u32, trial_code: String, element_names: Vec<ElementName> },
    NotifyTrialEnd   { trial_index: u32, trial_code: String, element_names: Vec<ElementName>, shutdown_reason: ShutdownReason },
    ReadinessGate    { element: ElementName, instance: InstanceId, spec: HealthCheckSpec },
    Barrier          { id: BarrierId, kind: BarrierKind, timeout: Option<Duration>, timeout_action: TimeoutAction },
    Checkpoint       { id: CheckpointId },
    Start,
    End,
}

pub(crate) struct ReductoGraph {
    nodes: HashMap<NodeId, ReductoNode>,
    edges: HashMap<NodeId, BTreeSet<NodeId>>,    // forward: from → tos
    metadata: HashMap<NodeId, NodeMetadata>,
}
```

`ReductoGraph` is the compiler's internal working data structure.
It is mutated through Stages Two and Three and linearised in
Stage Four into the public `ExecutionGraph`. The `ReductoGraph`
type is private to the `paramodel-compiler` crate; callers never
see it.

## Stage Three — The 8 rules

Each rule is a mutation of `ReductoGraph`. Rules run in the order
below. Each rule is idempotent in the sense that re-running it
on its own output yields the same graph — the suite as a whole is
not iterated; one pass of rules 1→8 produces the final graph.

All rules share a `RuleContext` providing: the topologically
sorted element stack, the `BindingStateComputer` from Stage One,
the trial enumerator, the set of trial-element names (§ Trial-
element identification), the lifeline clusters, and the DEDICATED
reverse-dependency map (owner-per-target).

### Rule 1 — Element lifecycle expansion

Replace each `TrialSeed(T)` node with a subgraph of per-element
lifecycle nodes. For each element `E` and each trial `T_i`:

```
if E is a command-mode trial element:
    emit  Activate(E, instance_i, T_i)
    emit  Await   (E, instance_i, T_i)
else:
    emit  Activate  (E, instance_i, T_i)
    emit  Deactivate(E, instance_i, T_i)
```

Command trial elements receive no `Deactivate` node — they self-
terminate, and `Await` marks the trial's operative end.

Node-id convention (matches reducto):
- `activate_{element}_t{trial_index}`
- `deactivate_{element}_t{trial_index}`
- `await_{element}_t{trial_index}`

No inter-element edges at this stage — Rule 2 adds those. Original
`TrialSeed` nodes are removed. Result: `N × E` lifecycle
subgraphs (N trials × E elements), disconnected.

### Rule 2 — Dependency edge materialisation

For each `Dependency { target, relationship }` declared on each
element, add edges by relationship type. In the patterns below,
`deactivate(E, T_i)` stands for whichever termination node applies
(deactivate for service elements, await for command trial
elements).

**SHARED.** Element `X` shares element `Y`. For each trial `T_i`:
```
activate(Y, T_i)    →  activate(X, T_i)
deactivate(X, T_i)  →  deactivate(Y, T_i)
```
`X` cannot activate until `Y` is active. `X` must deactivate before
`Y` deactivates. Concurrent dependents of `Y` are allowed.

**EXCLUSIVE.** Same edges as SHARED, plus a serialisation
constraint: no two exclusive dependents of `Y` may be active at
the same time. For distinct element prototypes `X` and `Z` both
exclusively depending on `Y`, add across-trial serialisation
edges:
```
deactivate(X, T_i) → activate(Z, T_j)   where T_j is the next trial requiring Z after T_i
```
For the same element prototype across consecutive trials where
`Y` is the same *instance* (i.e. `Y` is in the same coalesced
group at `T` and `T+1`):
```
deactivate(X, T)    → activate(X, T+1)
```
If two different element prototypes both exclusively depend on
`Y` within the *same* trial, that is a configuration error —
**W002** (§ Warnings).

Instance-scoped serialisation: add the edge only when `Y` is the
same instance at consecutive trials; across trials where `Y` is
a different instance (different group), no serialisation edge is
needed — the teardown/reactivation cycle satisfies exclusivity
implicitly. When `Y` coalesces at a much broader scope than its
exclusive dependents, the serialisation effectively spans a wide
range — emit **W001**.

Rule 2 runs *before* Rule 3 (group coalescing), so all EXCLUSIVE
edges are initially on per-trial nodes; Rule 3 remaps them to the
surviving coalesced nodes.

**DEDICATED.** `X` has a dedicated instance of `Y`. Each instance
of `X` gets its own `Y` instance:
```
activate(Y_for_X, T_i)  →  activate(X, T_i)
deactivate(X, T_i)      →  deactivate(Y_for_X, T_i)
```
The dedicated `Y` instance is tagged `dedicated_to: X` in node
metadata and participates in `X`'s coalescing (Rule 3). If `X` is
coalesced across a group, its dedicated `Y` is also coalesced
with it.

**LINEAR.** `X` must activate only after `Y` has fully completed
(activated and deactivated) within the same trial scope:
```
deactivate(Y, T_i)  →  activate(X, T_i)
```

Same-group-scope check: the LINEAR edge is added **only** when
`X` and `Y` share the same configuration group (same effective
binding-level fingerprint at the relevant group level). Upstream's
Java code currently applies LINEAR unconditionally — we
implement the check. See Scrutiny §S.1 for details.

**LIFELINE.** `X` has a lifeline to `Y`. When `Y` deactivates,
`X` is implicitly deactivated as a side-effect. Add only the
activation edge:
```
activate(Y, T_i)  →  activate(X, T_i)
```
Remove `deactivate(X, T_i)` from the graph entirely. Any edges
that targeted `deactivate(X, T_i)` are **remapped** onto
`deactivate(Y, T_i)`.

Lifeline clusters (multiple elements connected transitively by
LIFELINE edges): all members' deactivation edges collapse onto
the cluster's **root** deactivation (the element no LIFELINE
dependency names as its target).

### Rule 3 — Group coalescing (the reduction)

For each non-trial element `E`, fold consecutive trials where
`E`'s bound parameter values are identical into a single
activate/deactivate pair.

Algorithm sketch:

```
for each non-trial element E in topological order:
    let L = effective_binding_level(E)
    for each level-L group G (contiguous trial range with unchanged bindings):
        pick the first trial T_first in G
        keep activate(E, T_first) as the group's activation
        keep the last trial's deactivate/await as the group's deactivation
        for every other trial T in G:
            remove activate(E, T)   and deactivate(E, T)
            remap every incoming edge to the kept group node
            remap every outgoing edge to the kept group node
```

**Coalescing Congruence Principle.** Whenever per-trial lifecycle
nodes are merged into a group-level node, every edge referencing
any removed node must be remapped to the surviving group node.
This applies to:

1. **Activation congruence.** K per-trial activations → one group
   activation; all incoming and outgoing edges of the K-1 removed
   activates are remapped.
2. **Deactivation congruence.** K per-trial deactivates (or
   awaits) → one group deactivate; same edge-remap treatment,
   including the *first* trial's deactivate (which is removed, not
   preserved — the *last* trial's deactivate becomes the group
   deactivate). Loops that walk `i=1..K-1` must also handle `i=0`
   for deactivation.
3. **Dependency congruence.** When a non-trial element's
   deactivation depends on trial-scoped events
   (`NotifyTrialEnd`), the dependency must cover **all** trials
   in the group, not just the last — otherwise trials running
   concurrently within a group can leak past the deactivation
   boundary. See Rule 4's group-deactivation-ordering subsection.

Constraints:

- **Trial elements are never coalesced.** Even if their
  configuration is identical across consecutive trials, each
  must deactivate and reactivate to produce per-trial
  notifications and per-trial outcomes.
- **DEDICATED targets coalesce with their owner.** The effective
  binding level for a DEDICATED target is resolved by walking the
  DEDICATED ownership chain and taking the maximum binding level
  on the path — because a varying parameter might live on an
  interior element of the chain rather than the root.
- **Run-scoped elements (binding level 0, no axes touching them)
  coalesce to a single activate/deactivate spanning the entire
  graph**, placed near `Start` and `End`.
- **Elements must be concretely bound to be activatable**:
  in-place reconfiguration is not supported; at every group
  boundary where bound values change, the element deactivates
  and re-activates.

Group-boundary dependency ordering: if `A` depends on `B` and
both change at the same group boundary, `A` deactivates before
`B`, and `B` activates before `A` (reverse dependency order for
teardown, forward for activation).

### Rule 4 — Trial notification insertion

For each trial `T_i`, insert two notification nodes:

- `NotifyTrialStart(T_i)` — signals non-trial elements that a
  trial is about to open. Depends on: every non-trial element's
  current activation (per-trial if not coalesced, or the group
  activation if coalesced).
- `NotifyTrialEnd(T_i)` — signals a trial has closed. Depends on:
  every trial element's deactivation/await for `T_i`.

Ordering:
```
all non-trial activate(E, ...)    →  notify_trial_start(T_i)  →  all trial activate(E, T_i)
all trial deactivate/await(E, T_i) →  notify_trial_end(T_i)
```

**Coalesced-activate resolution (Rule 4's `find_activate_for_trial`).**
When Rule 3 coalesced a non-trial element, the per-trial
`activate(E, T_i)` node no longer exists. Resolution:

1. Look up the per-trial node; if found, use it.
2. Else collect every remaining `activate(E, …)` node.
3. If exactly one remains (single-group or run-scoped), use it.
4. If multiple groups exist, match by `group_index` for `T_i`
   from `BindingStateComputer::group_index_for_element(E, T_i)`.

**Group-deactivation ordering.** A coalesced non-trial element's
deactivation must depend on `notify_trial_end(T_i)` for **every**
trial `T_i` in the outgoing group, not just the last. For
sequential trials these edges are redundant (each trial
completes before the next begins); for concurrent trials within
the group they are *essential* — without them, a non-trial
element can begin deactivation while earlier trials'
`notify_trial_end` is still in flight, producing a race against
any synchronous work the notification triggers.

```
notify_trial_end(T_i)  →  deactivate(E_coalesced, G)   for every T_i in G
```

**DEDICATED target handling.** DEDICATED targets whose owner is
a trial element are not coalesced by Rule 3 (each trial gets its
own dedicated instance). Their per-trial deactivation nodes
therefore remain; each must be wired to its own
`notify_trial_end(T_i)`:

```
notify_trial_end(T_i)  →  deactivate(dedicated_target, T_i)   per trial T_i
```

**Exclusive serialisation rerouting.** Rule 2 created direct
exclusive serialisation edges of the form
`deactivate(X, T_i) → activate(X, T_{i+1})` (and cross-element
variants). After notifications are inserted, reroute these edges
through the notification boundaries **for trial elements only**:

```
Before:  deactivate(X, T_i)  →  activate(X, T_{i+1})
After:   deactivate(X, T_i)  →  notify_trial_end(T_i)
                               →  notify_trial_start(T_{i+1})
                               →  activate(X, T_{i+1})
```

The pre-edges `deactivate/await(X, T_i) → notify_trial_end(T_i)`
and `notify_trial_start(T_{i+1}) → activate(X, T_{i+1})` already
exist from Rule 4's main wiring; the direct cross-trial edge is
removed and replaced with a `notify_trial_end → notify_trial_start`
edge. A control path can no longer pass through two
`notify_trial_start`s concurrently while downstream trial
elements are mutually exclusive.

**Non-trial elements are *not* rerouted.** For a non-trial
element `B` with an EXCLUSIVE dependency, the notify wiring
direction is opposite: `activate(B) → notify_trial_start` and
`notify_trial_end → deactivate(B)`. Rerouting
`deactivate(B, T_i) → activate(B, T_{i+1})` through the notify
boundaries would allow two `B` instances to be active
simultaneously under certain orderings. Non-trial serialisation
edges stay as direct edges.

**Non-trial deactivation enforcement.** After all `notify_trial_end`
nodes are wired to group deactivations, remove any direct edges
from trial-element termination nodes to non-trial element
deactivations. The correct control path is
`trial_terminate(X, T_i) → notify_trial_end(T_i) → deactivate(non_trial, G)`;
the direct edge is redundant and can allow concurrent
deactivation if left in place.

### Rule 5 — Health-check readiness gates

For each `Activate(E, …)` whose element has a
`HealthCheckSpec`, insert a `ReadinessGate(E, …)` between the
activate and every step that depends on `E` being ready:

```
activate(E, …)  →  readiness_gate(E, …)  →  [dependent nodes]
```

The gate represents the health-check loop: the executor retries
per the element's `HealthCheckSpec` until ready. The executor SRD
owns the runtime semantics.

Do **not** insert the gate between `activate(E, …)` and `E`'s own
`deactivate/await` node — the direct edge must remain so the
element can still be torn down even if the health check never
passed. When the gate fails (exhausted retries or timeout), it
fails and the element's error policy is invoked.

### Rule 6 — Concurrency annotation

For each element `E` with `max_concurrency` or
`max_group_concurrency` set (SRD-0002 R26), annotate each of
`E`'s `Activate` nodes with the limit:

```
node.metadata["max_concurrency"]       = E.max_concurrency
node.metadata["max_group_concurrency"] = E.max_group_concurrency
```

These are **declarative directives** the executor observes at
runtime; they do **not** become structural dependency edges in
the graph. Reducto is explicit about this choice:

> "encoding concurrency limits as structural dependency edges in
> the graph … becomes explosively complex when execution occurs
> out of order or across a large number of elements. Such
> structural encoding risks creating arbitrary blocking
> conditions that deoptimize the entire execution graph."

### Rule 7 — Start and end materialisation

Add two sentinel nodes:

- `Start` — connected *to* every node with no incoming edges
  (the current roots of the graph after rules 1–6).
- `End` — every node with no outgoing edges is connected *to*
  `End`.

Validate acyclicity; a cycle at this point indicates a
configuration error in the element dependencies (or a bug in the
rules). Emit `Error::GraphHasCycle { path }` with the offending
path.

### Rule 8 — Transitive reduction

After rules 1–7 have assembled the graph, perform a standard
transitive reduction: for each node `N`, for each successor `S`
of `N`, if `S` is reachable from any other successor of `N` by
a non-direct path, remove the direct edge `N → S`.

Reachability is preserved — the set of nodes reachable from any
given node is unchanged — but the visible edge set shrinks to
the minimum required to express the ordering. This makes
linearised plans cleaner and reduces runtime edge-processing
overhead.

Implementation: BFS/DFS from each node's direct successors,
omitting the edge being tested; if the successor is still
reachable via any other path, drop the direct edge.

Algorithmic complexity: `O(V × (V + E))` in the worst case
(a BFS per edge). Acceptable for plan sizes observed in practice
(reducto notes no issue in upstream workloads).

## Post-rule validation and metadata stamping

After Rules 1–8:

1. **Lifecycle invariant validation.** For every element `E`:
   - `count(Activate nodes for E) == count(Deactivate/Await nodes for E)`
   - unless `E` has a `LIFELINE` dependency — then
     `count(Deactivate nodes) == 0` (subsumed by the lifeline
     target's deactivation).
   
   A mismatch indicates a coalescing or edge-remapping bug in
   the rules. Emit
   `Error::LifecycleInvariantViolation { element, activates, deactivates }`.

2. **Trial-code stamping.** For every node with a non-`None`
   `trial_index`, compute the trial code from the enumerator and
   stamp it on the node's metadata (`trial_code` label). This
   flows through linearisation into `NotifyTrialStart` /
   `NotifyTrialEnd` step bodies.

## Stage Four — Graph linearisation

`GraphLinearizer` topologically sorts the `ReductoGraph` and maps
each node to an `AtomicStep` (SRD-0009). Edges are materialised
into each `StepHeader.depends_on`.

Mapping:

| ReductoNode          | AtomicStep             | Notes |
|----------------------|------------------------|-------|
| `Start`              | (sentinel step id `start`)     | Header has no dependencies; trial_index = None. |
| `End`                | (sentinel step id `end`)       | Header depends_on covers every otherwise-terminal node. |
| `Activate`           | `AtomicStep::Deploy`   | Carries element, instance_number, resolved configuration. |
| `Deactivate`         | `AtomicStep::Teardown` | Carries element, instance_number, collect_artifacts flag. |
| `Await`              | `AtomicStep::Await`    | Carries element, instance_number, trial_id. |
| `SaveOutput`         | `AtomicStep::SaveOutput` | Carries element, instance_number, trial_id, OutputSelector. |
| `NotifyTrialStart`   | `AtomicStep::TrialStart` | Carries trial_id, element_names. |
| `NotifyTrialEnd`     | `AtomicStep::TrialEnd`   | Carries trial_id, element_names, shutdown_reason. |
| `ReadinessGate`      | `AtomicStep::Barrier` with `BarrierKind::ElementReady` | |
| `Barrier`            | `AtomicStep::Barrier`  | kind as given. |
| `Checkpoint`         | `AtomicStep::Checkpoint` | |
| `TrialSeed`          | *(error — must have been expanded by Rule 1)* | |

Sentinel `Start`/`End` in the public `ExecutionGraph` are
represented using `AtomicStep::Checkpoint` with a fixed
`checkpoint_id` of `start` / `end` respectively. Rationale: no
new variant needed; the checkpoint shape already has the
depends_on machinery. The executor SRD may add a fast-path for
these reserved ids.

(Alternatively: reserve sentinel `StepId`s `start` / `end` with
variant `Barrier` and kind `Custom`. The executor treats them as
no-ops. Decided in the executor SRD; this SRD just records the
obligation to produce them.)

### Instance tracking

The linearizer maintains a per-element instance counter. Each
`Activate` node increments it, producing the `instance_number`
for that deploy. `Deactivate` / `Await` / `SaveOutput` for the
same element reference the current (most recent) instance number.

Instance numbers are monotonically increasing within a plan and
uniquely identify each instance in combination with the element
name.

### Configuration overlay

For each `Deploy` step, the linearizer computes the resolved
configuration by overlaying:

1. The element's `Configuration` (plus any token-resolved values
   — the token grammar layer resolves these before linearisation).
2. The plan's `PlanBindings` (SRD-0008) if present for this
   (element, parameter).
3. The trial's axis bindings for the trial-index on this step.

The resulting `ResolvedConfiguration` is fully concrete — every
parameter has a `Value`, no tokens. See SRD-0008 D6 for the full
resolution chain.

### Element-dependency metadata

Each `Deploy` step carries an `element_deps` label listing the
element names that element directly depends on. This metadata is
used by the next step to build the `ElementInstanceGraph`.

### Building the `ElementInstanceGraph`

After linearisation, the compiler scans the step list and
synthesises the `ElementInstanceGraph` per SRD-0009:

1. Scan `Deploy` steps to discover every (element, instance_number)
   pair; record its `ResolvedConfiguration` as `ElementInstance.bindings`.
2. Build a `trial_index → trial_code` lookup from `TrialStart` steps.
   Each instance carries the trial code of the trial it first appeared
   in (or `None` if run-scoped).
3. Extract transitive element-level dependencies from each `Deploy`'s
   `element_deps` metadata; compute closure with BFS.
4. For each `Deploy`, walk its `depends_on` chain through non-`Deploy`
   steps (barriers, notifies) until reaching upstream `Deploy` steps;
   filter those edges against the transitive element-dependency set to
   prevent spurious edges from notification fan-in, then emit as
   `InstanceDependency` records. When a `Deploy` for the same element
   appears (serial reuse within a group), BFS walks through it to find
   the upstream instances.
5. Compute topological order via Kahn's algorithm.

The resulting graph is exposed via
`ExecutionPlan::element_instance_graph`.

## Trial-element identification

`identify_trial_elements(&TestPlan) -> BTreeSet<ElementName>` is
called before Rule 1 runs. The algorithm is scope-aware and
override-respecting:

1. **Explicit overrides first.** For each element `E`:
   - `E.trial_element == Some(true)` → forced-on; always
     included.
   - `E.trial_element == Some(false)` → forced-off; excluded from
     candidacy.

2. **Candidate pool construction.**
   - If any element has a non-run-scoped axis binding (at least
     one axis targets a parameter that belongs to a non-run-
     scoped element), the candidate pool is the set of
     trial-scoped elements (elements whose effective binding
     level > 0).
   - If no axes are present (single-trial plan), the candidate
     pool is all non-floating elements. A *floating* element has
     no outgoing dependencies AND no element depends on it. It's
     excluded from trial scope because it's unmoored from the
     study.

3. **Leaf selection.** Within the candidate pool, a trial element
   is one that no other candidate depends on. This selects the
   *most-dependent* (innermost) elements as the trial boundary.

4. **Union.** The final set is
   (forced-on elements) ∪ (leaves from step 3) − (forced-off
   elements).

Result: the set of element names that are trial elements.

## Relationship-type composition (25 rows, A → B → C)

When three elements form a chain — `A` depends on `B` via `R1`,
and `B` depends on `C` via `R2` — the composed behaviour follows
from independent application of each edge's rules. All 25
combinations are structurally valid (no combination produces a
rule error by itself; warnings may be emitted where specific
scoping creates concerns). The table below is the canonical
reference for downstream consumers (executor, UI). It is
preserved from reducto verbatim, adjusted only for name casing.

| `R1 (A→B)` | `R2 (B→C)` | Summary                                                                                          |
|-----------|-----------|---------------------------------------------------------------------------------------------------|
| Shared    | Shared    | Standard transitive sharing. `C → B → A`. Concurrent instances at every level.                    |
| Shared    | Exclusive | `B` serialises on `C`. `A` shares `B` normally. Exclusivity between `B` and `C` only.             |
| Shared    | Dedicated | Each `B` gets its own `C`. `A` shares `B`. `C`'s lifecycle tied to `B`'s.                         |
| Shared    | Linear    | `C` fully completes before `B` activates. `A` shares `B`. Order: `act(C)→deact(C)→act(B)→act(A)`. |
| Shared    | Lifeline  | `B` dies when `C` dies. `A` shares `B` and deactivates before `B`.                                |
| Exclusive | Shared    | `A` serialises on `B`. `B` shares `C`. Exclusivity is `A↔B` only.                                  |
| Exclusive | Exclusive | Double serialisation chain. One `A` at a time, within which one `B` at a time.                   |
| Exclusive | Dedicated | Each `B` gets its own `C`. `A` serialises on `B`. Isolated chain during `A`'s lifetime.           |
| Exclusive | Linear    | `C` fully completes → `B` activates → `A` serialises on `B`.                                      |
| Exclusive | Lifeline  | `A` exclusively accesses `B`; `B` dies when `C` dies.                                              |
| Dedicated | Shared    | Each `A` gets its own `B`. All `B`s share `C`. Trial-element `A` → per-trial `B`.                 |
| Dedicated | Exclusive | **Notable Rule 4 interaction.** Each `A`'s `B` is non-trial (not coalesced when owner is trial). Rule 2 creates serialisation edges between `B` instances. Rule 4's reroute applies only to trial elements, so `B`'s serialisation edges are left as direct edges. Rerouting non-trial `B` through notify boundaries would allow overlap. |
| Dedicated | Dedicated | Full isolation. Each `A` has its own `B`, each `B` has its own `C`.                                |
| Dedicated | Linear    | Each `A` has its own `B`; `C` completes before `B` activates.                                      |
| Dedicated | Lifeline  | Each `A` has its own `B`; `B` dies when `C` dies; `A` must deactivate before `B` dies.             |
| Linear    | Shared    | `B` completes → `A` starts. `B` shares `C` during its lifetime.                                    |
| Linear    | Exclusive | `B` completes → `A` starts. During `B`'s life, `B` exclusively accessed `C`.                       |
| Linear    | Dedicated | Full sequential with dedicated backing: `C` (dedicated to `B`) → `B` → `A`.                       |
| Linear    | Linear    | Fully sequential chain: `act(C)→deact(C)→act(B)→deact(B)→act(A)`.                                 |
| Linear    | Lifeline  | `B` dies when `C` dies; LINEAR requires `B` to deactivate before `A`, so `C` kills `B` → `A` runs. |
| Lifeline  | Shared    | `A` dies when `B` dies. `B` shares `C`. `A` has no explicit deactivation step.                     |
| Lifeline  | Exclusive | `A` dies when `B` dies. `B` serialises on `C`.                                                     |
| Lifeline  | Dedicated | `A` dies when `B` dies. Each `B` has its own `C`.                                                  |
| Lifeline  | Linear    | `C` completes → `B` activates (`A` implicitly activates) → `B` deactivates (`A` dies).             |
| Lifeline  | Lifeline  | Transitive lifeline cluster. `C`'s deactivation kills both `B` and `A`. Cluster root is `C`; all deactivation edges remap to `C`'s deactivation. |

Notable interactions that the compiler has to get right:

- **DEDICATED → EXCLUSIVE** (row 12): non-trial `B` must have
  direct serialisation edges, not rerouted through notify.
- **EXCLUSIVE → EXCLUSIVE**: double serialisation is a fully
  sequential pipeline — emit W001 if the outer exclusive binds
  at a broad scope.
- **LINEAR → LINEAR**: fully sequential lifecycles; useful for
  multi-phase pipelines.
- **LIFELINE → LIFELINE**: cluster rooted at `C`; only `C` has a
  deactivation step.

Only one relationship type is permitted per ordered pair `(X, Y)`.
Multiple types on the same pair are a configuration error. Some
combinations are inherently contradictory:

- `LIFELINE + LINEAR` — LIFELINE subsumes deactivation into the
  target's; LINEAR requires the target to fully deactivate before
  the source activates. Incompatible.
- `DEDICATED + SHARED` — dedicated forbids sharing.
- `EXCLUSIVE + DEDICATED` — DEDICATED is strictly stronger than
  EXCLUSIVE; the combination is redundant *and* confusing.

Detection happens at plan validation. Emit
`Error::ConflictingRelationshipTypes { pair, types }`.

## Warnings catalogue

| Code | Severity | Condition                                                                                   | Message template |
|------|----------|---------------------------------------------------------------------------------------------|------------------|
| W001 | Warn     | An EXCLUSIVE dependent's target coalesces at a much broader scope than the dependent.       | "Element '{X}' exclusively depends on '{Y}' which is scoped to level {K}. This serialises all {N} exclusive dependents of '{Y}' across the entire level-{K} group. Consider narrowing the exclusivity scope or verify full serialisation is intended." |
| W002 | Error    | Two distinct element prototypes both exclusively depend on the same target within the same trial scope. | "Elements '{X}' and '{Z}' both exclusively depend on '{Y}' within trial {T}. Mutual exclusivity cannot be satisfied when both must be active simultaneously. Restructure dependencies so at most one element exclusively depends on '{Y}' per trial." |
| W003 | Warn     | `EdgeFirst` trial ordering paired with a sampling strategy that may not include boundary points (per SRD-0008 D15). | "EdgeFirst ordering requires a sampling strategy that guarantees boundary points; {strategy} does not." |
| W004 | Warn     | An element has `max_concurrency > max_group_concurrency * group_count(E)` — the global cap is effectively unreachable. | "`max_concurrency` on element '{E}' ({n}) exceeds the maximum reachable instance count ({reachable}) across all groups; the cap is never active." |
| W005 | Info     | Transitive reduction removed `k` edges. | "Transitive reduction removed {k} edges from the execution graph." |
| W006 | Warn     | A LIFELINE cluster contains more than two elements and its root is coalesced. | "Lifeline cluster rooted at '{E}' is coalesced; cluster members {members} will all survive beyond their owners' lifetimes within the group. Verify this is intended." |

Additional warnings may accrue as the scrutiny pass identifies
them. `CompilerOptions.fail_on_warning_codes` can escalate any
warning to an error.

## Invariants

Compilation must preserve:

1. **Acyclicity.** The final `ExecutionGraph` is a DAG.
2. **Lifecycle counts.** Every element has equal Activate and
   Deactivate/Await counts — unless the element has a LIFELINE
   dependency, in which case its Deactivate count is zero.
3. **Coalescing congruence** (Rule 3 preamble). Every edge
   referencing a removed per-trial lifecycle node is remapped to
   the surviving group-level node.
4. **Notification wrap.** Every trial element's
   activate+deactivate/await lives between
   `notify_trial_start(T_i)` and `notify_trial_end(T_i)`.
5. **Group-end coverage.** A coalesced non-trial element's
   deactivation depends on every `notify_trial_end` in its
   outgoing group, not just the last.
6. **Trial-code presence.** Every node whose context includes a
   trial index has a `trial_code` label.
7. **Sentinels.** Exactly one `Start` node (no incoming edges)
   and one `End` node (no outgoing edges) exist.
8. **Transitive minimality.** After Rule 8, no edge `N → S`
   exists if an alternative path from `N` to `S` also exists.

Each invariant has a validator that runs after Stage Four and
emits an `Error`-severity diagnostic if violated. A failing
invariant is a compiler bug — it's always an error, never a
warning, and the plan is not returned.

## Scrutiny — corner cases, inconsistencies, resolutions

Per SRD-0002 R27, we walk reducto's rule set and record what we
found. Each item identifies a tension, our resolution, and the
rule(s) it affects.

### S.1 — LINEAR same-group-scope check (upstream gap)

Reducto's markdown states the LINEAR edge "applies only when X
and Y share the same trial scope (same configuration group)." The
upstream Java implementation
(`Rule2_DependencyEdges.applyLinear`) currently applies LINEAR
unconditionally for every trial. Reducto notes this explicitly as
a known gap.

**Resolution.** Our implementation enforces the same-group-scope
check. Use
`BindingStateComputer::same_group_for_elements(X, Y, T_i)` (the
method upstream already uses for `EXCLUSIVE` in Rule 2) to decide
whether the LINEAR edge applies at `T_i`. When X and Y are in
different groups, no edge is added; their lifecycles are
independent.

**Rule affected.** Rule 2 — LINEAR.

### S.2 — Propagation of LINEAR through binding levels

Reducto's binding-state propagation rule lists only SHARED and
EXCLUSIVE as propagating edges; DEDICATED and LIFELINE explicitly
do not propagate. LINEAR is silent.

**Resolution.** Treat LINEAR as *propagating* (same as SHARED /
EXCLUSIVE), matching the Java `BindingStateComputer`'s current
behaviour. Rationale: a LINEAR edge still implies the source
element has a lifetime dependency on the target element's
lifecycle within a shared trial scope — if the target is
parameterised at level L, the source's effective binding level
should also be at least L so they coalesce together at the
correct scope.

**Rule affected.** Stage One — binding-level propagation (§1.6).

### S.3 — Coalescing of first-trial deactivate

Reducto's markdown spells out that the *first* trial's deactivate
node is explicitly removed (not preserved) during coalescing,
and its edges are remapped to the surviving group deactivate.
Without this, loops that start at `i=1` (to preserve the first
trial's activate as the group activate) silently skip the
first-trial deactivate, producing orphaned edges.

**Resolution.** The coalescing loop must explicitly handle `i=0`
for deactivation, removing and remapping edges. The `i=0` case
for activation is the one that's kept; the `i=0` case for
deactivation is the one that's removed.

**Rule affected.** Rule 3.

### S.4 — Rule 4 trial-element reroute vs non-trial exclusion

Rerouting a direct cross-trial serialisation edge through
`notify_trial_end → notify_trial_start` works only for trial
elements — the notify wiring direction for non-trial elements is
the opposite, and the reroute would allow two non-trial instances
to be simultaneously active. Upstream calls this out clearly but
the Java implementation conditions the reroute on the element
being a trial element; correct.

**Resolution.** Our implementation mirrors upstream: condition
the reroute on `element ∈ trial_elements`. Non-trial serialisation
edges stay as direct `deactivate → activate` edges. This is the
intended behaviour; no fix needed, but the condition is brittle
and we add a validator that confirms non-trial EXCLUSIVE
dependencies still have direct serialisation edges after Rule 4
runs.

**Rule affected.** Rule 4.

### S.5 — Rule 6 group vs global concurrency interaction

Reducto treats `max_concurrency` and `max_group_concurrency` as
independent metadata on activate nodes. When both are set, the
executor enforces *both* — the tighter one effectively wins. But
the compiler doesn't warn if `max_concurrency > max_group_concurrency
× group_count`, which makes the global cap unreachable.

**Resolution.** Emit W004 at compile time when this condition
holds. The plan still compiles; the user sees the warning.

**Rule affected.** Rule 6.

### S.6 — Rule 8 complexity on large graphs

Transitive reduction is `O(V × (V + E))`. For plans with
thousands of steps (large cartesian products with many elements)
this becomes visible.

**Resolution.** Accept the cost for v1 — reducto's own doc reports
no issues on upstream workloads. If it becomes a bottleneck, we
replace the naive algorithm with Aho's `O(V × E)` transitive
reduction variant. Record as a potential future optimisation;
not a correctness concern.

**Rule affected.** Rule 8.

### S.7 — Lifeline cluster root detection

A LIFELINE cluster of more than two elements requires locating
the root — the unique element that nothing in the cluster has a
LIFELINE dependency on. Reducto assumes this root exists and is
unique. Corner case: what if multiple disjoint LIFELINE subgraphs
share members through other relationship types, creating multiple
apparent roots?

**Resolution.** Cluster membership is defined strictly by
LIFELINE edges — other relationship types do not contribute to
the cluster. The root is the element with no *outgoing* LIFELINE
edge (no LIFELINE edge targeting another cluster member from
this node) within its transitively-connected LIFELINE component.
If a component has more than one such element (cycle in LIFELINE
edges, impossible since it's a DAG), that's a plan-level error
reported before Rule 2 runs. If a component has zero such
elements, the LIFELINE dependencies form a cycle — also an
error.

**Rule affected.** Rule 2 — LIFELINE, and Rule 3 (which coalesces
cluster members).

### S.8 — DEDICATED chain binding-level resolution

When `A → B → C` is all DEDICATED and only the *middle* element
has a varying axis, reducto specifies that the effective binding
level for DEDICATED targets is resolved by walking the
DEDICATED ownership chain and taking the **maximum** binding
level along the path. Without this, the middle element's axis
would not propagate down to the root.

**Resolution.** `resolve_effective_binding_level_dedicated(E)`
walks upstream along DEDICATED edges transitively and returns the
max. Applied before Rule 3 coalescing decisions. Matches
upstream's `resolveEffectiveBindingLevel`.

**Rule affected.** Rule 3.

### S.9 — Sentinel step representation

Upstream reducto uses `ReductoNodeType.START` and `.END` during
Stage Three, then linearises them to `CheckpointState` in Stage
Four with metadata `type=start` / `type=end`. Reusing
`Checkpoint` here is cheap but semantically odd — start/end are
control-flow sentinels, not state snapshots.

**Resolution.** Same as upstream: map `Start`/`End` to
`AtomicStep::Checkpoint` with reserved `checkpoint_id`s of
`start` and `end`. Executors treat them as no-ops — they exist
to root the DAG. Confirming this assignment here pins it; if it
turns out to cause confusion in executor code, we introduce a
dedicated `AtomicStep::Sentinel` variant in a later amendment.

**Rule affected.** Stage Four linearisation / SRD-0009 mapping
table.

### S.10 — Multiple trial elements across different coalescing groups

The trial-element identification algorithm selects leaves *within
a scope*. If the plan has trial elements at different effective
binding levels (e.g. one trial element at level 3, another at
level 5), both are leaves, but they're not contemporaneous —
the level-3 element spans multiple level-5 groups.

**Resolution.** A trial is bounded by the innermost active trial
element(s) — those that change per trial. The notify wrap (Rule 4)
wraps only the innermost trial element's per-trial activation/
deactivation. Coarser-level trial elements effectively span
multiple inner-level trials. The notify_trial_end for each
inner-level trial fires as usual; the coarser trial element's
deactivation depends on every inner notify_trial_end in its
group (per Rule 4's group-deactivation ordering). This is the
intended reading of reducto; we add a compile-time sanity check
that identifies the innermost-level trial elements explicitly and
treats coarser-level trial elements as "spanning non-trial
elements" from the notify perspective.

**Rule affected.** Trial-element identification, Rule 4.

### Unresolved during scrutiny

None at time of SRD draft. New items are appended as they arise.

## Decisions

- **D1.** Compilation runs the four stages in order:
  enumeration → seeding → structuring (8 rules) → linearisation.
  Each stage is a single pass; the suite is not iterated.
- **D2.** Axis rank assignment is depth-first across the ordered
  element stack per SRD-0008 D5. Mixed-radix decomposition
  bijects trial numbers to offset vectors. Trial codes are
  the hex rendering defined in §1.4.
- **D3.** Binding-level propagation follows SHARED, EXCLUSIVE,
  and LINEAR edges (S.2); DEDICATED and LIFELINE do not
  propagate.
- **D4.** The 8 rules are implemented as defined in §Rule 1
  through §Rule 8, with the LINEAR same-group-scope check
  resolved per S.1.
- **D5.** `ReductoGraph` is an internal working type of the
  `paramodel-compiler` crate; it is not part of the public API.
  Only `ExecutionPlan` (via SRD-0009) escapes the crate.
- **D6.** Stage Four maps `ReductoNode` variants to `AtomicStep`
  variants per the table in §Stage Four. `Start` / `End` land as
  `AtomicStep::Checkpoint` with reserved ids (S.9), revisitable
  in a later SRD if executor code finds it confusing.
- **D7.** The `ElementInstanceGraph` is built by the compiler
  after linearisation, using `element_deps` metadata attached to
  `Deploy` steps.
- **D8.** Lifecycle and coalescing-congruence invariants are
  validated after Stage Four. Violations are always errors and
  indicate compiler bugs.
- **D9.** Warnings W001–W006 are defined in §Warnings. More
  accumulate as scrutiny continues.
  `CompilerOptions.fail_on_warning_codes` escalates any warning
  to an error.
- **D10.** Trial-element identification runs before Rule 1 and
  follows the override-respecting leaf-selection algorithm in
  §Trial-element identification.
- **D11.** The 25-row relationship composition table is the
  authoritative reference for cross-row semantics. Notable
  interactions (DEDICATED→EXCLUSIVE non-trial carve-out,
  LIFELINE→LIFELINE cluster root) are preserved.
- **D12.** `Compiler` is a trait returning `Result<ExecutionPlan,
  CompilationError>`. Synchronous; caller offloads to a blocking
  pool in async contexts.
- **D13.** `compile_incremental(&plan, &previous)` recompiles
  only the affected subgraph when the new plan differs from the
  previous one by a localised change (added axis value, changed
  configuration, etc.). Details of the delta algorithm are
  deferred to a follow-up SRD if needed; v1 falls back to a full
  recompile when `compile_incremental` is called, with a warning.
- **D14.** The compiler exposes a `validate(&plan)` method that
  runs all diagnostics without producing an `ExecutionPlan` —
  useful for IDE-style early feedback.
