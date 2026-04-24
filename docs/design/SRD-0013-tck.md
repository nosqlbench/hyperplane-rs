<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0013 — Technology Compatibility Kit (TCK)

## Purpose

Define the conformance test suite that validates an implementation
of any paramodel trait. The TCK gives adopting systems a single
button for "does my implementation satisfy the contract?" and
gives paramodel itself a regression harness for the default
implementations.

Per SRD-0003 R14 and the Phase 1 roadmap in SRD-0002 §8, the TCK:

- Lives in its own crate (`paramodel-tck`).
- Uses `proptest` for algebraic-law coverage.
- Is invocable from a downstream crate's test suite with one
  macro call.
- Covers every trait defined in SRDs 0004–0012 that has behavioural
  expectations (not every SRD surfaces testable behaviour; enums
  and pure data types are compile-time validated).
- Supports golden-file tests for compilation output, regeneratable
  via a `--bless` flag.

## Scope

**In scope.**

- Crate layout of `paramodel-tck`.
- The `ImplementationProvider` trait family: per-trait factories
  the TCK uses to obtain fresh instances under test.
- Algebraic property tests (Boolean algebra on constraints, domain
  membership, fingerprint determinism, plan fingerprint stability,
  result-filter canonicalisation, execution-graph invariants).
- Per-trait conformance suites for: `Compiler`, `Executor`,
  `Orchestrator`, `Scheduler`, `ResourceManager`, `ArtifactCollector`,
  `ExecutionStateManager`, `JournalWriter`, and the six persistence
  traits.
- Canonical test-plan fixtures covering each `RelationshipType` and
  each `AtomicStep` variant.
- Golden-file support for compiled `ExecutionGraph`s.
- The `paramodel-mock` sibling crate — minimal in-memory
  implementations of every runtime and persistence trait, used as
  default fixtures by the TCK and as a reference for implementers.
- The `run_all!` macro that wires everything up for a downstream
  crate.

**Out of scope.**

- Performance benchmarks — separate SRD if ever needed.
- Integration tests against real hyperplane infrastructure —
  hyperplane-tier SRDs.
- Security / access-control conformance — deferred until auth is
  in scope (SRD-0002 Phase 2 item 24).

## Background

Upstream reference:
- `links/paramodel/paramodel-tck/src/main/java/io/nosqlbench/paramodel/tck/**`
  — JUnit 5 extensions plus an `ImplementationProvider` interface
  with a factory method per type.
- `links/paramodel/paramodel-mock/src/main/java/...` — the
  reference mock implementations.

Observations:

1. **Upstream has one monolithic `ImplementationProvider`.** One
   interface exposes factories for every type: `createParameter`,
   `createDomain`, `createSequence`, `createTestPlan`,
   `createExecutionPlan`, etc. Convenient but heavy. In Rust we
   split it into per-crate sub-traits (`ParameterProvider`,
   `PlanProvider`, `ExecutorProvider`) so a TCK run doesn't force
   an implementer to supply a whole universe of types just to test
   one crate.
2. **Upstream TCK is JUnit 5 extensions.** In Rust the equivalent
   is functions annotated `#[test]` that take an implementation
   handle as input. We expose conformance checks as plain generic
   functions, plus a `#[macro] run_all!` that declares them as
   `#[test]`s in the caller's crate.
3. **Upstream uses JUnit's parameterised-test features for
   algebraic laws.** Rust equivalent is `proptest` (per SRD-0003
   R14). Laws are property tests; happy-path scenarios are plain
   tests.
4. **Mocks live in a separate crate.** Upstream keeps
   `paramodel-mock` separate from `paramodel-tck` so non-test
   code (examples, docs) can depend on mocks without pulling in
   the test harness. We preserve that split.

## TCK structure at a glance

```
  paramodel-tck  (opt-in crate; hidden behind a cargo feature)
  │
  ├── ImplementationProvider family
  │   ParameterProvider, ElementProvider, TrialProvider,
  │   PlanProvider, ExecutorProvider, StorageProvider
  │   (one per domain; adopters compose what they need)
  │
  ├── Conformance cases
  │   │   parameterised over the ImplementationProvider
  │   │
  │   ├── trait-level cases     (per trait: round-trip, errors, edge)
  │   ├── invariant cases       (every INV-* code has a case)
  │   └── scenario cases        (end-to-end multi-trait exercises)
  │
  └── Harness
      wires an adopter's provider into the case battery
      produces a pass/fail report per case per invariant

  paramodel-mock (opt-in sibling)
  │
  └── in-memory reference implementations of every trait
      (not a conformance target; a baseline fixture for TCK + docs)

  adopters: hyperplane-store-sqlite, future backends
  each adopter runs the TCK against their impl via their
  own ImplementationProvider
```

## Design

### Crate split

Two crates, both new:

- **`paramodel-mock`** — minimal in-memory implementations of
  every runtime trait (`Orchestrator`, `Scheduler`,
  `ResourceManager`, `ArtifactCollector`, `ExecutionStateManager`,
  `JournalWriter`, plus every persistence store). Pure-Rust,
  tokio-async, no external services, no disk. Suitable for:
  - TCK fixtures when a downstream crate doesn't supply a richer
    `ImplementationProvider`.
  - Documentation and example code.
  - Downstream test suites that want one-line in-memory
    paramodel instances.
- **`paramodel-tck`** — conformance tests. Depends on every
  paramodel trait crate plus `paramodel-mock`.

Both crates are opt-in: normal build graphs never see them.

### `ImplementationProvider` family

Instead of one big trait, one per domain:

```rust
pub trait ParameterProvider: Send + Sync + 'static {
    fn integer_range(&self, name: &ParameterName, min: i64, max: i64) -> IntegerParameter;
    fn integer_of   (&self, name: &ParameterName, values: BTreeSet<i64>) -> IntegerParameter;
    fn double_range (&self, name: &ParameterName, min: f64, max: f64) -> DoubleParameter;
    fn boolean      (&self, name: &ParameterName) -> BooleanParameter;
    fn string_any   (&self, name: &ParameterName) -> StringParameter;
    fn string_regex (&self, name: &ParameterName, pattern: &str) -> StringParameter;
    fn selection    (&self, name: &ParameterName, values: IndexSet<SelectionItem>) -> SelectionParameter;
}

pub trait ElementProvider: Send + Sync + 'static {
    fn element(&self, name: &ElementName, kind: &TypeId) -> Element;
    fn with_parameter(&self, element: Element, parameter: Parameter) -> Element;
    fn with_dependency(&self, element: Element, target: &ElementName, relationship: RelationshipType) -> Element;
    fn with_plug     (&self, element: Element, plug: Plug) -> Element;
    fn with_socket   (&self, element: Element, socket: Socket) -> Element;
}

pub trait PlanProvider: Send + Sync + 'static {
    fn test_plan(&self, name: &PlanName, elements: Vec<Element>, axes: Vec<Axis>) -> TestPlan;
    fn axis     (&self, name: &AxisName, target: ElementParameterRef, values: Vec<Value>) -> Axis;
}

pub trait CompilerProvider: Send + Sync + 'static {
    type Compiler: Compiler;
    fn compiler(&self, options: CompilerOptions) -> Self::Compiler;
}

pub trait ExecutorProvider: Send + Sync + 'static {
    type Executor: Executor;
    fn executor(
        &self,
        config:                ExecutorConfig,
        orchestrator:          Arc<dyn Orchestrator>,
        scheduler:             Arc<dyn Scheduler>,
        resource_manager:      Arc<dyn ResourceManager>,
        artifact_collector:    Arc<dyn ArtifactCollector>,
        state_manager:         Arc<dyn ExecutionStateManager>,
        journal_writer:        Arc<dyn JournalWriter>,
    ) -> Self::Executor;
}

// One trait per persistence store.
pub trait ArtifactStoreProvider: Send + Sync + 'static {
    type Store: ArtifactStore;
    fn fresh(&self) -> Self::Store;
}
pub trait CheckpointStoreProvider: Send + Sync + 'static {
    type Store: CheckpointStore;
    fn fresh(&self) -> Self::Store;
}
// … and so on for ExecutionRepository, JournalStore, MetadataStore, ResultStore.
```

A provider returns a fresh instance on each call so tests never
share state. Downstream crates implement only the providers for
the traits they supply.

### `paramodel-mock` reference implementations

Every runtime and persistence trait has a `Mock*` struct in
`paramodel-mock`:

```rust
pub struct MockOrchestrator       { /* in-memory instance table */ }
pub struct MockScheduler          { /* FIFO ready-queue */ }
pub struct MockResourceManager    { /* infinite capacity, records allocations */ }
pub struct MockArtifactCollector  { /* stores inline content in a Vec */ }
pub struct MockExecutionStateManager { /* in-memory step/trial state */ }
pub struct MockJournalWriter      { /* append-only Vec<JournalEvent> */ }
pub struct MockArtifactStore      { /* HashMap<ArtifactId, (record, content)> */ }
pub struct MockCheckpointStore    { /* HashMap<CheckpointId, Checkpoint> */ }
pub struct MockExecutionRepository{ /* ... */ }
pub struct MockJournalStore       { /* append-only Vec + tokio::sync::broadcast for subscribe */ }
pub struct MockMetadataStore      { /* flat HashMaps */ }
pub struct MockResultStore        { /* Vec + naive filter evaluator */ }
```

These satisfy the conformance tests by construction — running the
TCK against the mocks is the TCK's own integration test.

Mocks are deliberately simple. They exist to validate the traits,
to back documentation examples, and to let hyperplane write
integration tests that don't need a real SQLite or real runtime.
They are not performance-optimised.

### Conformance test functions

Each trait has a set of functions named `tck::<area>::<behavior>`:

```rust
// Example: constraint algebra
pub async fn tck_constraint_algebra_laws<P: ParameterProvider>(
    provider: &P,
) { /* proptest-driven: associativity, commutativity, ... */ }

pub async fn tck_domain_membership<P: ParameterProvider>(
    provider: &P,
) { /* generate N values, each must satisfy domain.contains */ }

pub async fn tck_value_fingerprint_determinism<P: ParameterProvider>(
    provider: &P,
) { /* same (kind, parameter, native value) → same fingerprint */ }

pub async fn tck_executor_happy_path<EP, CP>(
    executor_provider: &EP,
    compiler_provider: &CP,
) -> tck::Result<()>
where
    EP: ExecutorProvider,
    CP: CompilerProvider,
{ /* compile + execute a canonical plan; assert trial results */ }

// Persistence: round-trip, idempotency, pagination, filter.
pub async fn tck_result_store_round_trip<P: ResultStoreProvider>(
    provider: &P,
) { /* save then get → same record */ }

pub async fn tck_result_store_filter_semantics<P: ResultStoreProvider>(
    provider: &P,
) { /* fixed dataset + every ResultFilter variant against a reference evaluator */ }
```

Functions that drive property tests take a small `proptest::Config`
override so failures surface with the minimal reproducible counter-
example.

### `run_all!` macro

A downstream crate implements the providers it supports and
invokes:

```rust
paramodel_tck::run_all!(
    parameters  = MyParameterProvider::new(),
    plan        = MyPlanProvider::new(),
    compiler    = MyCompilerProvider::new(),
    result_store = MyResultStoreProvider::new(),
    // any provider that's unspecified falls back to paramodel-mock
);
```

The macro expands to a block of `#[tokio::test]`-annotated
functions, one per conformance check, wired to the supplied
providers. Omitting a provider installs the mock equivalent —
useful when an implementer only wants to validate the one trait
they're replacing.

An implementer can also invoke individual areas:

```rust
paramodel_tck::run_parameters!(MyParameterProvider::new());
paramodel_tck::run_persistence!(result_store = MyResultStoreProvider::new());
```

### Algebraic property tests (proptest)

Every algebraic law defined in earlier SRDs has a matching
property test:

- **`Constraint` Boolean algebra** (SRD-0004) — associativity,
  commutativity, distributivity, De Morgan, double negation,
  idempotence, annihilation, identity. Both `IntConstraint` and
  `DoubleConstraint` sample from random expressions and test
  values.
- **Canonical form of constraints.** Two semantically-equal
  constraint trees produce the same fingerprint after
  canonicalisation (flatten, sort, fold identities, dedupe,
  normalise double-negation).
- **`Domain` membership** — `sample(&rng)` output passes
  `contains`.
- **`Value` fingerprints** — `Value { value: X, parameter: P }`
  fingerprint depends only on `(kind, X, P.name())`, not on
  timestamp or generator metadata.
- **`TestPlan` fingerprint stability** — reorder axes with
  `with_axes_reordered` to the original order → original
  fingerprint. Add then remove an element → original fingerprint.
- **`ExecutionGraph` invariants** (SRD-0010 §Invariants) — eight
  invariants, each its own property-style test over a set of
  canonical plans plus randomly generated variants.
- **`ResultFilter` canonicalisation** — `filter.and(filter) ==
  filter`, `filter.and(Any) == filter`, `filter.or(Any) == Any`,
  etc.

### Canonical fixtures

`paramodel-tck::fixtures` exposes a curated plan set:

| Fixture                                 | What it covers |
|-----------------------------------------|----------------|
| `single_element_no_axes`                | Trivial plan — one element, zero axes → one trial. |
| `two_axis_grid`                          | Two int-range axes, cartesian product = 12 trials. |
| `three_element_chain`                   | A → B → C with all 5 `RelationshipType`s, one at a time (5 sub-fixtures). |
| `fan_out_service_clients`               | One service, many client instances with `max_concurrency > 1`. |
| `trial_element_override`                | Mix of auto-detected and explicit `trial_element: Some(true/false)`. |
| `dedicated_chain_varying_middle`        | A / B / C all DEDICATED, middle B parameterised (reducto S.8). |
| `lifeline_cluster_3`                    | Three-element LIFELINE cluster (host → container → process). |
| `reset_each_server`                     | Server that forces redeploy per trial (maps to RESET_EACH pattern). |
| `edge_first_ordering_3d`                | 3-axis plan for exercising EdgeFirst corner/edge/interior phases. |
| `partial_run_resume`                    | Plan that triggers mid-run checkpoint + resume path. |
| `all_25_chains`                          | Generator over all 25 relationship-type transitive-chain combinations. |

Each fixture is a factory function `fn <name>(&impl PlanProvider)
-> TestPlan` so the TCK can assemble the plan with whatever
implementation it's testing.

### Golden-file tests for compilation

For each canonical fixture, the compiler TCK compiles the plan
and compares its `ExecutionGraph` step list against a stored
reference in `paramodel-tck/fixtures/golden/<fixture>.json`.

Invocation:

```
cargo test --test tck_compiler              # compares
cargo test --test tck_compiler -- --bless   # regenerates golden files
```

Golden files are JSON serialisations of `ExecutionGraph` with
step ids, dependencies, reasons, and variant-specific fields.
Implementations that pass the invariant tests but whose compiler
emits different golden output signal a legitimate divergence —
the reviewer decides whether to update the golden or reject the
change.

Golden comparisons are deterministic because step ids follow
reducto's naming convention (SRD-0009 D3) and the execution
graph's canonical serialisation is stable.

### Reducto-rule unit tests

One test per rule (Rules 1–8 from SRD-0010):

- **Rule 1** — given N trials × E elements, the lifecycle
  expansion produces `N × E` activate/{deactivate,await}
  pairs with the correct variant for service vs command trial
  elements.
- **Rule 2** — for each `RelationshipType`, edges appear with
  the correct topology on a canonical pair-of-elements fixture.
  Plus: EXCLUSIVE serialisation, LINEAR same-group-scope (S.1),
  LIFELINE edge removal and remapping, LIFELINE cluster
  collapse.
- **Rule 3** — N trials with identical configuration coalesce to
  one activate + one deactivate; activation congruence,
  deactivation congruence (including first-trial removal, S.3),
  dependency congruence across all trials in a group.
- **Rule 4** — NotifyTrialStart/End insertion placement,
  coalesced-activate resolution, group-deactivation ordering,
  DEDICATED target per-trial wiring, exclusive-serialisation
  reroute (trial elements only, S.4), non-trial deactivation
  enforcement.
- **Rule 5** — readiness gates inserted between activate and
  dependents, not between activate and its own deactivate/await.
- **Rule 6** — `max_concurrency` and `max_group_concurrency`
  annotations on Deploy steps; W004 emission when global >
  group × group-count.
- **Rule 7** — exactly one `Start` and one `End` sentinel;
  acyclicity validated.
- **Rule 8** — transitive reduction doesn't remove edges that
  would change reachability.

Each rule test runs against the default compiler and against any
compiler supplied via `CompilerProvider`, so bespoke compilers
can verify they preserve rule semantics.

### Executor TCK coverage

- **Happy path** — canonical plan, mock runtime, assert every
  trial completes successfully.
- **Retry composition** — step failure triggers `RetryPolicy`;
  exhausted retries trigger plan-level `OnFailure`. Assert the
  exact sequence of journal events.
- **Intervention** — pause/resume under both `InterventionMode`s;
  stop(graceful=true) runs teardown; stop(graceful=false) skips
  it.
- **Checkpoint + resume** — run to mid-plan, checkpoint, stop;
  resume from checkpoint; assert the whole plan completes with
  correct per-trial results and no duplicate step executions on
  idempotent steps.
- **Concurrency limits** — an element with `max_concurrency: 2`
  has at most 2 `InProgress` Deploy steps at any point.
- **Barrier timeouts** — a barrier whose dependencies exceed
  `timeout` triggers the configured `TimeoutAction`.

### Persistence TCK coverage

For each store:

- **Round-trip** — save then get returns the original record
  (modulo fields the backend may legitimately normalise).
- **Idempotent saves** — double-save of the same logical key is a
  no-op; conflicting saves produce `PersistenceError::Conflict`.
- **NotFound** — get on unknown id returns
  `PersistenceError::NotFound`.
- **Pagination correctness** — paginate a large dataset; every
  item appears exactly once across pages; next_token termination
  is correct.
- **Stream correctness** — `stream(...)` yields the same set as
  `query(..., page = unbounded)`.

Store-specific:

- **`ResultStore::query` filter semantics** — exercise every
  `ResultFilter` variant against a fixed dataset; compare against
  a reference in-memory evaluator.
- **`ResultStore::aggregate`** — compare each aggregation kind
  (`Count`/`Min`/`Max`/`Sum`/`Avg`/`Percentile`) against a naive
  reference implementation.
- **`JournalStore::subscribe`** — appended events after subscribe
  are delivered; appended events before subscribe are not.
- **`ArtifactStore::run_retention`** — fixtures with mixed ages +
  sizes → retention removes the expected subset.
- **`CheckpointStore::prune`** — keeps newest N exactly.

### Result type

TCK functions return `tck::Result<()>` which accumulates all
failures rather than aborting on the first:

```rust
pub struct TckResult {
    pub passed:   usize,
    pub failed:   Vec<TckFailure>,
    pub skipped:  Vec<String>,
}

pub struct TckFailure {
    pub test:   String,
    pub error:  String,
    pub detail: Option<serde_json::Value>,
}
```

Individual test functions still use `assert!`, `assert_eq!`, and
`proptest` macros internally; the aggregation wrapper is for
callers that want one big "did the whole conformance run pass?"
answer without consulting the test runner's output.

## Decisions

- **D1.** Two new crates: `paramodel-mock` (minimal in-memory
  reference implementations) and `paramodel-tck` (conformance
  tests). Both depend on every paramodel trait crate. Both are
  opt-in (normal builds never include them).
- **D2.** `ImplementationProvider` is split into per-domain
  sub-traits: `ParameterProvider`, `ElementProvider`,
  `PlanProvider`, `CompilerProvider`, `ExecutorProvider`, one
  per persistence store (`ArtifactStoreProvider`, …). An
  implementer implements only the traits it supplies; missing
  providers fall back to `paramodel-mock`.
- **D3.** Conformance checks are plain generic async functions
  parameterised over provider trait bounds. `proptest` drives
  algebraic-law tests.
- **D4.** The `paramodel_tck::run_all!` macro wires every
  conformance check into the caller's crate as
  `#[tokio::test]`-annotated functions. Per-area macros
  (`run_parameters!`, `run_persistence!`, …) are available for
  narrower harnesses.
- **D5.** Canonical fixtures live in `paramodel-tck::fixtures`
  and cover every `RelationshipType`, every `AtomicStep` variant,
  every `TrialOrdering` mode, plus resume, fan-out, and all 25
  transitive-chain combinations.
- **D6.** Compilation golden files are JSON snapshots of
  `ExecutionGraph` output. The TCK compares byte-for-byte;
  `cargo test -- --bless` regenerates them. Golden divergence is
  a signal, not a failure — reviewers decide.
- **D7.** Reducto rules 1–8 each have a dedicated unit-test
  function (eight in total) that exercises the rule's pre/post
  conditions in isolation.
- **D8.** Executor TCK covers happy path, retry composition,
  intervention (pause/resume/stop under both `InterventionMode`s),
  checkpoint + resume, concurrency limits, barrier timeouts.
- **D9.** Persistence TCK covers round-trip, idempotent saves,
  NotFound, pagination correctness, stream correctness, plus
  store-specific semantics (result filter / aggregation, journal
  subscribe, artifact retention, checkpoint prune).
- **D10.** Mock implementations in `paramodel-mock` pass the
  full TCK by construction. Running the TCK against the mocks
  is the TCK's own integration test.
- **D11.** TCK functions return `tck::Result<()>` that
  accumulates failures so callers can produce single-report
  outcomes. Individual assertions still use `assert!` / `proptest`
  internally.
- **D12.** Security, auth, and multi-user conformance are out of
  scope for Phase 1. They join the TCK when the auth SRD lands
  (SRD-0002 §9 Phase 2 item 24).
