<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0012 — Persistence Traits

## Purpose

Define the storage-boundary traits paramodel exposes to any host
system that wants to persist test plans, execution plans,
executions, journals, checkpoints, trial results, and artifacts.
Per SRD-0002 R7 and R11, paramodel commits to the *shape* of
persistence without committing to a backend — hyperplane will
supply a single SQLite implementation of every trait in a later
SRD.

Six stores cover every durable artefact paramodel produces:

| Trait                   | Stores                                                                 |
|-------------------------|------------------------------------------------------------------------|
| `ArtifactStore`         | Artifact blobs from `SaveOutput` steps.                                |
| `CheckpointStore`       | `Checkpoint` snapshots from the executor (SRD-0011).                   |
| `ExecutionRepository`   | `ExecutionPlan` records + aggregate `ExecutionResults`.                |
| `JournalStore`          | Append-only stream of `JournalEvent`s (SRD-0011).                      |
| `MetadataStore`         | `TestPlanMetadata` + `ExecutionPlanMetadata` for catalog browsing.     |
| `ResultStore`           | Per-trial `TrialResult` records with a typed query surface.            |

All traits are async (tokio, per SRD-0003 R17) and return
`Result<T, PersistenceError>`. Stream-returning methods use
`futures::Stream`.

## Scope

**In scope.**

- Trait definitions for all six stores.
- The shared `PersistenceError` enum.
- Typed query model for `ResultStore` (declarative filter AST, not
  a string DSL).
- Pagination primitives (`PageRequest`, `PageToken`, `Page<T>`).
- Streaming accessors for large collections (`Stream<T>`).
- Retention-policy hooks (for artifact and journal stores).
- What each store persists in its own domain types.
- Versioning stance — paramodel traits are version-agnostic;
  schema versioning is backend-managed.

**Out of scope.**

- The SQLite backend that hyperplane will provide — separate SRD
  in the hyperplane tier.
- Schema definitions, migrations, connection pooling — backend
  concerns.
- Cross-store transactions — the backend implementation can group
  related writes atomically; the trait surface does not expose a
  `Transaction` type. Per-method atomicity is guaranteed; multi-
  method atomicity is not.
- Authentication, authorisation, encryption at rest — hyperplane-
  tier.
- Caching and read-through layers — out of scope; callers wrap
  stores as needed.
- Metric observability on stores — left to the backend's own
  tracing.

## Background

Upstream reference:
- `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/persistence/{ArtifactStore,CheckpointStore,ExecutionRepository,JournalStore,MetadataStore,ResultStore}.java`
- `links/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/persistence/Sqlite*.java`
  (concrete impls; informative only).

Observations that shape the Rust design:

1. Upstream stores are narrow CRUD interfaces. We preserve that.
2. Upstream `ResultStore.Query` is a fluent builder. We replace it
   with a declarative filter enum — serde-able, composable, and
   consistent with SRD-0004's "no closures in the algebra" stance.
3. Upstream `JournalStore.replay(executionId, afterSequence)`
   returns a Java `Stream`. Rust equivalent is
   `impl Stream<Item = …>` behind a trait method — we use an
   associated type pattern with `BoxStream` to keep dyn-dispatch
   possible.
4. Upstream pagination is absent on most stores; queries that can
   return unbounded lists rely on `limit`/`offset` on the query
   builder. For Rust we introduce an explicit `PageRequest` /
   `PageToken` on every list-returning method where the natural
   result size is unbounded.
5. Upstream persists `ExecutionResult` (the aggregate) in
   `ExecutionRepository` and `TrialResult` in `ResultStore`.
   We keep this split — they are different domain objects.
6. `ExecutionRepository` in upstream also persists `ExecutionPlan`
   itself. We preserve that: a plan and its executions live in
   the same store, so one can navigate from plan fingerprint →
   all executions of that plan.

## Design

All traits live in the `paramodel-persistence` crate (new).
Dependencies: `paramodel-elements`, `paramodel-trials`,
`paramodel-plan`, `paramodel-executor`.

### Shared infrastructure

#### `PersistenceError`

```rust
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    #[error("not found: {kind} / {id}")]
    NotFound { kind: &'static str, id: String },

    #[error("conflict: {kind} / {id} already exists")]
    Conflict { kind: &'static str, id: String },

    #[error("version mismatch on {kind} / {id}: expected {expected}, found {actual}")]
    VersionMismatch {
        kind:     &'static str,
        id:       String,
        expected: String,
        actual:   String,
    },

    #[error("serialisation failure: {0}")]
    Serialization(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("retention policy violation: {0}")]
    Retention(String),

    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

pub type PersistenceResult<T> = std::result::Result<T, PersistenceError>;
```

Per SRD-0003 D3, each crate defines its own `Error`. This one is
shared across all six traits so callers don't need a match-ladder
per store.

#### Pagination

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageRequest {
    pub size:  u32,                // 1..=1000; backend clamps to a maximum
    pub token: Option<PageToken>,   // None = first page
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct PageToken(String);       // opaque to callers; backend-specific encoding

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Page<T> {
    pub items:      Vec<T>,
    pub next_token: Option<PageToken>,
    pub total_hint: Option<u64>,     // optional server-side count estimate
}
```

Pagination is cursor-based (opaque `PageToken`), not
offset-based, so backends can implement efficient iteration over
large tables without scanning. The default `PageRequest::size`
is implementation-defined; backends clamp to a maximum (1000
is the suggested cap).

#### Streaming

For scan-style reads where holding the whole result set in memory
is wasteful, each store offers a `stream_…` method returning an
owned `'static` stream:

```rust
pub type PersistenceStream<T> =
    futures::stream::BoxStream<'static, PersistenceResult<T>>;
```

An owned (`'static`) stream is freely composable, can be spawned
onto a tokio task, and has no lifetime surgery at call sites. The
backend puts whatever shared state the stream needs (typically an
`Arc<Pool>`) into the future itself before pinning.

Streams yield one record at a time with backend-controlled
buffering. Errors surface per-item so a partial scan is
recoverable.

### `ArtifactStore`

```rust
#[async_trait::async_trait]
pub trait ArtifactStore: Send + Sync + 'static {
    /// Store an artifact with its content. If an artifact with
    /// the same `ArtifactRef.name` under the same
    /// `(execution, trial, element)` coordinate already exists,
    /// behaviour depends on `SaveMode`.
    async fn save(
        &self,
        context:   &ArtifactContext,
        artifact:  &ArtifactRef,
        content:   ArtifactContent,
        mode:      SaveMode,
    ) -> PersistenceResult<ArtifactId>;

    async fn get       (&self, id: &ArtifactId) -> PersistenceResult<ArtifactRecord>;
    async fn download  (&self, id: &ArtifactId) -> PersistenceResult<ArtifactContent>;
    async fn list      (&self, context: &ArtifactContext, page: PageRequest) -> PersistenceResult<Page<ArtifactRecord>>;
    async fn stream    (&self, context: &ArtifactContext) -> PersistenceStream<ArtifactRecord>;
    async fn delete    (&self, id: &ArtifactId) -> PersistenceResult<()>;

    /// Apply the retention policy and remove expired artifacts.
    /// Returns a report of what was freed.
    async fn run_retention(&self, policy: &RetentionPolicy) -> PersistenceResult<CleanupReport>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactContext {
    pub execution: Option<ExecutionId>,
    pub trial:     Option<TrialId>,
    pub element:   Option<ElementName>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactRecord {
    pub id:        ArtifactId,
    pub context:   ArtifactContext,
    pub artifact:  ArtifactRef,        // from SRD-0006
    pub stored_at: jiff::Timestamp,
    pub size_bytes: u64,
    pub sha256:    Option<[u8; 32]>,    // backend may compute for integrity
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveMode {
    /// Fail if an artifact with the same logical key exists.
    CreateNew,
    /// Overwrite any existing artifact at the same logical key.
    Replace,
    /// Append a versioned copy (name suffixed with a sequence).
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RetentionPolicy {
    pub keep_newest:   Option<u32>,                 // per context
    pub max_age:       Option<std::time::Duration>,
    pub max_size_bytes: Option<u64>,                 // per context
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CleanupReport {
    pub artifacts_removed: u64,
    pub bytes_freed:       u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd,
         serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ArtifactId(ulid::Ulid);
```

`ArtifactContent` is the same enum from SRD-0011
(`Inline(Vec<u8>) | Stream(..) | Uri(String)`) — re-exported here
so callers can pass through the executor's collected content
without re-allocation.

### `CheckpointStore`

```rust
#[async_trait::async_trait]
pub trait CheckpointStore: Send + Sync + 'static {
    async fn save   (&self, checkpoint: &Checkpoint) -> PersistenceResult<()>;
    async fn get    (&self, id: &CheckpointId)        -> PersistenceResult<Checkpoint>;
    async fn delete (&self, id: &CheckpointId)        -> PersistenceResult<()>;

    /// Most recent checkpoint for an execution, or `None` if
    /// none exist.
    async fn latest  (&self, execution: &ExecutionId) -> PersistenceResult<Option<Checkpoint>>;

    async fn list    (&self, execution: &ExecutionId, page: PageRequest)
        -> PersistenceResult<Page<CheckpointHandle>>;

    /// Enforce checkpoint count per execution (keep newest N).
    async fn prune   (&self, execution: &ExecutionId, keep_newest: u32)
        -> PersistenceResult<CleanupReport>;
}
```

`CheckpointHandle` (SRD-0011) is the lightweight browsing record;
`Checkpoint` is the full snapshot. Listing returns handles;
`get` returns the full checkpoint. This keeps browsing cheap and
full loads explicit.

A checkpoint's `plan_fingerprint` is always included — the caller
(executor) can cross-check it against the plan being resumed.

### `ExecutionRepository`

```rust
#[async_trait::async_trait]
pub trait ExecutionRepository: Send + Sync + 'static {
    // -- Execution plans --------------------------------------------
    async fn save_plan    (&self, plan: &ExecutionPlan) -> PersistenceResult<()>;
    async fn get_plan     (&self, id: &ExecutionPlanId) -> PersistenceResult<ExecutionPlan>;
    async fn get_plan_by_fingerprint(&self, fp: &Fingerprint) -> PersistenceResult<Option<ExecutionPlan>>;
    async fn list_plans   (&self, filter: &PlanFilter, page: PageRequest)
        -> PersistenceResult<Page<ExecutionPlanSummary>>;

    // -- Execution results ------------------------------------------
    async fn save_execution    (&self, results: &ExecutionResults) -> PersistenceResult<()>;
    async fn get_execution     (&self, id: &ExecutionId) -> PersistenceResult<ExecutionResults>;
    async fn list_executions   (&self, filter: &ExecutionFilter, page: PageRequest)
        -> PersistenceResult<Page<ExecutionSummary>>;
    async fn executions_of_plan(&self, plan: &ExecutionPlanId, page: PageRequest)
        -> PersistenceResult<Page<ExecutionSummary>>;

    // -- Convenience ------------------------------------------------
    async fn latest_execution_of_plan(&self, plan: &ExecutionPlanId)
        -> PersistenceResult<Option<ExecutionSummary>>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanFilter {
    pub source_plan_fingerprint: Option<Fingerprint>,
    pub name_contains:           Option<String>,
    pub compiled_after:          Option<jiff::Timestamp>,
    pub compiled_before:         Option<jiff::Timestamp>,
    pub has_label:               Vec<(LabelKey, LabelValue)>,
    pub has_tag:                 Vec<(TagKey, TagValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionFilter {
    pub plan_fingerprint: Option<Fingerprint>,
    pub status:           Option<ExecutionStatus>,
    pub started_after:    Option<jiff::Timestamp>,
    pub started_before:   Option<jiff::Timestamp>,
    pub has_label:        Vec<(LabelKey, LabelValue)>,
    pub has_tag:          Vec<(TagKey, TagValue)>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionPlanSummary {
    pub id:                       ExecutionPlanId,
    pub source_plan_fingerprint:  Fingerprint,
    pub compiled_at:              jiff::Timestamp,
    pub trial_count:              u32,
    pub step_count:               u32,
    pub labels:                   Labels,
    pub tags:                     Tags,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutionSummary {
    pub id:               ExecutionId,
    pub plan_fingerprint: Fingerprint,
    pub final_status:     ExecutionStatus,
    pub started_at:       jiff::Timestamp,
    pub completed_at:     Option<jiff::Timestamp>,
    pub successful_trials: u32,
    pub failed_trials:     u32,
    pub skipped_trials:    u32,
    pub total_trials:      u32,
}
```

`ExecutionPlanSummary` and `ExecutionSummary` are the lightweight
browsing records. `ExecutionPlan` (SRD-0009) and
`ExecutionResults` (SRD-0011) are the heavy ones; get them via
the plain `get_*` methods.

Saving a plan or execution is idempotent on fingerprint/id: a
second save of the same logical record is a no-op, not a conflict.

### `JournalStore`

```rust
#[async_trait::async_trait]
pub trait JournalStore: Send + Sync + 'static {
    async fn append (&self, event: &JournalEvent) -> PersistenceResult<()>;
    async fn append_batch(&self, events: &[JournalEvent]) -> PersistenceResult<()>;

    /// Replay events from `after_sequence` forward (exclusive).
    /// If `after_sequence` is `None`, yields from the start.
    async fn replay(
        &self,
        execution:      &ExecutionId,
        after_sequence: Option<JournalSequence>,
    ) -> PersistenceStream<JournalEvent>;

    async fn latest_event   (&self, execution: &ExecutionId) -> PersistenceResult<Option<JournalEvent>>;
    async fn latest_sequence(&self, execution: &ExecutionId) -> PersistenceResult<JournalSequence>;

    /// Drop events with `sequence < before` for the given
    /// execution — use for journal compaction after a checkpoint.
    /// Callers are responsible for ensuring the checkpoint
    /// references a sequence ≥ before.
    async fn truncate_before(&self, execution: &ExecutionId, before: JournalSequence)
        -> PersistenceResult<()>;

    async fn delete_all     (&self, execution: &ExecutionId) -> PersistenceResult<()>;

    /// Observe new events as they land. Live tailing for UIs and
    /// event-driven subscribers.
    async fn subscribe      (&self, execution: &ExecutionId) -> PersistenceStream<JournalEvent>;
}
```

Append is monotonic per execution: each successful `append`
commits the event's sequence number and makes it visible to
concurrent `replay` / `subscribe` callers. `append_batch` is
provided as a throughput optimisation — the backend writes the
batch atomically where possible.

`subscribe` returns a live stream that yields appended events
from the current sequence forward. Backends implement it via
pub/sub, channel, or polling depending on capability. The stream
closes when the caller drops it.

### `MetadataStore`

```rust
#[async_trait::async_trait]
pub trait MetadataStore: Send + Sync + 'static {
    async fn save_test_plan_metadata    (&self, fp: &Fingerprint, meta: &TestPlanMetadata) -> PersistenceResult<()>;
    async fn get_test_plan_metadata     (&self, fp: &Fingerprint) -> PersistenceResult<Option<TestPlanMetadata>>;
    async fn list_test_plans            (&self, page: PageRequest) -> PersistenceResult<Page<(Fingerprint, TestPlanMetadata)>>;

    async fn save_execution_plan_metadata(&self, id: &ExecutionPlanId, meta: &ExecutionPlanMetadata) -> PersistenceResult<()>;
    async fn get_execution_plan_metadata (&self, id: &ExecutionPlanId) -> PersistenceResult<Option<ExecutionPlanMetadata>>;
    async fn list_execution_plans        (&self, page: PageRequest) -> PersistenceResult<Page<(ExecutionPlanId, ExecutionPlanMetadata)>>;

    /// Free-form tag/label updates. Useful for post-hoc
    /// annotation without re-saving the whole plan/result.
    async fn set_labels(&self, scope: &MetadataScope, labels: &Labels) -> PersistenceResult<()>;
    async fn set_tags  (&self, scope: &MetadataScope, tags:   &Tags)   -> PersistenceResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetadataScope {
    TestPlan       { fingerprint: Fingerprint },
    ExecutionPlan  { id: ExecutionPlanId },
    Execution      { id: ExecutionId },
    Trial          { id: TrialId },
    Artifact       { id: ArtifactId },
}
```

The metadata store is the catalog layer: "what plans exist, what
has been run, how is each one labelled." It never holds the
heavy plan/result contents — those live in their dedicated
stores. Calls that set labels / tags after creation route all
scopes through a single `set_labels` / `set_tags` pair.

### `ResultStore`

```rust
#[async_trait::async_trait]
pub trait ResultStore: Send + Sync + 'static {
    async fn save  (&self, result: &TrialResult)   -> PersistenceResult<()>;
    async fn get   (&self, trial:  &TrialId)       -> PersistenceResult<Option<TrialResult>>;
    async fn delete(&self, trial:  &TrialId)       -> PersistenceResult<()>;

    async fn query (&self, filter: &ResultFilter, page: PageRequest)
        -> PersistenceResult<Page<TrialResult>>;
    async fn stream(&self, filter: &ResultFilter)
        -> PersistenceStream<TrialResult>;
    async fn count (&self, filter: &ResultFilter) -> PersistenceResult<u64>;

    async fn aggregate(&self, filter: &ResultFilter, agg: &Aggregation)
        -> PersistenceResult<AggregateResult>;
}
```

#### `ResultFilter`

A serde-able declarative filter AST — no closures, no string DSL:

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResultFilter {
    // -- leaf predicates --------------------------------------------
    Any,
    TrialId                 { id:  TrialId },
    ExecutionId             { id:  ExecutionId },
    PlanFingerprint         { fp:  Fingerprint },
    Status                  { status: TrialStatus },
    StatusIn                { statuses: BTreeSet<TrialStatus> },
    StartedAfter            { ts: jiff::Timestamp },
    StartedBefore           { ts: jiff::Timestamp },
    AttemptNumber           { cmp: Comparison, value: u32 },

    /// Metric threshold on a specific (element, result_parameter)
    /// coordinate. Value type must match the parameter's kind.
    Metric                  { coord: ElementParameterRef, cmp: Comparison, value: Value },

    /// Assignment-side filter: "trials where element.param was X".
    Assignment              { coord: ElementParameterRef, value: Value },

    /// Trial code match (exact or prefix).
    TrialCode               { pattern: TrialCodePattern },

    LabelEquals             { key: LabelKey, value: LabelValue },
    TagEquals               { key: TagKey,   value: TagValue },

    // -- combinators ------------------------------------------------
    And(Vec<ResultFilter>),
    Or (Vec<ResultFilter>),
    Not(Box<ResultFilter>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison { Eq, Ne, Lt, Le, Gt, Ge }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrialCodePattern {
    Exact  { code: String },
    Prefix { prefix: String },
}

impl ResultFilter {
    // combinator constructors mirroring the Constraint algebra from SRD-0004
    pub fn and(self, other: ResultFilter) -> Self;
    pub fn or (self, other: ResultFilter) -> Self;
    pub fn not(self)                       -> Self;
}
```

Construction is by ordinary enum/struct literals plus the
fluent combinators. Backends are expected to turn `ResultFilter`
into native queries (SQL `WHERE` clauses in the SQLite case); the
tree is structured enough that pushdown is mechanical.

A backend may reject a filter it can't support efficiently via
`Error::InvalidQuery`. That's rare — the filter language is
narrow by construction.

#### Aggregation

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Aggregation {
    Count,
    Min   { metric: ElementParameterRef },
    Max   { metric: ElementParameterRef },
    Sum   { metric: ElementParameterRef },
    Avg   { metric: ElementParameterRef },
    Percentile { metric: ElementParameterRef, p: f64 },   // 0.0..=1.0
    GroupBy { dimension: GroupDimension, then: Box<Aggregation> },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupDimension {
    Assignment { coord: ElementParameterRef },
    Status,
    TrialCodeDigit { position: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AggregateResult {
    Scalar  { value: Value },
    Count   { n: u64 },
    Grouped { groups: BTreeMap<Value, Box<AggregateResult>> },
}
```

Aggregation is shallow: scalar + grouped-by-one-dimension. Users
wanting richer analytics export with `stream(...)` and handle it
downstream. The built-in aggregation covers the common "what was
the median latency at threads=16?" style query without pulling
every row into memory.

### Retention policies

Per-store retention hooks:

- `ArtifactStore::run_retention(policy)` — see `RetentionPolicy`
  above. Runs on demand; scheduling is the caller's
  responsibility.
- `CheckpointStore::prune(execution, keep_newest)` — keep N
  latest per execution, drop the rest.
- `JournalStore::truncate_before(execution, sequence)` — drop
  events before a known-safe watermark (typically the sequence
  embedded in the most recent checkpoint).
- Other stores have no automatic retention; hyperplane's catalog
  maintenance orchestrates periodic cleanup as its own concern.

## Decisions

- **D1.** Six persistence traits: `ArtifactStore`,
  `CheckpointStore`, `ExecutionRepository`, `JournalStore`,
  `MetadataStore`, `ResultStore`. One shared
  `PersistenceError` / `PersistenceResult<T>`.
- **D2.** All trait methods are async (tokio). Streaming
  accessors use `BoxStream`. Per-method atomicity is
  guaranteed; cross-method atomicity is a backend concern not
  exposed on the trait surface.
- **D3.** Pagination is cursor-based via opaque `PageToken`; no
  offset/limit pair on list methods. Backends clamp `PageRequest::size`
  to a hard maximum.
- **D4.** `ResultFilter` is a serde-able declarative enum with
  `And`/`Or`/`Not` combinators and typed leaves (`Metric`,
  `Assignment`, `Status`, `TrialCode`, `LabelEquals`, etc.). No
  string DSL, no closures.
- **D5.** `Aggregation` supports `Count`, `Min`, `Max`, `Sum`,
  `Avg`, `Percentile`, and single-dimension `GroupBy`. Deeper
  analytics are handled client-side via `stream(...)`.
- **D6.** `ExecutionRepository` stores both plans and execution
  results in one trait; `plan → executions` navigation is a
  first-class method.
- **D7.** `MetadataStore` is the catalog layer. It stores
  lightweight metadata records (`TestPlanMetadata`,
  `ExecutionPlanMetadata`) and exposes scoped label/tag updates
  through a single `MetadataScope` enum.
- **D8.** `JournalStore::subscribe` provides live event tailing.
  Implementations choose the delivery mechanism (pub/sub,
  channel, polling); the trait pins only the stream shape.
- **D9.** `SaveMode { CreateNew | Replace | Version }` on
  `ArtifactStore::save` controls how duplicates are handled.
  Upstream is silent; we make the behaviour explicit.
- **D10.** `ArtifactRecord` carries an optional `sha256` field
  the backend may compute for integrity verification. Upstream
  doesn't require this; Rust backends that compute it anyway
  surface it for consumers.
- **D11.** Schema versioning is backend-managed. Paramodel
  traits are version-agnostic; schema migrations are the
  hyperplane SQLite SRD's concern.
- **D12.** Idempotent saves: `save_plan`, `save_execution`,
  `save_test_plan_metadata`, `save_execution_plan_metadata`, and
  `save` on `ResultStore` treat a repeat save of the same
  logical key as a no-op. Conflict is only reported when two
  different payloads share the same key (a bug on the caller's
  side).
- **D13.** `ArtifactStore::run_retention`, `CheckpointStore::prune`,
  `JournalStore::truncate_before` are opt-in; paramodel does not
  schedule retention itself. The hyperplane layer runs them on
  a timer.
- **D14.** `PersistenceError` has specific variants for
  `NotFound`, `Conflict`, `VersionMismatch`, `Serialization`,
  `Backend`, `Retention`, `InvalidQuery`. Backend-specific
  detail travels as a boxed source error on `Backend` /
  `Serialization`.
- **D15.** `PersistenceStream<T>` is owned and `'static` —
  `BoxStream<'static, PersistenceResult<T>>`. Backends move any
  shared state (typically `Arc<Pool>`) into the stream before
  pinning. This keeps streams freely composable, spawnable onto
  tokio tasks, and free of lifetime surgery at call sites.
- **D16.** `ResultStore` keeps both `query(filter, page) ->
  Page<TrialResult>` and `stream(filter) -> Stream<TrialResult>`.
  `query` is the hot path for UI browsing (backend applies
  `LIMIT`); `stream` is the unbounded path for analytics
  pipelines that consume the whole filtered set.

