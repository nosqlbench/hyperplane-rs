<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0007 — Elements and Relationships

## Purpose

Define **Element** — the anchor type of the whole system. Everything
paramodel does is about parameterising, scheduling, deploying, and
observing elements. This SRD pulls together the shapes defined in
SRDs 0004, 0005, and 0006 (`Parameter`, `Value`, `Labels`, `Tags`,
`Plug`, `Socket`, `Wire`, `TrialId`, `ElementName`) into the `Element`
struct and defines the supporting types it needs: `Dependency`,
`RelationshipType`, `HealthCheckSpec`, `OperationalState`,
`ShutdownSemantics`, element configuration, exports, element type
descriptors, and the runtime trait.

## Scope

**In scope.**

- The `Element` struct — its declarative shape (fields serialised
  into plans and persistence).
- `ElementName` — the validated newtype used everywhere (referenced
  by earlier SRDs; pinned here).
- `Dependency { target: ElementName, relationship: RelationshipType }`
  — the edge type that forms the element DAG.
- `RelationshipType` — the five-variant enum (`Shared`, `Exclusive`,
  `Dedicated`, `Linear`, `Lifeline`) per SRD-0002 R1, with helper
  methods that reducto's Rule 2 uses.
- Configuration — the element's authored parameter bindings (fixed
  values the element declares for its own parameters), with support
  for token references. Axis bindings override them per trial; see
  the resolution chain in D21.
- Exports — named values an element publishes for downstream
  elements to consume.
- `HealthCheckSpec` — readiness timing.
- `OperationalState` — runtime lifecycle enum.
- `LiveStatusSummary` — a live-state query result.
- `ShutdownSemantics` — `Service` (stop signal) vs `Command`
  (self-terminating).
- Trial-element override field on `Element`.
- Concurrency fields on `Element` (`max_concurrency`,
  `max_group_concurrency`) per R26.
- `ElementTypeDescriptor` and `ElementTypeDescriptorRegistry` trait
  — how adopting systems register the element types they support.
- `ElementRuntime` trait — the async runtime behaviour surface
  (`materialize`, `dematerialize`, `status_check`, trial hooks,
  state observation). Implementations live in hyperplane; paramodel
  defines the trait.

**Out of scope.**

- Concrete element implementations (EC2 nodes, Docker services,
  diagnostic probes) — hyperplane-tier SRDs.
- The element *binding tree* (cascading parameter values down a
  dependency chain) — compilation pipeline SRD.
- Token-resolution rules (the language for `${self.ip}`,
  `${other.export_name}`, `${{db:dockerRegistry}}`) — test-plan
  and/or compilation SRD; this SRD only defines the shape of the
  `Token` reference.
- Materialization-outputs persistence format — persistence SRD.

## Background

Upstream reference:
- `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/elements/Element.java`
- `.../elements/RelationshipType.java`
- `.../elements/{ElementFactory,ElementPrototype,ElementProvider,ElementTypeDescriptor,ElementTypeDescriptorProvider,DependencyRequirement}.java`
- `.../elements/{TrialContext,TrialLifecycleParticipant,OperationalStateObservable}.java`
- `links/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/paramodel/DockerImageElement.java`
- `links/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/execution/HyperplaneRuntime.java`
- Reducto sections on element stacks, binding-state propagation,
  trial-element identification, and relationship-type composition.

Shape observations that drive the Rust design:

1. Upstream's `Element` is a single Java interface that bundles
   *both* declarative structure (name, parameters, dependencies,
   tags) and dynamic behaviour (`materialize`, `dematerialize`,
   `statusCheck`, trial lifecycle callbacks, state observation).
   In Rust these separate cleanly:
   - The **declarative** shape becomes a serde-able struct — this
     is what goes into plans, gets fingerprinted, and flows across
     the wire.
   - The **behavioural** shape becomes an async trait — this is
     what the runtime calls to actually provision and tear down
     concrete resources, and it's implemented in the hyperplane
     tier.
2. Upstream's `Element.traits()` returned an unstructured
   `Map<String, String>`. SRD-0005 replaced that tier with
   structured **plugs** and **sockets**, so the Rust `Element`
   carries `plugs: Vec<Plug>` and `sockets: Vec<Socket>` instead.
3. Upstream's `configuration()` returns `Map<String, Object>` of
   raw values; token interpolation happens elsewhere. The Rust
   design models configuration entries as a small enum that
   distinguishes literal values from token references, so the
   compiler can see at a glance what needs resolution.
4. The `ElementFactory` / `ElementPrototype` / `ElementProvider`
   trio in upstream is infrastructure for plugin-style element
   registration. V1 needs only `ElementTypeDescriptor` (type
   validation) and a registry trait; full factory/provider
   plumbing is deferred to hyperplane SRDs.
5. `BindingNode` / `ElementBindingTree` are compilation-time
   artifacts. They're out of scope here and will land in the
   compilation SRD (Phase 1 item 8).

## The three graphs at a glance

![Three graphs: (1) Element Graph, authored by the user in the TestPlan with Element nodes and RelationshipType edges; (2) Element Instance Graph, derived by the compiler Stage 1 via mixed-radix axis expansion and instance coalescing; (3) Atomic Step Graph, derived by the compiler Stage 2 emitting Deploy/Await/SaveOutput/Teardown/Barrier steps — what the executor runs.](diagrams/SRD-0007/three-graphs.png)

Invariants: the authored graph is user-editable; the instance graph
is derived and surfaces in UIs for inspection; the atomic graph is
what actually executes.

## Design

All of the following lives in the `paramodel-elements` crate. This
is the central algebra crate — after absorbing the former
`paramodel-core` — so elements, parameters, attributes, values,
trials, and the runtime trait all live together here.
Dependencies outside the crate: none (beyond the workspace
third-party crates).

### `ElementName`

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ElementName(String);

impl ElementName {
    pub fn new(s: impl Into<String>) -> Result<Self>;       // validates
    pub fn as_str(&self) -> &str;
}

impl std::fmt::Display for ElementName { /* ... */ }
```

Validation (at construction): non-empty, ≤ 64 bytes,
ASCII-identifier-safe (`[A-Za-z_][A-Za-z0-9_\-]*`). Same regex class
as `ParameterName` for consistency.

### The `Element` struct

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize,
         bon::Builder)]
pub struct Element {
    /// Unique identifier within a test plan.
    pub name: ElementName,

    // -- metadata tiers (SRD-0005) ----------------------------------

    /// Intrinsic facts about this element. Must contain at least
    /// `(name, <element_name>)` and `(type, <type_id>)`.
    pub labels: Labels,

    /// Extrinsic organisation (owner, priority, environment, etc.).
    pub tags: Tags,

    // -- compatibility surface (SRD-0005) ---------------------------

    /// Points where this element needs upstream connections.
    pub plugs: Vec<Plug>,

    /// Points where downstream elements can connect to this one.
    pub sockets: Vec<Socket>,

    // -- parameterisation (SRD-0004) --------------------------------

    /// Configurable input dimensions of this element.
    pub parameters: Vec<Parameter>,

    /// Typed output dimensions this element publishes after
    /// materialization (corresponds to `@result` annotations in
    /// hyperplane Dockerfiles).
    pub result_parameters: Vec<Parameter>,

    /// The element's authored parameter bindings (may reference
    /// tokens). Axis values override entries here for a given
    /// trial; see D21 for the full resolution chain.
    pub configuration: Configuration,

    /// Named values this element publishes to downstream elements.
    pub exports: Exports,

    // -- topology ----------------------------------------------------

    /// Typed dependency edges — the element DAG.
    pub dependencies: Vec<Dependency>,

    // -- lifecycle and scheduling -----------------------------------

    /// Readiness-check timing. `None` = ready immediately after
    /// starting.
    pub health_check: Option<HealthCheckSpec>,

    /// How this element terminates: explicit stop signal (Service)
    /// or natural completion (Command).
    pub shutdown_semantics: ShutdownSemantics,

    /// Explicit trial-element override. `None` = auto-detect via
    /// reducto's leaf-node heuristic.
    pub trial_element: Option<bool>,

    /// Max active instances of this element globally.
    /// `None` = unbounded.
    pub max_concurrency: Option<u32>,

    /// Max active instances within one coalesced group.
    /// `None` = inherit from `max_concurrency`.
    pub max_group_concurrency: Option<u32>,
}
```

Builder invariants (enforced at construction):

- `labels` contains a `type` entry whose value appears in the type
  descriptor registry (see below), or the build fails with
  `UnknownElementType`.
- Every `Parameter::name` in `parameters` is unique within the
  element. Same rule for `result_parameters`. A parameter name may
  appear in both lists (input that the element also echoes as an
  output — rare but legal).
- Every `Configuration` key references an existing parameter in
  `parameters`.
- Every `Dependency::target` is a valid `ElementName` (but need
  not exist in this element's context — dependency-resolution
  happens at plan-compile time).
- `plug.name` and `socket.name` are unique within the element's
  combined plug+socket namespace (SRD-0005 D13).
- Namespace uniqueness across labels, tags, plugs, sockets, and
  parameter names is enforced (SRD-0005 D5).
- `max_concurrency >= 1` when present. `max_group_concurrency` ≤
  `max_concurrency` when both are present.

### `Configuration` and `Exports`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Configuration(BTreeMap<ParameterName, ConfigEntry>);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigEntry {
    /// An immediate, typed value bound to the parameter.
    Literal { value: Value },

    /// A token expression resolved at plan-compile time.
    /// Spelling of the expression is defined in the test-plan /
    /// compilation SRDs; this SRD only models that the entry is a
    /// token rather than a literal.
    Token { expr: TokenExpr },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TokenExpr(String);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Exports(BTreeMap<ExportName, TokenExpr>);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ExportName(String);
```

`TokenExpr` is an opaque string at this layer — the compiler SRD
defines its grammar (`${self.ip}`, `${other_element.endpoint}`,
`${{db:dockerRegistry}}`, etc.) and the resolution algorithm.
`Element` only records the *shape*: "this configuration entry is a
token expression, not a literal." At the element level we don't
parse or evaluate tokens.

`Exports` maps user-defined export names (e.g. `service_addr`) to
token expressions (`${self.ip}:4567`) that downstream elements can
reference. Resolution happens at deploy time, when the upstream
element has materialized and its address is known.

### `RelationshipType` and `Dependency`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipType {
    /// Concurrent access allowed; default.
    Shared,
    /// Serialize dependents of the target (reducto: serialization edges).
    Exclusive,
    /// One target instance per dependent (reducto: coalesces with owner).
    Dedicated,
    /// Full-lifecycle ordering within same trial scope (target completes
    /// before dependent starts).
    Linear,
    /// Target's teardown subsumes the dependent's (dependent has no
    /// explicit teardown step).
    Lifeline,
}

impl RelationshipType {
    pub fn requires_serialization_barrier(&self) -> bool { *self == Self::Exclusive }
    pub fn requires_dedicated_instance   (&self) -> bool { *self == Self::Dedicated }
    pub fn implies_lifecycle_coupling    (&self) -> bool { *self == Self::Lifeline }
    pub fn is_linear                     (&self) -> bool { *self == Self::Linear }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Dependency {
    pub target:       ElementName,
    pub relationship: RelationshipType,
}

impl Dependency {
    pub fn shared   (target: ElementName) -> Self { Self { target, relationship: RelationshipType::Shared } }
    pub fn exclusive(target: ElementName) -> Self { Self { target, relationship: RelationshipType::Exclusive } }
    pub fn dedicated(target: ElementName) -> Self { Self { target, relationship: RelationshipType::Dedicated } }
    pub fn linear   (target: ElementName) -> Self { Self { target, relationship: RelationshipType::Linear } }
    pub fn lifeline (target: ElementName) -> Self { Self { target, relationship: RelationshipType::Lifeline } }
}
```

Relationship-type semantics, composition rules, and the 25-entry
cross-product table from reducto are preserved **verbatim** in the
compilation SRD (SRD-0002 R27). This SRD defines the enum and the
helper accessors the planner uses; the deeper semantic rules live
where they're applied.

### `HealthCheckSpec`

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HealthCheckSpec {
    pub timeout:        std::time::Duration,
    pub max_retries:    u32,
    pub retry_interval: std::time::Duration,
}
```

The host system owns the health-check mechanism (protocol, endpoint,
acceptance). Paramodel only needs the timing parameters so the
runtime can coordinate.

### `OperationalState` and `LiveStatusSummary`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalState {
    Inactive,      // not yet started
    Provisioning,  // infrastructure being allocated
    Starting,      // process starting up
    HealthCheck,   // verifying readiness
    Ready,         // available for use
    Running,       // actively serving a trial
    Stopping,      // graceful shutdown in progress
    Stopped,       // stopped normally, resources still allocated
    Failed,        // error state — cannot operate
    Terminated,    // fully torn down, all resources released
    Unknown,       // status cannot be determined
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LiveStatusSummary {
    pub state:   OperationalState,
    pub summary: String,  // one-line human-readable evidence
}

impl LiveStatusSummary {
    pub fn unknown (summary: impl Into<String>) -> Self;
    pub fn inactive()                            -> Self;
}
```

Normal progression: `Inactive → Provisioning → Starting →
HealthCheck → Ready → Running → Stopping → Stopped → Terminated`.
`Failed` and `Unknown` are non-sequential and can be entered from
any state.

### `ShutdownSemantics`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownSemantics {
    /// Long-running; requires explicit stop signal. Default.
    Service,
    /// Self-terminating; awaited rather than stopped.
    Command,
}

impl Default for ShutdownSemantics {
    fn default() -> Self { Self::Service }
}
```

Reducto's Rule 1 (lifecycle expansion) emits `Deploy` + `Teardown`
steps for `Service` elements and `Deploy` + `Await` steps for
`Command` trial elements.

### Element type descriptors

Adopting systems register the element types they support. Paramodel
needs only a minimal descriptor + registry.

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize,
         bon::Builder)]
pub struct ElementTypeDescriptor {
    /// Canonical type id (e.g. "node", "service", "command").
    pub type_id: TypeId,

    /// Labels that an element of this type is required to carry.
    pub required_labels: BTreeSet<LabelKey>,

    /// Labels that are forbidden on elements of this type, with a
    /// message explaining why.
    pub forbidden_labels: BTreeMap<LabelKey, String>,

    /// Labels that trigger a warning (not an error) when present.
    pub label_warnings: BTreeMap<LabelKey, String>,

    /// True if this type represents infrastructure (a node).
    pub provides_infrastructure: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TypeId(String);

pub trait ElementTypeDescriptorRegistry: Send + Sync + 'static {
    fn descriptors(&self) -> Vec<ElementTypeDescriptor>;
    fn type_aliases(&self) -> BTreeMap<TypeId, TypeId> { BTreeMap::new() }

    fn valid_type_ids(&self) -> BTreeSet<TypeId> {
        self.descriptors().into_iter().map(|d| d.type_id).collect()
    }

    fn descriptor(&self, type_id: &TypeId) -> Option<ElementTypeDescriptor> {
        self.descriptors().into_iter().find(|d| &d.type_id == type_id)
    }

    fn has_infrastructure_type(&self) -> bool {
        self.descriptors().iter().any(|d| d.provides_infrastructure)
    }
}
```

The registry is the one place in the elements layer where
`dyn Trait` appears (it's a host-provided service, not part of the
element algebra itself). A permissive default registry
(`OpenRegistry`) accepts any type id — used in tests.

### `ElementRuntime` — the behavioural surface

The runtime trait is what a concrete element implementation
(`Ec2NodeElement`, `DockerServiceElement`, `DiagnosticElement`)
provides. It's invoked by the executor during plan execution.

```rust
#[async_trait::async_trait]    // until 2024 async-trait-in-dyn is stable
pub trait ElementRuntime: Send + Sync + 'static {
    /// Provision the element's concrete resources.
    ///
    /// Called during plan execution when a `Deploy` step fires
    /// against this element. The `resolved` map contains fully
    /// interpolated configuration (tokens expanded). Returns the
    /// element's materialization outputs (endpoint URLs, instance
    /// ids, allocated ports, etc.) — these are typed values keyed
    /// by `result_parameters` names.
    async fn materialize(
        &self,
        resolved: &ResolvedConfiguration,
    ) -> Result<MaterializationOutputs>;

    /// Release the element's provisioned resources. Idempotent.
    async fn dematerialize(&self) -> Result<()>;

    /// Report the element's current operational state.
    async fn status_check(&self) -> LiveStatusSummary;

    /// Trial lifecycle hooks.
    async fn on_trial_starting(&self, ctx: &TrialContext) -> Result<()> { Ok(()) }
    async fn on_trial_ending  (&self, ctx: &TrialContext) -> Result<()> { Ok(()) }

    /// Register a listener for state transitions. Implementations
    /// must deliver a synthetic initial transition from `Unknown`
    /// to the current state so registration acts as catch-up.
    fn observe_state(&self, listener: StateTransitionListener) -> StateObservation;
}

#[derive(Debug, Clone)]
pub struct TrialContext {
    pub trial_id:    TrialId,
    pub trial:       std::sync::Arc<Trial>,  // borrowed for the hook lifetime
    pub timestamp:   jiff::Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ResolvedConfiguration(BTreeMap<ParameterName, Value>);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct MaterializationOutputs(BTreeMap<ParameterName, Value>);

pub type StateTransitionListener = Box<dyn Fn(StateTransition) + Send + Sync + 'static>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateTransition {
    pub from:      OperationalState,
    pub to:        OperationalState,
    pub summary:   String,
    pub timestamp: jiff::Timestamp,
}

pub trait StateObservation: Send + Sync + 'static {
    fn cancel(&self);
}
```

The crate is tokio-based per SRD-0003 R17. `ElementRuntime`
implementations live in hyperplane-side crates. Paramodel provides
a mock implementation in `paramodel-mock` for TCK tests.

### Joining an `Element` to its runtime

An `Element` by itself is a declaration. To actually run it, the
executor pairs the declaration with a concrete `ElementRuntime`
selected by the element's `type` label via a registry:

```rust
pub trait ElementRuntimeRegistry: Send + Sync + 'static {
    /// Produce a runtime for this element. Host systems typically
    /// dispatch on `element.labels.get(&label::r#type())` to pick
    /// which concrete `ElementRuntime` implementation to construct.
    fn runtime_for(
        &self,
        element: &Element,
    ) -> Result<Arc<dyn ElementRuntime>>;
}
```

The registry (and the concrete runtimes) live in hyperplane. This
SRD defines the trait shape only.

### The three graphs: where an Element lives

Running an element involves three successive graphs — one authored
by the user, two derived by the compiler. These names (pinned in
SRD-0002 §6.4) are used consistently across the rest of the SRDs.

**Element Graph — authored.** Nodes are `Element` structs defined
by this SRD. Edges are each element's `Dependency` records
(`target` + `RelationshipType`). This is what users write. It
captures:

- The elements themselves (name, parameters, labels, tags, plugs,
  sockets, configuration, exports, concurrency caps, shutdown
  semantics, health-check timing, trial-element override).
- The **user-prescribed relationships** between them — every edge
  is explicitly authored with a `RelationshipType` that the
  compiler will honour.

Axes live in the test plan (SRD Phase 1 item 6) and attach to this
graph: an axis varies one of an element's parameters and, by
taint propagation, affects every downstream element in the graph.

**Element Instance Graph — derived (compiler phase 1).** Nodes are
*element instances* — one per unique bound parameter set after
mixed-radix trial enumeration and binding-state computation. Edges
are instance-to-instance dependencies obtained by projecting the
authored `RelationshipType`s onto the instance layer, applying
group coalescing, `Dedicated` propagation, and `Lifeline` cluster
collapse. This layer answers "how many instances of each element
exist, how is each one bound, and which instances connect to which?"
— the element-level cardinality and nesting picture, before any
control-flow steps are emitted.

The compiler SRD will describe how this graph is computed from
the authored Element Graph + the test plan's axes. In reducto
terms: Stage One (mixed-radix enumeration + binding state) and
Rule 3 (group coalescing) are what produce it. It surfaces through
`ExecutionPlan::element_instance_graph()` so UI and tooling can
visualise element-level topology without descending into atomic
steps.

**Execution Graph — derived (compiler phase 2).** Nodes are
`AtomicStep`s. Edges are step-to-step ordering constraints. This is
what the executor actually runs. It is derived from the Element
Instance Graph by reducto's remaining rules (lifecycle expansion,
dependency edge materialisation, trial notifications, health-check
gates, concurrency annotations, start/end sentinels, transitive
reduction).

### What this SRD contributes to each graph

- **Element Graph** — fully defined here. `Element` is the node
  type, `Dependency` + `RelationshipType` is the edge record, and
  every field on `Element` feeds into how the next graph is
  derived.
- **Element Instance Graph** — this SRD supplies the inputs
  (parameters that get bound, dependency edges, `RelationshipType`,
  `shutdown_semantics`, `trial_element` override) but does not
  specify the derivation. The compiler SRD owns that.
- **Execution Graph** — `Element.max_concurrency` /
  `max_group_concurrency` flow through to `Deploy`-step metadata
  per R26; `Element.shutdown_semantics` decides Teardown vs Await;
  `Element.health_check` gates readiness nodes; everything else is
  in the compiler SRD.

### Other SRD boundaries

- The executor SRD (Phase 1 item 9) runs the Execution Graph by
  pairing each step's target element with a concrete `ElementRuntime`
  via the `ElementRuntimeRegistry`.
- Result parameters show up in a trial's `TrialResult.metrics`
  (SRD-0006) — each metric is a `Value` under a
  `(element, result_parameter)` coordinate.

## Decisions

- **D1.** `Element` is a concrete struct, not a trait. Its fields
  are the declarative shape that flows through plans, compilation,
  and persistence. Behaviour (materialize, teardown, observe) is
  split into the separate `ElementRuntime` trait.
- **D2.** `ElementName` is a validated newtype over `String`; the
  canonical name type for dependencies, wires, trial coordinates,
  and step targets.
- **D3.** `Dependency { target: ElementName, relationship:
  RelationshipType }` is the single edge record between elements.
  No other variants.
- **D4.** `RelationshipType` is a flat Rust enum with exactly five
  variants: `Shared`, `Exclusive`, `Dedicated`, `Linear`,
  `Lifeline` (pinning SRD-0002 R1). Helper methods match upstream's
  behaviour queries.
- **D5.** `Configuration` entries are `ConfigEntry::Literal { value }`
  or `ConfigEntry::Token { expr }`. Literals carry typed `Value`s;
  token resolution is deferred to the compiler SRD.
- **D6.** `Exports` maps user-defined `ExportName`s to `TokenExpr`s.
- **D7.** Labels, tags, plugs, and sockets per SRD-0005. The
  namespace-uniqueness rule applies across all four plus the
  element's parameter names.
- **D8.** `labels` must contain a `type` entry whose value is a
  registered `TypeId`. Element construction fails otherwise. The
  registry is pluggable via `ElementTypeDescriptorRegistry`.
- **D9.** `HealthCheckSpec` is `{ timeout, max_retries,
  retry_interval }`. Omitted → ready immediately after starting.
- **D10.** `OperationalState` is a fixed 11-variant enum matching
  upstream. `LiveStatusSummary = { state, summary }`.
- **D11.** `ShutdownSemantics { Service, Command }` drives reducto
  Rule 1 (Teardown vs Await). Default is `Service`.
- **D12.** `Element.trial_element: Option<bool>` supports reducto's
  explicit trial-element override (force on / force off / auto).
- **D13.** `Element.max_concurrency` and `max_group_concurrency`
  are declarative concurrency caps per SRD-0002 R26. The compiler
  emits them as metadata on `Deploy` steps; the executor enforces
  them dynamically. No structural concurrency edges in the
  execution graph.
- **D14.** `ElementRuntime` is an async trait (tokio), owned by
  paramodel-elements, implemented in hyperplane. Methods:
  `materialize`, `dematerialize`, `status_check`,
  `on_trial_starting`, `on_trial_ending`, `observe_state`.
- **D15.** `ElementTypeDescriptor` carries the minimal metadata
  needed for construction validation (required labels, forbidden
  labels, warning labels, infrastructure flag). `ElementTypeDescriptorRegistry`
  is the host-provided service that dispatches by `TypeId`.
- **D16.** `ElementRuntimeRegistry` pairs an `Element` declaration
  with a concrete `ElementRuntime` at execution time. Selection is
  host-defined; typical dispatch is on the element's `type` label.
- **D17.** `BindingNode` / `ElementBindingTree` from upstream are
  out of scope for this SRD and deferred to the compilation SRD.
- **D18.** `ElementFactory` / `ElementPrototype` / `ElementProvider`
  from upstream are out of scope for this SRD and deferred to
  hyperplane-tier SRDs where actual type registration happens.
- **D19.** `ElementRuntime` and `ElementRuntimeRegistry` use the
  `async_trait` crate attribute for now. Native async-fn-in-trait
  with `dyn` dispatch is not yet stable for our needs on nightly;
  we revisit when it is. No other dyn-trait is introduced to the
  element layer.
- **D20.** The three canonical graphs are named **Element Graph**
  (authored), **Element Instance Graph** (derived, reducto
  phase 1), and **Execution Graph** (derived, reducto phase 2).
  All subsequent SRDs use these names; SRD-0002 §6.4 is the
  pinning reference.
- **D21.** `Configuration` is not a "defaults" map. It is the
  element's *authored parameter bindings* — fixed values the
  element declares for its parameters. The resolution precedence
  when the planner looks up a parameter value for a trial is:
  1. **Axis binding** — if an axis varies this parameter for this
     trial, its value wins.
  2. **Element configuration** — otherwise, the value in the
     element's `Configuration` map (literal or resolved token) is
     used.
  3. **Parameter default** — if the element carries no
     configuration entry for this parameter and the parameter
     itself has a `default` (SRD-0004 D4), that default is used.
  4. **Error** — if no binding was produced by 1–3, binding fails
     with `Error::UnboundParameter { element, parameter }`.
  "Default" in this chain refers only to the parameter-level
  fallback (step 3); element configuration entries are bindings,
  not defaults.
- **D22.** `TokenExpr` is an opaque string at the element layer.
  Grammar is deferred — and will be broader than pure
  interpolation (full *expression* semantics over plan state,
  element exports, and runtime variables). The test-plan and
  compiler SRDs define it together.

