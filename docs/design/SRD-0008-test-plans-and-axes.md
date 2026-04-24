<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0008 — Test Plans and Axes

## Purpose

A **test plan** composes elements, axes that vary specific element
parameters, fixed plan-level parameter bindings, and execution
policies into a single immutable, fingerprintable document. The
test plan is what the user authors and what the compiler consumes;
everything downstream (the Element Instance Graph and the
Execution Graph from SRD-0002 §6.4) is derived from it.

This SRD defines `TestPlan`, `TestPlanBuilder`, `Axis`,
`ExecutionPolicies`, `OptimizationStrategy`, `TrialOrdering`, and
the plan-level binding layer. It does **not** specify compilation
rules (those live in SRD Phase 1 item 8).

## Scope

**In scope.**

- `TestPlan` — immutable struct built via `TestPlanBuilder`. Carries
  elements, axes, plan-level bindings, policies, optimisation
  strategy, trial ordering, metadata.
- `Axis` — a parameter elevated to a study dimension, owned
  explicitly by an `(ElementName, ParameterName)` coordinate.
- `ExecutionPolicies` — retry, timeout, intervention, partial-run
  behaviour, on-failure policy.
- `OptimizationStrategy` enum (`None`, `Basic`, `PruneRedundant`,
  `Aggressive`).
- `TrialOrdering` enum (`Sequential`, `Shuffled { seed }`,
  `EdgeFirst`, `DependencyOptimized`, `CostOptimized`,
  `Custom { name }`).
- Plan-level parameter bindings — a `BTreeMap` keyed by
  `(ElementName, ParameterName)` that overrides an element's own
  `Configuration` entries.
- Plan fingerprinting — canonical byte form hashed with BLAKE3.
- Axis reordering — `TestPlan::with_axes_reordered(...) -> Result<TestPlan>`
  returning a new plan.
- Validation rules that the builder enforces at construction time
  (structural, not semantic).

**Out of scope.**

- Compilation rules (how the plan becomes an Element Instance
  Graph and then an Execution Graph) — SRD Phase 1 item 8.
- Token-expression grammar in plan-level bindings — deferred per
  SRD-0007 D22.
- `PlanValidator` style deep semantic validation (cycle detection,
  scope containment, schedulability checks) — the compiler does
  those as part of its pipeline; this SRD's builder only enforces
  local invariants.
- Runtime intervention commands (pause, resume, stop) — executor
  SRD.
- Plan persistence/versioning beyond the immutability + fingerprint
  model — persistence SRD.

## Background

Upstream reference:
- `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/plan/TestPlan.java`
- `.../plan/Axis.java`
- `.../plan/AttachedParameter.java`
- `.../plan/OptimizationStrategy.java`
- `.../plan/TrialOrdering.java`
- `.../plan/policies/ExecutionPolicies.java`
- `links/hyperplane/docs/studies/study_system.md` (for the
  `on_failure` policy and the bindings section)
- `links/paramodel/paramodel-engine/src/main/java/io/nosqlbench/paramodel/engine/planners/reducto/reducto.md`
  — Section "Method / Stage One" discusses axis ranks and the
  element-scoped ownership model.

Shape observations that drive the Rust design:

1. **Axis ownership is explicit.** Reducto is unambiguous:
   "axes are defined on parameters, and parameters belong to
   elements. The canonical structural representation reflects this
   natural ownership: axes are scoped within the element that owns
   the parameter they vary." The Rust `Axis` therefore carries
   `element: ElementName, parameter: ParameterName` directly. No
   name-matching heuristic, no `AttachedParameter` wrapper type.
2. **TestPlan is pure data.** Upstream has `TestPlan.commit()`
   that mutates an internal flag and produces an `ExecutionPlan`.
   In Rust, `TestPlan` is pure and immutable; compilation is a
   free function (`Compiler::compile(&TestPlan)`) defined by the
   compiler SRD. The Draft/Committed distinction upstream encodes
   is already handled by the builder-vs-struct split (SRD-0004
   R4).
3. **Axis values are typed.** Upstream `Axis<T>` is generic; Rust
   carries `values: Vec<Value>` with a builder invariant that every
   value's kind matches the target parameter's kind. Heterogeneous
   axis values across parameter kinds are rejected at build time.
4. **Plan-level bindings are real.** The hyperplane study system
   YAML has a `bindings:` block (`VECTORBENCH_BASE_URL = "http://${jvector.service_addr}"`).
   We add this to the plan layer, sitting between axes and an
   element's own `Configuration`.
5. **`TrialOrdering::Custom` can't carry a closure.** Upstream's
   `custom(Comparator<Trial>)` bakes a lambda into the plan; that
   breaks our serde-everywhere stance. We turn it into
   `Custom { name }` — the name dispatches to a registered custom
   ordering at compile time.

## Plan structure at a glance

```
  TestPlan
  │
  ├── name, id, metadata
  │
  ├── elements: Vec<Element>      ← copies from Element Graph (SRD-0007)
  │   │
  │   └── each has parameters with Configuration (element-level defaults)
  │
  ├── axes: Vec<Axis>              ← parameter sweeps
  │   │
  │   └── Axis { name, target: (ElementName, ParameterName), values: Vec<Value> }
  │
  └── trial_policies: trial_timeout, retry, ordering


  value resolution chain (first wins):

    axis binding for this trial
         │
         ▼ else
    element Configuration entry
         │
         ▼ else
    parameter default
         │
         ▼ else
    parameter required + unbound → compile error
```

## Design

All types live in the `paramodel-plan` crate (new). Dependencies:
`paramodel-elements`, `paramodel-trials`.

### `TestPlanId` and `PlanName`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TestPlanId(ulid::Ulid);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct PlanName(String);     // human-authored identifier

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct AxisName(String);     // per-plan unique axis label
```

`PlanName` validation: non-empty, ≤ 128 bytes, ASCII-identifier-
safe with `-` allowed. `AxisName` follows the same rules as
`ParameterName`.

### `TestPlan`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TestPlan {
    pub id:                    TestPlanId,
    pub name:                  PlanName,

    /// Elements participating in the study, carried inline.
    /// Unique by `Element.name`. Order is preserved and feeds
    /// reducto's depth-first axis-rank assignment.
    pub elements:              Vec<Element>,

    /// Axes that vary specific element parameters. Ordered; the
    /// order determines major→minor axis prioritisation (reducto
    /// mixed-radix ranks).
    pub axes:                  Vec<Axis>,

    /// Plan-level parameter bindings. Override an element's own
    /// Configuration entries for the duration of this plan. A
    /// binding here may be a literal Value or a TokenExpr.
    pub bindings:              PlanBindings,

    pub policies:              ExecutionPolicies,
    pub optimization_strategy: OptimizationStrategy,
    pub trial_ordering:        TrialOrdering,

    pub labels:                Labels,
    pub tags:                  Tags,

    pub metadata:              TestPlanMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TestPlanMetadata {
    pub created_at:  jiff::Timestamp,
    pub created_by:  Option<String>,
    pub description: Option<String>,
    pub version:     Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct PlanBindings(BTreeMap<ElementParameterRef, ConfigEntry>);

/// The (element, parameter) coordinate used throughout for binding
/// lookups and axis targets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
pub struct ElementParameterRef {
    pub element:   ElementName,
    pub parameter: ParameterName,
}
```

`TestPlan` has no mutating methods. Iteration on a plan is
"build, commit, replace":

- Build a plan via `TestPlan::builder()`.
- Pass `&TestPlan` to the compiler or persist it.
- To revise, call `plan.edit() -> TestPlanBuilder` (clones back
  into a builder), modify, and `.build()` again — the result is a
  *new* `TestPlan` with a new `TestPlanId` and a new fingerprint.
  Dependents reference plans by fingerprint; no one is silently
  disturbed by edits.

Accessors on `&TestPlan`:

```rust
impl TestPlan {
    pub fn trial_space_size(&self) -> u64;     // product of axis cardinalities

    pub fn element(&self, name: &ElementName)  -> Option<&Element>;
    pub fn axis  (&self, name: &AxisName)      -> Option<&Axis>;

    /// Lookup a plan-level binding for a specific element parameter.
    pub fn binding(&self, coord: &ElementParameterRef) -> Option<&ConfigEntry>;

    pub fn fingerprint(&self) -> Fingerprint;   // cached on first call

    /// Returns a builder pre-populated from this plan, for iteration.
    pub fn edit(&self) -> TestPlanBuilder<_>;

    pub fn with_axes_reordered(&self, order: &[AxisName]) -> Result<TestPlan>;
}
```

### `Axis`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct Axis {
    /// Plan-unique identifier for the axis.
    pub name: AxisName,

    /// The (element, parameter) coordinate this axis varies.
    pub target: ElementParameterRef,

    /// Ordered values the axis sweeps. Every value's kind must
    /// match the target parameter's kind (builder-checked).
    pub values: Vec<Value>,

    /// Boundary values for edge-first ordering. Defaults to
    /// {first, last} of `values`; override for non-endpoint
    /// boundaries (e.g. a mid-range "typical" value in a
    /// non-monotonic sweep).
    pub boundary_values: Option<Vec<Value>>,

    pub description: Option<String>,
    pub labels:      Labels,
    pub tags:        Tags,
}

impl Axis {
    pub fn cardinality     (&self) -> usize { self.values.len() }
    pub fn element_name    (&self) -> &ElementName   { &self.target.element   }
    pub fn parameter_name  (&self) -> &ParameterName { &self.target.parameter }
    pub fn contains        (&self, value: &Value) -> bool;
    pub fn index_of        (&self, value: &Value) -> Option<usize>;
    pub fn effective_boundary_values(&self) -> &[Value];  // boundary_values or [first,last]
}
```

Validation (enforced by both `Axis::builder().build()` and by
`TestPlan::builder().build()` — the plan builder re-validates in
its own context):

- `values` non-empty.
- Every value in `values` and `boundary_values` matches the target
  parameter's kind (checked against the plan's element list during
  plan construction).
- `boundary_values`, if present, is a subset of `values`.
- No duplicate values within `values` (axis values are distinct
  points; repetition is a sampling concern, not an axis concern).

Axes are *not* `DerivedParameter`-eligible — if the plan builder
sees an axis targeting a parameter that's declared as
`Parameter::Derived`, it fails with `DerivedParameterCannotBeAxis`.

### `ExecutionPolicies`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ExecutionPolicies {
    pub trial_retry:              RetryPolicy,
    pub element_deployment_retry: RetryPolicy,

    pub trial_timeout:         Option<std::time::Duration>,
    pub element_start_timeout: Option<std::time::Duration>,

    pub intervention_mode:   InterventionMode,
    pub partial_run_behavior: PartialRunBehavior,
    pub on_failure:          OnFailure,
}

impl ExecutionPolicies {
    pub fn defaults() -> Self { /* sensible defaults: no retry, no timeout, immediate intervention, retain, skip */ }
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct RetryPolicy {
    pub max_attempts:     u32,                // 1 = no retries
    pub backoff:          BackoffStrategy,
    pub retryable_errors: BTreeSet<String>,   // error kinds that trigger retry
}

impl RetryPolicy {
    pub fn none() -> Self { Self { max_attempts: 1, backoff: BackoffStrategy::Immediate, retryable_errors: BTreeSet::new() } }
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackoffStrategy {
    Immediate,
    Fixed       { delay:   std::time::Duration },
    Linear      { base:    std::time::Duration },
    Exponential { factor:  f64, initial: std::time::Duration },
}

impl BackoffStrategy {
    pub fn delay_for_attempt(&self, attempt: u32) -> std::time::Duration { /* ... */ }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterventionMode {
    /// Honour pause/stop requests immediately, even mid-trial.
    Immediate,
    /// Finish active trials before honouring pause/stop.
    AfterActiveTrials,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartialRunBehavior {
    /// Keep partial results when the run is interrupted.
    RetainResults,
    /// Treat any interruption as a full-run failure.
    FailRun,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OnFailure {
    /// Halt the study on the first failed trial.
    Stop,
    /// Mark the trial failed and continue. (Default.)
    Skip,
    /// Retry the trial up to `attempts` times before marking failed.
    Retry { attempts: u32 },
}
```

`OnFailure` is drawn from the hyperplane study system
(`links/hyperplane/docs/studies/study_system.md`, §8
"Partial failure"). The other policies match upstream's
`ExecutionPolicies` interface.

### `OptimizationStrategy`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptimizationStrategy {
    /// No optimisation. Plans compile with every per-trial
    /// activate/deactivate step materialised.
    None,
    /// Apply safe, uncontroversial reductions (group coalescing
    /// under PERSIST, transitive-edge removal). Default.
    Basic,
    /// Aggressively eliminate redundant trials where the plan can
    /// prove equivalence.
    PruneRedundant,
    /// All of the above plus reordering for cache locality and
    /// resource packing.
    Aggressive,
}

impl Default for OptimizationStrategy {
    fn default() -> Self { Self::Basic }
}
```

Exact semantics are pinned in the compilation SRD; this SRD pins
only the shape and the default.

### `TrialOrdering`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrialOrdering {
    /// Lexicographic traversal in axis definition order.
    Sequential,

    /// Pseudo-random permutation (reproducible given seed).
    Shuffled           { seed: u64 },

    /// Three-phase: corners, edges, interior fill.
    EdgeFirst,

    /// Minimise element deploy/teardown churn.
    DependencyOptimized,

    /// Expensive trials first for fail-fast.
    CostOptimized,

    /// Custom ordering registered with the compiler by name.
    Custom             { name: String },
}

impl Default for TrialOrdering {
    fn default() -> Self { Self::Sequential }
}
```

The compiler holds a registry of named custom orderings; the plan
carries only the name so the decision serialises cleanly.

### `PlanBindings` and the resolution chain

A plan-level binding overrides an element's own `Configuration`
for the duration of this plan. Resolution precedence for "what
value does parameter P of element E get on trial T?" (superseding
SRD-0007 D21 step 2):

1. **Axis** — if an axis targets `(E, P)` for trial T, its value
   wins.
2. **Plan binding** — if `PlanBindings[(E, P)]` is present, that
   `ConfigEntry` (literal or resolved token) is used.
3. **Element configuration** — `E.configuration[P]` if present.
4. **Parameter default** — `P.default` if set.
5. **Error** — `Error::UnboundParameter { element, parameter }`.

SRD-0007 D21 is amended by this SRD's D6 to insert step 2.

### Fingerprinting

Plan fingerprint is deterministic and covers every authored bit
that the compiler would consume. Canonical form:

```
PLAN_TAG (0x20)
  ‖ utf8_len_prefixed(name.as_str())
  ‖ u32_le(elements.len())   for each in elements order:
      ‖ element.fingerprint()            (see below)
  ‖ u32_le(axes.len())       for each in axis order:
      ‖ axis.fingerprint()
  ‖ u32_le(bindings.len())   sorted by (element, parameter):
      ‖ binding.fingerprint()
  ‖ policies.fingerprint()
  ‖ canonical_bytes(optimization_strategy)
  ‖ canonical_bytes(trial_ordering)
```

`Element::fingerprint()`, `Axis::fingerprint()`,
`PlanBindings` entry fingerprint, and `ExecutionPolicies::fingerprint()`
are computed over their canonical byte forms (kind tag +
length-prefixed fields + sorted maps + recursive fingerprints for
nested `Value`s). Hash algorithm is BLAKE3 per SRD-0003 D7.

Labels, tags, metadata, and `id` are **not** in the plan's
fingerprint. A plan fingerprint answers "do these two plans
describe the same study?" — labels and tags don't change the
study; the id is just an identifier.

### `TestPlanBuilder` validations

The builder validates these at build time (returns `Result`):

- Every `Element` in `elements` has a unique `name`.
- Every `Axis.target` names an element in `elements` and a
  parameter declared on that element.
- Every `Axis.values` entry has the same kind as the target
  parameter.
- Targeted parameter is not a `Parameter::Derived` variant.
- Axis names (`AxisName`) are unique within the plan.
- `(element, parameter)` pairs are unique across axes — no two
  axes varying the same parameter.
- Every `PlanBindings` entry references an element and a parameter
  that exist in `elements`.
- No `PlanBindings` entry targets the same `(element, parameter)`
  as an axis (would be overridden anyway; reject as authoring
  error).
- `elements` satisfies intra-element invariants inherited from
  SRD-0007 (labels include `type`, namespace uniqueness, etc.).
- `policies` values are internally consistent (e.g. `RetryPolicy.max_attempts >= 1`).

These are **structural**. Semantic checks — dependency cycle
detection, scope containment, schedulability, namespace
uniqueness across dependency targets — are the compiler's job.

### Axis reordering

```rust
impl TestPlan {
    pub fn with_axes_reordered(&self, order: &[AxisName]) -> Result<TestPlan>;
}
```

- `order` must be a permutation of the plan's existing axis names.
- Returns a new `TestPlan` with a new `id` and a new
  `fingerprint`. Other content is unchanged.
- Missing names, duplicates, or unknown names are
  `Error::InvalidAxisOrder`.

This is a convenience for "try the same study with a different
major axis"; the alternative is to edit and rebuild manually via
`plan.edit()`.

## Decisions

- **D1.** `TestPlan` is an immutable struct built by
  `TestPlanBuilder`. No `commit()` method, no mutable state. The
  compiler is a free function that consumes `&TestPlan`.
- **D2.** `TestPlanId` is a newtype over `ulid::Ulid`. Plan
  equality for "same study?" uses fingerprints, not ids.
- **D3.** `Axis` is owned by an explicit `(ElementName,
  ParameterName)` coordinate. No name-matching heuristic, no
  `AttachedParameter` wrapper.
- **D4.** Axis values are typed against the target parameter's
  kind, validated by the builder. Derived parameters are not
  eligible as axes.
- **D5.** Axis order on the `TestPlan` is authoritative for
  major→minor prioritisation and reducto's mixed-radix axis ranks.
  `TestPlan::with_axes_reordered(&[AxisName])` returns a new plan
  for trying different orderings.
- **D6.** Parameter-value resolution for trial T on element E
  parameter P follows this precedence: axis → plan binding →
  element configuration → parameter default → error. This
  supersedes SRD-0007 D21 by inserting the plan-binding layer.
- **D7.** `ExecutionPolicies` carries trial retry, element
  deployment retry, trial timeout, element start timeout,
  `InterventionMode`, `PartialRunBehavior`, and `OnFailure`.
  Mirrors upstream plus the `OnFailure` policy from the hyperplane
  study system.
- **D8.** `RetryPolicy` is a struct (`max_attempts`, `backoff`,
  `retryable_errors`). `BackoffStrategy` is an enum
  (`Immediate`, `Fixed`, `Linear`, `Exponential`).
- **D9.** `OptimizationStrategy` has four variants (`None`,
  `Basic`, `PruneRedundant`, `Aggressive`); default is `Basic`.
- **D10.** `TrialOrdering` is a serde-able enum; `Custom { name }`
  dispatches through a compiler-side registry rather than
  carrying a closure.
- **D11.** Plan fingerprint is BLAKE3 over canonical bytes
  covering elements, axes, bindings, policies, optimisation
  strategy, and trial ordering. Labels, tags, metadata, and `id`
  are excluded.
- **D12.** `TestPlanBuilder` enforces structural invariants
  (element-name uniqueness, axis target validity, axis-value
  kind-matching, plan-binding target validity, axis-name
  uniqueness, parameter coverage by at most one authoring
  source). Semantic invariants (cycles, scope containment,
  schedulability) are deferred to the compiler SRD.
- **D13.** `TestPlan::edit(&self) -> TestPlanBuilder` supports
  iterative authoring; each build yields a new plan with a new
  id and fingerprint so downstream references pin versions
  deterministically.
- **D14.** Labels and tags are carried on `TestPlan`, `Axis`, and
  `TestPlanMetadata`. They are not in the plan fingerprint; they
  are for organisation and traceability.
- **D15.** Incoherent combinations of plan-level parameters are
  **compile errors**, not warnings. We start strict and relax
  case by case if empirical experience justifies it. Initial
  rules the compiler must enforce:
  - `TrialOrdering::EdgeFirst` requires a sampling strategy that
    is guaranteed to include boundary points. `SamplingStrategy::Grid`
    qualifies; `SamplingStrategy::Linspace { count }` qualifies iff
    `count >= 2` (endpoints retained); `SamplingStrategy::Random`
    does not.
  - `TrialOrdering::Custom { name }` with a name that is not
    registered with the compiler is an error.
  - Axis-value types that don't match the target parameter's kind
    (also enforced by the builder at D12) remain errors.

  Additional coherence rules accumulate in the compiler SRD as
  they are identified; this SRD does not attempt an exhaustive
  list. The principle is the compiler's job: the plan is what the
  user authors, and the plan-to-execution bridge flags
  contradictions before execution begins.

