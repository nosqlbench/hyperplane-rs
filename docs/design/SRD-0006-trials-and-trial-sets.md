<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0006 — Trials and Trial Sets

## Purpose

A **trial** is one concrete configuration of an element deployment
— a complete assignment of values across every (element,
parameter) coordinate that will run together. A **trial set** is a
collection of trials the user wants to explore — the sampled points
of the configuration space. A **trial result** is what an element
deployment produced when the trial was run.

Everything in this SRD is in service of the element model: trials
*parameterise* elements, trial sets *collect* parameterisations the
user wants to explore, and trial results *record* what those
elements produced. The shapes defined here — `Trial`, `TrialSet`,
`SamplingStrategy`, `TrialStatus`, `TrialResult` — are the
vocabulary the compiler, the executor, and the persistence layer
use to talk about one run of one deployment.

> **Naming note.** Earlier drafts used `Sequence` for what is now
> `TrialSet`. The rename reflects the semantic pinned in D5 below:
> a collection has no inherent execution order. Linear inter-element
> ordering within a trial is expressed on element dependencies
> (`RelationshipType::Linear`, SRD-0007 D4); there is no authored
> inter-trial ordering at this layer.

## Scope

**In scope.**

- `Trial` — an immutable assignment of `Value`s across (element,
  parameter) coordinates, plus an identifier and optional
  metadata.
- `TrialSet` — an immutable collection of trials with an
  associated sampling strategy. `Vec<Trial>` storage is for stable
  enumeration / reproducibility; *not* an execution schedule.
- `SamplingStrategy` — how the trial set was (or will be)
  populated: `Grid`, `Random { count, seed }`, `Linspace { count }`.
- `TrialStatus` — lifecycle enum: `Pending`, `InProgress`,
  `Completed`, `Failed`, `Skipped`, `Cancelled`.
- `TrialResult` — the outcome after execution: status, typed
  metrics, artifact references, timing, provenance, error info,
  attempt number.
- `Trial::canonical_bytes()` — a stable byte encoding exposed for
  callers that need to hash a trial at a trust boundary
  (persistence, audit). No cached fingerprint, no fingerprint
  field.
- Boundary with the compiler: trial *codes* (mixed-radix digit
  strings from reducto) are compiler-time metadata, not authored
  on trials directly.

**Out of scope.**

- Cross-parameter / cross-trial constraints — deferred to the
  test-plan SRD. Trial carries no constraints field (change from
  upstream, rationale below).
- Axes — named parameter dimensions with explicit value lists
  that generate trials; defined in the test-plan SRD.
- The trial-execution engine, scheduler, resource manager —
  executor SRD.
- Persistence wire format for results — persistence SRD. If the
  store wants to key records by content hash, it computes that
  hash from `Trial::canonical_bytes()` at write time; the trial
  type itself does not carry one.
- Element-specific output-capture behaviours (`SaveOutputCommand`,
  `OUTPUT_VOLUME`) — hyperplane-tier element SRDs.
- Trial- or set-level fingerprints as first-class methods or
  fields. Fingerprints on individual `Value`s stay (SRD-0004 D10 —
  per-value tamper detection is its own concern). See D12 for the
  reasoning.

## Background

Upstream reference (read, not copied):

- `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/sequence/{Trial,Sequence,TrialResult,TrialStatus,TrialBuilder,SequenceBuilder}.java`
- `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/parameters/SamplingStrategy.java`

Observations from the source that shape this SRD:

- **Trial-level constraints are unused in the planner.** Upstream
  `Trial.constraints()` returns
  `List<Constraint<Map<String, Value<?>>>>` — closures that test
  the assignment map. The reducto planner does not consume these;
  cross-parameter validity is resolved in the test-plan layer
  (plan validation, study composer, axis locality rules). We drop
  this field on `Trial` to keep the type focused; the test-plan
  SRD will carry the equivalent as a plan-level constraint
  collection evaluated during enumeration.
- **Metrics are untyped in upstream.** `TrialResult.metrics()`
  returns `Map<String, Object>`. Rust can do better: metrics are
  outputs declared by an element's `@result` annotations (from
  the Dockerfile convention) and are naturally typed values. We
  use the same `Value` enum for metrics as for assignments, so
  metric outputs flow through the same typing and serde paths as
  inputs.
- **Provenance is shallow.** Upstream `ProvenanceInfo` on
  `TrialResult` carries a `configurationFingerprint` string, an
  optional sequence id, an optional execution-plan version, and a
  free-form environment map. We simplify: keep the id and the
  environment map; drop the fingerprint fields (see D12).
- **`ArtifactReference.sizeBytes` is a `long` in upstream with a
  convention that `-1` means unknown.** In Rust this is clearly
  `Option<u64>`.
- **Upstream "Sequence" implied ordered execution.** Post-reducto,
  the execution graph defines ordering via element dependencies
  and resource constraints; trial-set position carries no
  scheduling semantics. The type is renamed `TrialSet` and its
  `Vec<Trial>` storage is labelled as authoring/enumeration order
  only (D5).

## Trial model at a glance

```
  TestPlan authoring                    Compiler (SRD-0010)
       │                                     │
       │ axes: 2 dims × 3 values             │ mixed-radix enumeration
       │ (e.g. dataset ∈ {small, med, big}   │ + binding-state
       │  concurrency ∈ {1, 10})             │
       │                                     ▼
       └────────────▶ 6 trials (2 × 3 cartesian)
                      ┌──────────────────────────┐
                      │ Trial { id, assignments, │
                      │         labels, tags }   │
                      └──────────────────────────┘

  Assignments: element_name → parameter_name → Value
      client.dataset     = "small"
      client.concurrency = 1
      harness.version    = "1.0"    (not on an axis; config)

  TrialSet collects all trials for one execution. Paramodel
  preserves identity across: authoring (expected set), execution
  (running set), results (completed set).
```

## Design

All types live in the `paramodel-trials` crate. The crate depends
on `paramodel-elements` (for `Value`, `Labels`, `Tags`, name newtypes,
`TrialId`) and nothing else.

### Name types borrowed

These come from earlier SRDs, reused here without restatement:

- `ElementName` (defined in SRD-0007; newtype contract pinned in
  SRD-0003).
- `ParameterName` (SRD-0004).
- `Value` (SRD-0004) — the per-kind enum with `Provenance`
  carrying `parameter`, `generated_at`, `generator`, `fingerprint`.
- `TrialId` (already defined in `paramodel-elements::ids` per the
  cross-crate foundation — see SRD-0003 D8).
- `Labels`, `Tags` (SRD-0005).

### `Trial`

```rust
#[derive(Debug, Clone, PartialEq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct Trial {
    pub id:          TrialId,
    pub assignments: Assignments,
    pub labels:      Labels,     // intrinsic (e.g. trial_code once compiled)
    pub tags:        Tags,       // organisational
    pub metadata:    Option<TrialMetadata>,
}

/// Two-level map: element name → parameter name → value.
#[derive(Debug, Clone, PartialEq,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Assignments(BTreeMap<ElementName, BTreeMap<ParameterName, Value>>);

impl Assignments {
    pub fn get(&self, element: &ElementName, param: &ParameterName) -> Option<&Value>;
    pub fn for_element(&self, element: &ElementName) -> Option<&BTreeMap<ParameterName, Value>>;
    pub fn iter(&self) -> impl Iterator<Item = (&ElementName, &ParameterName, &Value)>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TrialMetadata {
    pub enumeration_index: Option<u32>,   // position in the producing TrialSet, when relevant
    pub group:             Option<String>,
    pub generation_method: Option<String>,
    pub priority:          Option<i32>,
}
```

Invariants (checked at construction):

- For every `(element, param) → value` entry,
  `value.provenance().parameter == param`. No silently mis-addressed
  values.
- No element appears with an empty inner map.
- All map ordering is by `BTreeMap` natural order — needed for
  stable canonical bytes.

`Trial` has no `validate()` method. Trial-level validity is
decided at the test-plan layer, which knows which axes and
parameters should participate. Self-consistency (the invariants
above) is enforced by the builder on construction.

Note that `Trial` is not `Eq` because a `Value::Double` payload
may be an `f64`. Equality is `PartialEq` only.

### `TrialSet`

```rust
#[derive(Debug, Clone, PartialEq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TrialSet {
    pub id:       TrialSetId,
    pub strategy: SamplingStrategy,
    pub trials:   Vec<Trial>,
    pub labels:   Labels,
    pub tags:     Tags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TrialSetId(ulid::Ulid);

impl TrialSet {
    pub fn len(&self)     -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self)    -> std::slice::Iter<'_, Trial>;
    pub fn get(&self, i: usize) -> Option<&Trial>;
}
```

A `TrialSet` is an immutable collection of trials plus an identifier
and the strategy that produced the population. Construction is via
the builder; mutation happens only by building a new `TrialSet`.
The strategy is carried on the set (not the trials) because it
describes *how* the trial list was populated, not what any
individual trial is.

**Execution order is not expressed by `Vec<Trial>` position.** The
Vec is the stable authoring/enumeration order — reproducible
across compilations of the same plan, useful for reporting ("trial
3 of 12 failed"), and the basis for `TrialMetadata.enumeration_index`
when a trial wants to remember where it sat in its producing set.
The executor may run trials concurrently subject to
element-graph dependencies (`RelationshipType::Linear`, SRD-0007),
per-element concurrency caps (`max_concurrency`,
`max_group_concurrency`, SRD-0002 R26), and scheduler policy. Any
required inter-element ordering is expressed at the element graph,
not here.

### `SamplingStrategy`

```rust
#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SamplingStrategy {
    /// Exhaustive cartesian product of axis values.
    Grid,

    /// Random sampling with a deterministic seed.
    Random { count: u32, seed: u64 },

    /// Evenly-spaced points across numeric axes.
    Linspace { count: u32 },
}

impl SamplingStrategy {
    pub fn grid()                       -> Self { Self::Grid }
    pub fn random(count: u32, seed: u64) -> Self { Self::Random { count, seed } }
    pub fn linspace(count: u32)         -> Self { Self::Linspace { count } }
}
```

Direct port of upstream's sealed interface with the same variants
and validity rules (non-zero counts, enforced in constructors).
Additional strategies (pairwise, edge-first, adaptive) may arrive
later; they slot in as new variants of this enum.

### `TrialStatus`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
    Cancelled,
}

impl TrialStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending | Self::InProgress)
    }
    pub fn is_success(&self) -> bool { *self == Self::Completed }
    pub fn is_failure(&self) -> bool { *self == Self::Failed }
}
```

Same variant set as upstream. The status of a still-running trial
is `InProgress`; `Skipped` applies when the planner decides not to
execute it (e.g. upstream element failed); `Cancelled` is user-
initiated. `is_terminal` guards the transition from "scheduler
cares" to "result is final."

### `TrialResult`

```rust
#[derive(Debug, Clone, PartialEq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TrialResult {
    pub trial_id:    TrialId,
    pub status:      TrialStatus,
    pub attempt:     u32,              // ≥ 1

    pub metrics:     Metrics,          // typed, two-level
    pub artifacts:   Vec<ArtifactRef>,
    pub timing:      ExecutionTiming,
    pub provenance:  TrialProvenance,

    pub error:       Option<ErrorInfo>,       // present iff status == Failed
    pub skip_reason: Option<String>,          // present iff status == Skipped
    pub labels:      Labels,
    pub tags:        Tags,
}

/// Typed metrics, same two-level shape as Trial's assignments but
/// populated from elements' `@result`-declared output parameters.
#[derive(Debug, Clone, PartialEq,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Metrics(BTreeMap<ElementName, BTreeMap<ParameterName, Value>>);

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ArtifactRef {
    pub element:      ElementName,
    pub name:         String,
    pub uri:          String,                // resolvable by the ArtifactStore
    pub content_type: String,                // MIME
    pub size_bytes:   Option<u64>,           // None = unknown
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize)]
pub struct ExecutionTiming {
    pub started_at:   jiff::Timestamp,
    pub completed_at: jiff::Timestamp,
}

impl ExecutionTiming {
    pub fn duration(&self) -> std::time::Duration { /* completed_at - started_at */ }
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct TrialProvenance {
    pub trial_set_id: Option<TrialSetId>,
    pub environment:  BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq,
         serde::Serialize, serde::Deserialize, bon::Builder)]
pub struct ErrorInfo {
    pub kind:         String,     // error type / class name
    pub message:      String,
    pub stack_trace:  Option<String>,
    pub is_retryable: bool,
    pub error_code:   Option<String>,
}
```

Invariants (checked at construction by the builder):

- `attempt >= 1`.
- `status == Failed` ⇒ `error.is_some()`.
- `status == Skipped` ⇒ `skip_reason.is_some()`.
- `status != Failed` ⇒ `error.is_none()`.
- `status != Skipped` ⇒ `skip_reason.is_none()`.
- `timing.completed_at >= timing.started_at`.

The typed-metrics shape means a metric has the same `Value`
provenance as an input parameter: the `Value::provenance`
records which parameter produced it, when, and its fingerprint.
That keeps "what was run" and "what was measured" on a single
algebra.

### Canonical bytes for trust boundaries

```rust
impl Trial {
    /// Stable byte encoding of this trial's content (id excluded —
    /// see D12). Callers compute a hash at the boundary where they
    /// need one (persistence write, audit compare) via
    /// `Fingerprint::of(&trial.canonical_bytes())`.
    pub fn canonical_bytes(&self) -> Vec<u8>;
}
```

Byte layout:

```
TRIAL_TAG ‖ u32_le(n_elements)
  for each (element_name, params) in assignments, sorted by element_name:
    utf8_len_prefixed(element_name) ‖ u32_le(n_params)
    for each (param_name, value) in params, sorted by param_name:
      utf8_len_prefixed(param_name) ‖ value.fingerprint() (32 bytes)
```

`TRIAL_TAG = 0x60` keeps the encoding disjoint from the constraint
canonical-byte ranges (`0x10..=0x5A`) and value tag ranges
(`0x01..=0x05`) already in use in `paramodel-elements`. `TrialId`,
`metadata`, `labels`, and `tags` are **not** in the encoding —
those are ancillary to identity.

No cached field, no `fingerprint()` method. Users that need a hash
compute `Fingerprint::of(&t.canonical_bytes())` on demand. Two
`Trial` values with equal canonical bytes produce equal hashes by
construction.

`TrialSet` does not expose `canonical_bytes()` in v1; if a caller
needs a set-level hash, they can fold over `trials`' canonical
bytes themselves. The use case hasn't arisen yet and adding it
later is non-breaking.

### Relation to axes and the compiler

- **Axes** (test-plan SRD) produce trials. An axis has a
  `Parameter` and an ordered list of values; the Cartesian
  product of all axes generates candidate trials.
- **Trial codes** (reducto, SRD-0002 R27) are a *human-legible*
  digit string for a trial in a plan's mixed-radix enumeration
  (e.g. `0x0200`). They are produced during compilation and
  stamped onto the trial as a **label** with a well-known key
  (proposal: `trial_code`, defined in the compiler SRD). Trial
  codes are plan-scoped, not set-scoped; a trial in two different
  plans has two different codes. The compiler SRD owns the code,
  not this SRD.
- **Sampling strategies** (`Grid`, `Random`, etc.) choose *which*
  of the Cartesian-product trials actually populate the set. They
  live on the `TrialSet` alongside the trials they produced.

## Decisions

- **D1.** `Trial` is a struct (not a trait). Fields: `id`,
  `assignments`, `labels`, `tags`, `metadata`. Construction is
  via `bon::Builder`; the type is immutable after construction.
- **D2.** `TrialId` lives in `paramodel-elements::ids` (foundational,
  cross-crate). See SRD-0003 D8. IDs are not part of any canonical
  byte encoding of a trial.
- **D3.** `Assignments` is a two-level `BTreeMap<ElementName,
  BTreeMap<ParameterName, Value>>` with builder-enforced
  self-consistency (value.provenance.parameter matches the inner
  key; no empty inner maps).
- **D4.** `Trial` does not carry cross-parameter constraints.
  Upstream's `Trial.constraints()` is not ported; the equivalent
  semantics live at the test-plan / plan-validation layer.
- **D5.** `TrialSet` is a struct with `id`, `strategy`, `trials`,
  `labels`, `tags`. `Vec<Trial>` is authoring/enumeration order
  only; it carries **no execution-order semantic**. The executor
  runs trials concurrently subject to element-graph dependencies,
  per-element concurrency caps, and scheduler policy.
- **D6.** `TrialSetId` is a newtype over `ulid::Ulid`.
- **D7.** `SamplingStrategy` is a Rust enum (`Grid`,
  `Random { count, seed }`, `Linspace { count }`) with
  `#[serde(tag = "kind")]`. Direct port of upstream's sealed
  interface, ready to accept additional variants (pairwise,
  edge-first, …) without breaking serde compatibility for existing
  ones.
- **D8.** `TrialStatus` is a six-variant enum matching upstream
  (`Pending`, `InProgress`, `Completed`, `Failed`, `Skipped`,
  `Cancelled`) with `is_terminal`, `is_success`, `is_failure`
  accessors.
- **D9.** `TrialResult` is a struct carrying typed metrics, typed
  artifacts, typed timing, typed provenance, and optional error/
  skip-reason fields. Status-conditional invariants (failed ⇒
  error, skipped ⇒ skip_reason) are enforced by the builder.
- **D10.** Metrics use the same `Value` enum and two-level
  `BTreeMap<ElementName, BTreeMap<ParameterName, Value>>` shape
  as assignments. A metric is "the value of an element's result
  parameter for this trial," not a free-form key/value pair.
- **D11.** Artifact references carry `element`, `name`, `uri`,
  `content_type`, and `size_bytes: Option<u64>` (upstream's
  `-1`-means-unknown convention becomes `None`).
- **D12.** No trial- or set-level fingerprint methods, fields, or
  caches. The original rationale for them was a hash-based
  scheduling algorithm that reducto replaced. Tamper detection and
  content-addressable linkage remain achievable at trust
  boundaries via `Trial::canonical_bytes()` + `Fingerprint::of(…)`,
  computed by the persistence / audit layer on demand.
  Per-`Value` fingerprints are unaffected (SRD-0004 D10 — tamper
  detection at the value layer is independently justified).
- **D13.** `TrialProvenance` carries `trial_set_id` and
  `environment`. No fingerprint fields; verification is a
  boundary-layer concern per D12. Extra provenance bits (host
  info, plan reference) can be added here later without breaking
  existing records.
- **D14.** Timing uses `jiff::Timestamp` for wall-clock points
  and `std::time::Duration` (derived) for spans, per SRD-0003 D5.
- **D15.** Trial codes (reducto's mixed-radix digit strings) are
  compiler-produced and attached to a `Trial` as a `Labels` entry
  (well-known key `trial_code`) during compilation. This SRD does
  not define the code format; the compiler SRD does.
