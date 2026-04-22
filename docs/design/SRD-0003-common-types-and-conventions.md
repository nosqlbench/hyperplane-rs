<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0003 — Common Types and Conventions

## Purpose

Pin the shared foundations every paramodel and hyperplane crate relies on,
so per-aspect SRDs don't re-litigate them. Covers toolchain, workspace
skeleton, error model, ownership defaults, time types, ID newtypes,
content-hash policy, serde policy, logging, async runtime, license
headers, and the unsafe policy.

This SRD locks in the items called out in SRD-0002 §6 (R8, R9, R10, R17,
R24, R25) and adds the specific crate choices that follow from them.

## Scope

In scope:

- Rust toolchain and edition.
- Workspace layout (`Cargo.toml`, shared dependencies, shared lints).
- Error model and panic policy.
- Default ownership / mutability posture.
- Time and duration types.
- Name and ID newtypes (general pattern — specific IDs are defined in the
  SRDs that own the domain).
- Content-hash / fingerprint algorithm.
- Serde and wire-format conventions.
- Logging.
- Async runtime and async-trait posture.
- License header templates (Rust, Markdown, TOML).
- Unsafe policy.

Out of scope:

- Specific domain types (parameters, plan, executor, agent protocol) —
  they are defined by their own SRDs.
- Persistence wire format details — defined by the persistence SRD; this
  SRD pins only the serde defaults.
- Crate-level public API — defined per crate.

## Background

SRD-0002 resolved several direction-setting questions but deferred the
concrete crate picks to this SRD. The choices below prefer modern,
actively-maintained Rust crates; nothing is drawn from upstream Java
convention.

## Design

### Toolchain

- Edition: **2024**.
- Toolchain: **nightly**, pinned via a workspace-root `rust-toolchain.toml`:

  ```toml
  [toolchain]
  channel = "nightly"
  components = ["rustfmt", "clippy", "rust-src"]
  profile = "default"
  ```

  The `channel` line may be pinned to a specific nightly date (e.g.
  `"nightly-2026-04-15"`) when a particular feature requires it. Default
  is the unpinned `"nightly"` string; we advance it deliberately.
- No MSRV commitment. We track nightly.

### Workspace layout

Top-level `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members  = ["crates/*"]

[workspace.package]
edition    = "2024"
license    = "Apache-2.0"
repository = "https://github.com/jshook/hyperplane-rs"
authors    = ["Jonathan Shook"]

[workspace.dependencies]
# Shared across crates. Per-crate Cargo.toml writes `foo.workspace = true`.
tokio             = { version = "1", features = ["full"] }
serde             = { version = "1", features = ["derive"] }
serde_json        = "1"
thiserror         = "2"
anyhow            = "1"
tracing           = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
jiff              = { version = "0.1", features = ["serde"] }
blake3            = "1"
bon               = "3"
ulid              = { version = "1", features = ["serde"] }

[workspace.lints.rust]
unsafe_code         = "forbid"
missing_debug_implementations = "warn"
rust_2024_compatibility = "warn"

[workspace.lints.clippy]
pedantic      = { level = "warn", priority = -1 }
nursery       = { level = "warn", priority = -1 }
# Selected opt-outs; we'll revisit in practice.
module_name_repetitions = "allow"
missing_errors_doc      = "allow"
missing_panics_doc      = "allow"
```

Each crate's `Cargo.toml`:

```toml
[package]
name        = "paramodel-elements"
version     = "0.1.0"
edition.workspace     = true
license.workspace     = true
repository.workspace  = true
authors.workspace     = true

[dependencies]
serde.workspace     = true
thiserror.workspace = true
# ...

[lints]
workspace = true
```

Rationale for workspace-level shared deps + lints: every crate pulls the
same versions and the same lint baseline without repeating itself.

### Error model (R8)

- Library crates define one error type per crate by default, using
  `thiserror::Error` on a `pub enum Error { ... }` in `src/error.rs`.
  Public APIs return `Result<T, Error>` with `pub type Result<T> =
  std::result::Result<T, Error>;`.
- If a crate's error space grows large enough, the crate may split into
  per-subsystem error enums, wrapped from the top-level `Error` with
  `#[from]`. That split is announced in the crate's SRD.
- Upstream errors are wrapped, not re-thrown. `#[source]` + `#[from]`
  preserve chains.
- `anyhow` is allowed only in binary / CLI crates and in tests. No
  `anyhow::Error` in any `paramodel-*` or library-layer `hyperplane-*`
  public API.
- `panic!`, `unwrap`, `expect` are reserved for invariant violations that
  indicate a bug. Each such call site names the violated invariant in its
  message.

Example (shape only):

```rust
// crates/paramodel-elements/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("parameter '{name}' value is out of domain: {reason}")]
    OutOfDomain { name: String, reason: String },

    #[error("constraint violated: {0}")]
    ConstraintViolated(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

### Ownership (R9)

- Public types are owned, immutable values. No `&mut self` methods on
  domain types except inside explicit builder or state-machine types.
- Mutation lives in dedicated builders. Default pattern: `Foo` is
  immutable; `FooBuilder` is mutable; `FooBuilder::build(self) -> Foo`
  is the one-way transition. We use the `bon` crate (opt-in, derive-
  based) for boilerplate-free builders:

  ```rust
  use bon::Builder;

  #[derive(Debug, Clone, PartialEq, Eq, Builder)]
  pub struct TestPlan {
      name: PlanName,
      axes: Vec<Axis>,
      // ...
  }
  ```

  The derive generates `TestPlan::builder() -> TestPlanBuilder<_>`. Per-
  crate SRDs choose whether to expose the derived builder directly or
  wrap it with a hand-rolled facade.
- No `Arc<Mutex<…>>` in paramodel-layer library code. If shared mutable
  state is genuinely required (executor, runtime), the owning SRD names
  it and justifies it.
- Function signatures prefer `&T` over `Arc<T>` unless ownership transfer
  or shared-across-threads retention is intentional.

### Time types (R10)

- Elapsed time: `std::time::Duration`.
- Wall-clock timestamps: `jiff::Timestamp` (UTC instants) and
  `jiff::Zoned` only where timezone awareness is required (user-facing
  output). Rationale: sound around leap-seconds and DST, modern, serde
  support out of the box, no `chrono` legacy footguns.
- JSON serialisation of `Timestamp` is the default ISO-8601 string
  (`2026-04-20T18:00:00Z`). Persistence may override to a nanosecond
  integer for compactness; decided in the persistence SRD.

### Names and IDs

A uniform newtype pattern:

```rust
// Human-authored names: validated string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ParameterName(String);

impl ParameterName {
    pub fn new(s: impl Into<String>) -> Result<Self> { ... }
    pub fn as_str(&self) -> &str { &self.0 }
}

impl std::fmt::Display for ParameterName { ... }
```

Rules:

- **No bare `String`** for domain IDs in public APIs.
- Newtypes derive `Debug, Clone, PartialEq, Eq, Hash, Serialize,
  Deserialize`. `Display` is implemented where the value is user-visible.
- Human-authored names validate in their constructor: non-empty, UTF-8,
  no control characters, and (for identifier-like names) match the
  regex `[A-Za-z_][A-Za-z0-9_]*`. Each name type's SRD pins its
  validation rule.
- Machine-generated IDs use:
  - `ulid::Ulid` wrapped in a newtype — for ordered, unique identifiers
    like `TrialId`, `StepId`, `JobId`. Lexicographically sortable by
    creation time, URL-safe.
  - `Fingerprint` (§ Content hashes) — for content-addressed identifiers
    like `PlanFingerprint`, `ElementFingerprint`.

Per-crate SRDs enumerate their ID types; this SRD just pins the pattern.

### Content hashes / fingerprints

- Algorithm: **BLAKE3**. Rationale: faster than SHA-2, cryptographically
  strong, parallelisable, stable spec, one widely-used Rust crate
  (`blake3`).
- Speed context (public benchmarks, modern x86-64):
  - BLAKE3: ~1–3 GB/s single-threaded; ~6–10 GB/s multi-threaded with
    internal parallelism on a few cores.
  - SHA-256 with hardware SHA-NI: ~1–2 GB/s single-threaded; not
    parallelisable within a single hash.
  - SHA-256 software (no SHA-NI): ~350–500 MB/s.
  At our workload scale (fingerprinting plans, trial assignments, small
  artefact headers) both comfortably exceed any practical throughput
  need. BLAKE3 is chosen primarily for the consistent story across
  CPUs (it doesn't rely on a SHA-NI branch that's absent on older or
  embedded targets) and for the pleasant API of the `blake3` crate.
  Override to SHA-256 if interop with an external system requires it.
- Output: full 32-byte digest stored internally.
- A single `Fingerprint` newtype lives in `paramodel-elements::fingerprint`:

  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Hash)]
  pub struct Fingerprint([u8; 32]);

  impl Fingerprint {
      pub fn of<T: FingerprintInput>(value: &T) -> Self { ... }
      pub fn as_bytes(&self) -> &[u8; 32] { &self.0 }
      pub fn display_short(&self) -> DisplayShort<'_> { ... } // first 12 hex chars
  }

  impl std::fmt::Display for Fingerprint { /* lowercase hex, 64 chars */ }
  impl std::fmt::Debug   for Fingerprint { /* "b3:" + display_short */ }
  ```

- User-facing rendering prefix: `b3:` followed by the first 12 hex
  characters (`b3:a4f7c203d891`). Internal equality and persistence use
  the full 32 bytes.
- Inputs are canonicalised before hashing. `FingerprintInput` is a trait
  that writes a canonical byte stream; ordinary types implement it by
  delegating to a documented canonical serializer (not general serde —
  serde order is not stable across versions). The exact canonical form
  for each domain type is defined in that domain's SRD.

### Serde

- All `paramodel-*` public data types derive `serde::Serialize` +
  `serde::Deserialize` by default.
- Default wire format: JSON.
- Field naming: Rust `snake_case` on the wire too. No global renaming
  unless an external contract demands it.
- Persistence types use `#[serde(deny_unknown_fields)]`. API
  request/response types on the HTTP surface are permissive by default;
  each HTTP SRD may tighten.
- Versioning: only types that cross release boundaries (persistence
  records, wire-protocol messages) carry an explicit `schema_version`
  field. Internal DTOs don't.
- No `Arc`-wrapped fields on serde-visible types.

### Logging

All log output is structured **JSONL** — one JSON object per line,
serialised from typed event structs built with `bon`. The discipline is
that a log line reads with the same shape and field names as the struct
that produced it, so an operator looking at a line can read it as if it
were the Rust value.

- Logging crate: **`tracing`** only. No direct use of `log`.
- All binaries install `tracing-subscriber` with
  `tracing_subscriber::fmt::layer().json()` plus
  `EnvFilter::from_default_env()` (`RUST_LOG` compatible). Output goes to
  stderr. No text-on-TTY special case; JSONL everywhere.
- Library crates emit events; they do **not** install a subscriber.
- Domain events are typed structs in their owning crate, with `#[derive(
  Debug, Clone, serde::Serialize, bon::Builder)]`. Each event carries
  exactly the fields the operator wants to see on the JSONL line; no
  free-form `format!` bodies.
- A small `log-event` helper crate (name to be finalised in its own
  SRD; provisional `paramodel-log`) exposes:

  ```rust
  pub trait Event: serde::Serialize {
      const LEVEL: tracing::Level;
      const NAME:  &'static str;
  }

  pub fn emit<E: Event>(e: &E) {
      // Serialises `e` as a flat JSON object and hands it to tracing
      // as a single structured event under `NAME` at `LEVEL`, so the
      // fmt::json layer renders one JSONL row with those fields.
  }
  ```

  Usage:

  ```rust
  use bon::Builder;

  #[derive(Debug, Clone, serde::Serialize, Builder)]
  pub struct TrialStarted {
      trial_id: TrialId,
      attempt:  u32,
      element:  ElementName,
  }

  impl Event for TrialStarted {
      const LEVEL: tracing::Level = tracing::Level::INFO;
      const NAME:  &'static str   = "trial_started";
  }

  emit(&TrialStarted::builder()
      .trial_id(trial.id().clone())
      .attempt(1)
      .element(element_name.clone())
      .build());
  ```

  On the wire:

  ```json
  {"timestamp":"2026-04-20T18:00:00Z","level":"INFO","event":"trial_started","trial_id":"01JAX…","attempt":1,"element":"jvector"}
  ```

- Ad-hoc `tracing::info!` / `tracing::debug!` macro calls are allowed
  for low-frequency diagnostics, but any event that shows up in normal
  operation (trial lifecycle, command dispatch, node state transitions,
  etc.) goes through a typed `Event` struct.
- `#[tracing::instrument(skip_all)]` is used on public async functions
  only where a span is genuinely useful; don't instrument every function
  reflexively. Spans are serialised by the JSON formatter as
  context fields on their child events.

### Async runtime (R17)

- Runtime: **`tokio`**. Multi-threaded runtime by default in binaries.
  Tests may use `#[tokio::test]` or `#[tokio::test(flavor = "current_thread")]`
  as appropriate.
- Async trait methods use native 2024-edition `async fn` in traits. No
  `async-trait` crate except when the trait genuinely needs to be
  object-safe over async methods; that case is called out in the owning
  SRD.
- Library crates do not spawn background tasks against an implicit
  runtime. Any task spawn is done through an explicit handle passed in
  by the caller.

### License headers

Rust (`.rs`) — first two lines of every file:

```rust
// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0
```

Markdown (`.md`), TOML (`.toml` in non-dependency contexts), SQL, YAML,
Dockerfile — HTML-comment or `#`-comment form:

```
<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->
```

`Cargo.toml` relies on the workspace `license` field rather than a
comment header.

Additional contributors add their own `Copyright (c) Name` line
immediately below the existing one when they add or materially modify a
file.

### Unsafe policy

- `#![forbid(unsafe_code)]` at every library crate root by default.
  Inherited via `[workspace.lints.rust] unsafe_code = "forbid"` above
  (which sets the deny level workspace-wide). Crates that legitimately
  need `unsafe` override at the crate root with an explicit
  `#![allow(unsafe_code)]` and call it out in their own SRD, with the
  specific module that contains the `unsafe` named.
- Binary crates inherit the same default. They may relax only if the
  underlying library they wrap is itself unsafe; that is rare and is
  documented at the call site.

## Decisions

- **D1.** Edition 2024 on nightly (R25). Toolchain pinned in
  `rust-toolchain.toml`.
- **D2.** Workspace `Cargo.toml` owns shared dependency versions and
  shared lint configuration; crates inherit via `.workspace = true`.
- **D3.** Error model: `thiserror`-based per-crate enums with a local
  `Result<T>` alias. `anyhow` at binary / test edges only. No `panic!`
  on recoverable failures.
- **D4.** Ownership default: owned immutable values; `bon` derive for
  builders; no `Arc<Mutex<…>>` in paramodel library code.
- **D5.** Time types: `std::time::Duration` for elapsed,
  `jiff::Timestamp` for wall-clock.
- **D6.** IDs are newtypes. Human names are validated in constructors.
  Machine IDs use `ulid::Ulid` (ordered) or `Fingerprint` (content-
  addressed) wrapped in newtypes.
- **D7.** Content hashes use BLAKE3. Full 32-byte digest stored; `b3:` +
  first 12 hex chars for user-facing short form.
- **D8.** serde: `Serialize` + `Deserialize` derived on public paramodel
  types by default. JSON default wire format. `snake_case` field names.
  Persistence types are `deny_unknown_fields`.
- **D9.** Logging is JSONL everywhere via `tracing` + the JSON formatter
  of `tracing-subscriber`. Domain events are typed `Serialize` +
  `bon::Builder` structs emitted through a typed `Event` helper so
  each line reads with the same shape as the struct that produced it.
  Library crates don't install subscribers; binaries install one at
  startup with `EnvFilter` honouring `RUST_LOG`.
- **D10.** Async: `tokio` multi-threaded runtime; native 2024 async-fn-
  in-trait; library crates don't implicitly spawn.
- **D11.** License headers: `// Copyright (c) Jonathan Shook` + SPDX on
  every `.rs` file; comment-form equivalents on non-Rust text files.
- **D12.** `#![forbid(unsafe_code)]` by default; exceptions are local,
  named, and documented in the owning SRD.

