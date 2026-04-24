<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0014 — Element Kind Registry & Bundling

## Purpose

SRD-0007 defines the trait shapes an adopter implements to
supply elements (`ElementTypeDescriptor`,
`ElementTypeDescriptorRegistry`, `ElementRuntime`,
`ElementRuntimeRegistry`, `ShutdownSemantics`). Each is a
necessary surface. What SRD-0007 leaves open — and every
adopter independently re-solves — is *how those surfaces are
bundled and wired into paramodel at startup*.

This SRD adds the missing layer: an `ElementKind<Ctx>`
bundling trait that packages a descriptor, a runtime builder,
and mandatory `ShutdownSemantics` as a single declaration;
`inventory`-based compile-time auto-registration of those
bundles; and the bridges that turn the walked collection into
the two registries paramodel already consumes. The
`ShutdownSemantics` declaration is mandatory at bundle time,
and the plan compiler rejects elements whose instance-level
`shutdown_semantics` disagrees with their kind's declared
value.

The bundling and registration mechanism is generic over an
adopter-supplied context type `Ctx` — paramodel has no concept
of "agent handle" or "artifact store," but every runtime needs
*some* ambient capabilities from its adopter, and parameterising
the trait over `Ctx` keeps paramodel pure while giving adopters
the typed context they need.

## Scope

**In scope.**

- `ElementKind<Ctx>` trait — the bundling declaration.
- `ElementKindEntry<Ctx>` — the `inventory::submit!` value
  type that carries a static reference to a kind singleton.
- Bridge from the walked `inventory` collection to the two
  paramodel registries (`ElementTypeDescriptorRegistry`,
  `ElementRuntimeRegistry`).
- Compile-time `ShutdownSemantics` validation — how the plan
  compiler cross-checks an element instance's
  `shutdown_semantics` against its kind's declared value.
- **Compatibility lives on implementations, not in the
  registry.** Element-to-element compatibility (who may depend
  on whom, in what relationship, with what plug/socket
  wiring) is determined by the metadata each implementation
  exposes through paramodel types (labels, tags, plugs,
  sockets, relationship declarations on `Dependency`), and is
  enforced by paramodel's existing algebra. The registry does
  not host a central compatibility matrix.
- The adopter-contract: what an adopter supplies (a `Ctx`
  type, a dependency on `inventory`, kind crates that
  `inventory::submit!` their bundles).
- New invariants.

**Out of scope.**

- Dynamic plugin loading (out-of-process or shared-library).
  A future SRD could add this; it would require a plugin ABI.
- The concrete kinds any specific adopter ships (hyperplane's
  four kinds live in SRD-0102).
- The `Ctx` shape any specific adopter uses
  (`HyperplaneRuntimeContext` is owned by SRD-0102).

## Depends on

- SRD-0007 (Elements and Relationships — the trait shapes this
  SRD bundles).
- SRD-0005 (Labels, Plugs, Sockets — compatibility metadata
  this SRD points at).
- SRD-0009 (AtomicStep & execution graph — the compile-time
  check added by D4 runs during plan compilation).
- `inventory` crate.

---

## Concepts at a glance

```
                    ┌─────────────────────────────────────┐
                    │   paramodel (this SRD)              │
                    │                                     │
                    │   ElementKind<Ctx> ← trait          │
                    │   ElementKindEntry<Ctx> ← wrapper   │
                    │   KindRegistry<Ctx> ← loader        │
                    │                                     │
                    │   adapts to paramodel's SRD-0007:   │
                    │   ElementTypeDescriptorRegistry     │
                    │   ElementRuntimeRegistry            │
                    └─────────────────────────────────────┘
                                    ↑
                            implements + submits
                                    │
                    ┌───────────────┴────────────────┐
                    │   Adopter crate (e.g.          │
                    │   hyperplane-elements-ec2)     │
                    │                                │
                    │   EC2NodeKind ┐                │
                    │   AgentKind ──┼── inventory::  │
                    │   ServiceDockerKind ─ submit!  │
                    │   CommandDockerKind ┘          │
                    └────────────────────────────────┘
```

Each kind bundles three declarations — descriptor, shutdown
semantics, runtime builder — and registers at compile time.
Paramodel walks the inventory at startup and builds the two
registries SRD-0007 defined.

## D1 — `ElementKind<Ctx>` trait

A single trait bundles the three per-kind declarations an
adopter would otherwise spread across its own wiring:

```rust
pub trait ElementKind<Ctx>: Send + Sync + 'static
where
    Ctx: Send + Sync + 'static,
{
    /// The paramodel descriptor for this kind (per SRD-0007).
    /// Supplies labels/tags expectations and the
    /// `provides_infrastructure` flag.
    fn descriptor(&self) -> ElementTypeDescriptor;

    /// The `ShutdownSemantics` every instance of this kind
    /// carries. Paramodel has no valid "unspecified" value —
    /// an element is either long-running (`Service`) or
    /// run-to-completion (`Command`), and reducto's Rule 1
    /// emits different step sequences accordingly.
    fn shutdown_semantics(&self) -> ShutdownSemantics;

    /// Construct a concrete runtime for a specific element
    /// instance. Called by the executor at step-dispatch time,
    /// after parameter resolution. `ctx` is the adopter-supplied
    /// ambient context (e.g. an agent handle, an artifact
    /// store handle, a cloud-API client).
    fn build_runtime(
        &self,
        element: &Element,
        ctx: &Ctx,
    ) -> Result<Arc<dyn ElementRuntime>>;
}
```

**What's deliberately not on this trait.**

- **No compatibility / relationship-rules method.** See D5.
  Compatibility is expressed through the descriptor, labels,
  tags, plugs, sockets, and declared dependencies on
  `Element` — paramodel's existing algebra. A method like
  `allowed_targets()` would create a second source of truth
  for rules already expressible by the primary shape.
- **No teardown hook.** `ElementRuntime::dematerialize` owns
  teardown; the kind is a factory, not a process supervisor.
- **No kind-level config.** Per-instance configuration goes on
  `Element`. Kind-level policy (e.g. default timeouts) belongs
  on the descriptor or is part of the runtime's internal
  behaviour.

**Why `Ctx` is generic.** Paramodel has no opinion on what
ambient capabilities a runtime needs. Making `Ctx` a type
parameter lets the adopter declare one (hyperplane: agent
handle + artifact store handle; a different adopter: whatever
they need) without paramodel hard-coding a trait object with
dyn-dispatch overhead or speculative methods.

## D2 — Auto-registration via `inventory`

Adopter crates register kinds at compile time using the
`inventory` crate. A registration looks like:

```rust
inventory::submit! {
    ElementKindEntry::new(&MyKind)
}
```

Where `ElementKindEntry<Ctx>` wraps a static reference:

```rust
pub struct ElementKindEntry<Ctx: 'static> {
    kind: &'static dyn ElementKind<Ctx>,
}

impl<Ctx: 'static> ElementKindEntry<Ctx> {
    pub const fn new(kind: &'static dyn ElementKind<Ctx>) -> Self {
        Self { kind }
    }

    pub fn kind(&self) -> &'static dyn ElementKind<Ctx> {
        self.kind
    }
}

inventory::collect!(ElementKindEntry<()>);
// Adopters additionally call inventory::collect! for their Ctx.
```

**Adopter-side collection.** `inventory::collect!` is called
per concrete `Ctx` in the adopter's crate. Paramodel collects
for `()` so the default (unit-context) case works without
adopter setup; adopters with a real context call
`inventory::collect!(ElementKindEntry<MyCtx>)` in their own
binary crate.

**No central enum, no manifest file, no dynamic loading.** The
seam is pure compile-time linkage. Adding a new kind is adding
a crate dependency in `Cargo.toml` and calling
`inventory::submit!` in that crate.

## D3 — Registry bridge

At process start (or on first registry access), paramodel walks
`inventory::iter::<ElementKindEntry<Ctx>>()` and builds two
views:

```rust
pub struct KindRegistry<Ctx: 'static> {
    by_type_id: BTreeMap<TypeId, &'static dyn ElementKind<Ctx>>,
}

impl<Ctx: Send + Sync + 'static> KindRegistry<Ctx> {
    pub fn load() -> Self { /* walks inventory */ }

    pub fn descriptors(&self) -> Vec<ElementTypeDescriptor> { /* ... */ }

    pub fn kind(&self, type_id: &TypeId)
        -> Option<&'static dyn ElementKind<Ctx>> { /* ... */ }
}
```

Paramodel then supplies two adapters:

- `KindDescriptorRegistry<Ctx>` implementing
  `ElementTypeDescriptorRegistry` — returns
  `kind.descriptor()` for each registered kind.
- `KindRuntimeRegistry<Ctx>` implementing
  `ElementRuntimeRegistry` — dispatches by the element's
  `type` label to the matching kind's `build_runtime(element, ctx)`.

The two adapters share the same underlying `KindRegistry<Ctx>`;
walking inventory happens once.

**Duplicate type ids.** Two kinds registering the same
`TypeId` is a configuration error. The registry flags it at
load time and fails startup — the adopter depends on two
crates exporting the same `TypeId`, which is not a silent-merge
scenario.

**Empty registry.** Legal. An adopter that registers no kinds
gets an empty registry. Plan-compile of a plan using elements
of any `TypeId` fails with `UnknownElementType`, which is the
same error a missing-kind produces today.

## D4 — `ShutdownSemantics` compile-time validation

Every `ElementKind<Ctx>` declares its `shutdown_semantics`.
Every `Element` instance also carries `shutdown_semantics` on
its own struct (per SRD-0007). The plan compiler, during its
existing type-validation pass (SRD-0010), adds this check:

> For each `Element`, look up its kind's declared
> `shutdown_semantics` via the registry. If the element's
> own value disagrees, emit compile error
> `KindShutdownSemanticsMismatch { element, kind_value,
> instance_value }`.

This prevents the accident where a plan author marks a
`Command`-lifecycle element as `Service` (or vice versa) and
reducto emits the wrong step sequence. The kind's declared
semantics is authoritative for the kind; plan-level overrides
are rejected at compile time.

**Why this belongs in paramodel, not in each adopter.** It's a
consistency check over primitives paramodel already owns
(`ShutdownSemantics`, `ElementTypeDescriptor`,
`ElementRuntimeRegistry`). No adopter-specific behaviour is
involved. Per SRD-0007's existing builder invariants, this
kind of cross-reference check is a plan-compile concern.

## D5 — Compatibility rides on implementations, not the registry

Element-to-element compatibility (who can depend on whom,
with what relationship, over what plug/socket wiring) is
expressed through the metadata each implementation exposes
via paramodel's existing primitives:

- **Labels + Tags** (SRD-0005) — classify elements; filters on
  `Dependency::target` + label predicates enforce "dependencies
  may only target elements bearing label X."
- **Plugs + Sockets** (SRD-0005) — named connection points with
  compatibility rules (plug on A matches socket on B by name +
  type). A Docker service's "deploys-onto" plug matches a
  node's "hosts" socket; the wiring is the compatibility.
- **`RelationshipType`** (SRD-0007) — the five-variant enum on
  `Dependency` captures *how* the relationship is managed
  (`Shared`, `Exclusive`, `Dedicated`, `Linear`, `Lifeline`).
  Each implementation declares which values are acceptable
  on its incoming and outgoing edges via the normal
  `Element::dependencies` shape.
- **`provides_infrastructure`** on `ElementTypeDescriptor` —
  the existing boolean that marks host-like elements. Adopters
  use it to distinguish "may be depended on as infrastructure"
  from "runs as a workload."

**Consequence: the registry SRD does not enumerate
compatibility.** There is no compatibility matrix living in
this SRD or in an adopter's kind-registry doc. Each kind
implementation declares its plugs, sockets, labels, and
acceptable-relationship set; paramodel's compilation pipeline
validates the Element Graph against those declarations. If a
new relationship between two kinds needs expressing, the
fix is on the kinds' metadata, not in a central table.

This is why the adopter-contract (D6) does not mention
compatibility — it's already handled by the primitives an
adopter uses to build elements.

## D6 — Adopter contract

An adopter wiring paramodel into their system supplies:

1. **A `Ctx` type.** The ambient capabilities every runtime
   needs. Can be `()` for tests or trivial adopters; in
   practice carries handles to adopter-side services (for
   hyperplane, an agent handle and an artifact-store handle —
   SRD-0102 owns the shape).
2. **A dependency on `inventory`.** Transitive through the
   registry adapter crate; no direct action required.
3. **Kind crates that register via `inventory::submit!`.** Each
   kind crate depends on paramodel, implements
   `ElementKind<Ctx>`, and calls `inventory::submit!` for a
   static singleton.
4. **A `inventory::collect!(ElementKindEntry<Ctx>)` in the
   final binary crate.** Standard inventory pattern — the
   binary crate declares the collection for the adopter's
   `Ctx`.
5. **A `KindRegistry<Ctx>::load()` call at startup.** Populates
   the two registry adapters paramodel consumes.

Adopters get from paramodel: `ElementKind<Ctx>` trait,
`ElementKindEntry<Ctx>` wrapper, `KindRegistry<Ctx>` loader,
the two registry adapters, the compile-time
`ShutdownSemantics` check.

Adopters do not get from paramodel: any opinion on how kinds
are grouped into crates, what the adopter's `Ctx` shape is,
or what the compatibility story is — those are metadata
decisions on the implementations.

## D7 — New invariants

| Code | Invariant |
|---|---|
| `INV-ELEMENT-KIND-OPEN` | The registration seam is the only way kinds plug in; no central enum. |
| `INV-ELEMENT-KIND-REGISTRATION` | Every kind reachable at runtime was registered via `inventory::submit!` in a compile-time-linked crate. |
| `INV-ELEMENT-KIND-SHUTDOWN-SEMANTICS` | Every kind declares its `ShutdownSemantics`; element instances whose own value disagrees with their kind's declared value are rejected at plan-compile time. |
| `INV-ELEMENT-KIND-TYPE-UNIQUE` | `TypeId` values are unique across all registered kinds; duplicate registrations fail startup. |

These are paramodel-tier invariants: every adopter gets them
by depending on paramodel, without re-asserting them in their
own SRDs.

## D8 — Open questions

None remaining.

## Reference material

- SRD-0007 — `ElementTypeDescriptor`, `ElementRuntime`,
  `ElementRuntimeRegistry`, `ShutdownSemantics` trait shapes
  this SRD bundles.
- SRD-0005 — labels, plugs, sockets; the compatibility
  primitives D5 points at.
- SRD-0010 — compilation pipeline; host of the new D4 check.
- `inventory` crate — the compile-time plugin-collection
  library this SRD relies on.
