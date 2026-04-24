<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0101 — System State Boundaries

## Purpose

Pin down precisely what state hyperplane owns versus what state
paramodel owns, with both surfaces living inside the controller
process per SRD-0100 D2 (`INV-CTL-SOLE-WRITER`). No distributed
storage story: one physical store, one writer, one controller per
hyperplane deployment. This SRD enumerates the table groups and
their owners so we don't end up with two subsystems recording the
same fact in two places.

**Division of labor (load-bearing principle).** Paramodel defines
the abstractions and the trait surfaces that persist them.
Hyperplane provides concrete implementations of those traits,
plus bespoke tables *only* for concerns paramodel has no
abstraction for. If a concept can be expressed through an existing
paramodel abstraction, it must be — new hyperplane-specific tables
are reserved for things paramodel genuinely has no concept of
(nodes, agent connections, provisioning requests, image digest
caches, etc.).

**Single-file, modular-code rule.** Both surfaces land in one
physical SQLite file. The code that talks to that file stays
modular — one module per trait impl, each owning its slice of the
schema. This matches the current `paramodel-store-sqlite`
structure.

## Scope

**In scope.**

- Paramodel-owned entities (per SRD-0012 persistence traits):
  catalogue by trait.
- Hyperplane-owned entities: catalogue by responsibility.
- Cross-reference graph — which hyperplane rows key into which
  paramodel rows.
- Cascade-delete policy — what goes when.
- Migration ownership — who writes schema changes, who runs them,
  how forward/backward compatibility is handled.
- Physical file layout — where SQLite lives, WAL policy.

**Out of scope.**

- Concrete per-table column schemas (belongs to the implementing
  SRD — e.g. SRD-0104 specifies the `nodes` table columns).
- Access-control policy (SRD-0114).
- Backup / restore operational playbook (SRD-0112).

## Depends on

- SRD-0100 (invariants: controller is sole writer).
- Paramodel SRD-0012 (persistence traits).

---

## State ownership at a glance

![State ownership in one SQLite file: paramodel-owned tables (studies, plans, executions, checkpoints, journal_events, trials, artifacts...) accessed by hyperplane only via SRD-0012 traits; bridge tables (trial_deployments, execution_nodes) owned by hyperplane with foreign keys into paramodel; hyperplane-owned tables (nodes, agents, deployments, images, users, roles, shares, audit, system events).](diagrams/SRD-0101/state-ownership.png)

**Reading rules:**

- Paramodel never references hyperplane tables (reverse FKs
  forbidden — paramodel doesn't know hyperplane exists).
- Hyperplane may reference paramodel rows through bridge
  tables (read-only on the paramodel side; hyperplane owns
  the bridge row itself).
- The concept test `INV-STATE-PARAMODEL-FIRST`: does a
  paramodel abstraction already cover this? If yes, it
  lives in paramodel. Only if no, a hyperplane table.

## D1 — Paramodel-owned entities

Paramodel's persistence traits own these surfaces. Hyperplane
provides the SQLite implementation by delegating to the
`paramodel-store-sqlite` crate — hyperplane does not define the
schema here, only depends on it.

| Trait | Stores | Representative tables (paramodel-owned) |
|---|---|---|
| `MetadataStore` | Study, plan, execution, element descriptors | `studies`, `plans`, `executions`, `elements`, `parameters` |
| `CheckpointStore` | Execution checkpoints for resume | `checkpoints` |
| `JournalStore` | Monotonic execution event log | `journal_events` |
| `ResultStore` | Trial outcomes, metric bindings | `trials`, `trial_metrics`, `trial_parameters` |
| `ArtifactStore` | Blob content (logs, outputs, captured files) | `artifacts`, `artifact_chunks` |
| `ExecutionRepository` | Live execution state snapshot | `execution_snapshots` |

**Rules.**

- Hyperplane code reaches these via the trait, never via raw SQL.
- A new paramodel entity lands in paramodel first (new trait or
  extended existing trait), then hyperplane consumes it — never
  the other way around.
- Hyperplane may add indices on paramodel-owned tables through
  `paramodel-store-sqlite`'s extension mechanism (specified by
  paramodel). Adding columns is a paramodel change.

## D2 — Hyperplane-owned entities

Concerns paramodel has no abstraction for. Each group is owned
by a specific hyperplane SRD that specifies the concrete column
set; this SRD lists only the responsibility boundary.

| Group | Owner SRD | Purpose |
|---|---|---|
| Nodes | SRD-0104 | Provisioned compute hosts, their lifecycle state, cloud-provider identifiers |
| Agent connections | SRD-0105 | `agents` table — one row per deployed agent with `auth_token_hash`, `agent_id`, `node_id`, registration state |
| Deployment jobs | SRD-0104/SRD-0105 | `deployments` — the SSH-deploy bookkeeping for each agent install |
| Provisioning requests | SRD-0104 | Long-running user requests for node capacity (especially EC2) |
| Image registrations | SRD-0103 | `images` — per-image-digest extracted `ParamSpace` cache keyed by digest |
| Image tag resolutions | SRD-0103 | `image_tag_cache` — short-TTL tag → digest mappings for the resolve-at-lookup flow |
| SSH key material | SRD-0112 | Deploy keys generated by the controller, mode-protected on disk |
| System configuration | SRD-0112 | Key-value config store, including feature flags |
| Users + groups | SRD-0114 | `users`, `groups`, `group_members` |
| Roles + tokens | SRD-0114 | `roles`, `api_tokens`, `token_scopes` |
| Resource shares | SRD-0114 | `resource_shares` — per-resource read/execute grants to users/groups/workspaces |
| Workspaces | SRD-0114 | `workspaces`, `workspace_members` |
| Audit journal | SRD-0114 | `audit_events` — security-relevant actions with full principal + request context |
| System events | SRD-0111 | `system_events` — hyperplane lifecycle/topology/configuration events (non-journal) |

**Rules.**

- Each group above is a bounded concern. Adding a new group
  requires a sibling SRD that justifies why paramodel does not
  already cover it.
- Every hyperplane table uses ULID primary keys (`TEXT` in
  SQLite) unless the ID is owned by an external system (e.g.
  EC2 instance IDs stored as `TEXT`).
- Every hyperplane table has `created_at` and `updated_at`
  (TEXT, RFC-3339 UTC). Rows are not silently mutated; writers
  bump `updated_at`.

## D3 — Cross-reference graph

Hyperplane rows may reference paramodel rows, and vice versa.
Direction of reference matters for cascade behaviour (D4) and
for enforcing the "no paramodel changes from hyperplane code
except via trait" rule.

**Hyperplane → paramodel references (permitted).**

- `system_events.execution_id` → `executions.id` (SRD-0111;
  correlate a topology event with an executing plan).
- `audit_events.subject_resource_id` → any paramodel table's
  primary key (SRD-0114; string-typed, not an enforced FK).
- `resource_shares.resource_kind` + `.resource_id` → a paramodel
  resource (plan, execution, result) by polymorphic pair.

**Paramodel → hyperplane references (forbidden).** Paramodel does
not know hyperplane exists. Any paramodel table that needed to
reference a hyperplane row (e.g. "which node did this trial run
on") is an indication the concept belongs in paramodel itself,
not in hyperplane — and should be raised as a paramodel SRD
request, not as a reverse foreign key.

**Bridge tables.** When hyperplane needs to link its own concerns
to paramodel entities without modifying paramodel schema, it uses
a bridge table owned by hyperplane:

| Bridge table | Links | Owner SRD |
|---|---|---|
| `trial_deployments` | `trials.id` (paramodel) ↔ container deployment (hyperplane-internal) | SRD-0106/SRD-0107 |
| `execution_nodes` | `executions.id` (paramodel) ↔ `nodes.id` (hyperplane) | SRD-0104 |

Bridge tables hold the reference on the hyperplane side only.
The paramodel-side FK (e.g. `trials.id`) is read-only from
hyperplane's perspective — accessed via the paramodel trait for
validation, but the bridge row itself is a hyperplane write.

**INV-STATE-PARAMODEL-FIRST.** Any persisted concept expressible
through an existing paramodel abstraction must live in paramodel,
not in a parallel hyperplane table. A hyperplane table shadowing
a paramodel concept is a violation detected by review; the
reviewer's escalation is to raise a paramodel SRD, not to
rubber-stamp the shadow.

## D4 — Cascade-delete policy

![Three cascade regimes: soft-delete (default; rows marked deleted, retained for audit — nodes, users, plans, executions, image registrations); hard-delete with cascade (on parent delete, transient children gone — node→agents+deployments, image→image_tag_cache, workspace→workspace_members; requires admin scope); never-deleted (append-only; retention window-based — journal_events, system_events, audit_events).](diagrams/SRD-0101/cascade-regimes.png)


Hyperplane entities follow three deletion regimes.

**Soft-delete (default for user-facing resources).** Most
hyperplane rows are marked deleted but retained, to preserve the
audit journal's referential integrity and allow operator
recovery. Applies to:

- Nodes (after termination; row stays, marked `terminated`).
- Users (after deactivation; SRD-0114).
- Plans/executions referenced by the audit journal.
- Image registrations (retained for digest history).

Soft-deleted rows are filtered from normal queries but returned
by explicit `include_deleted=true` endpoints for audit use.

**Hard-delete with cascade (for transient bookkeeping).** When a
parent row goes, its transient children go. Applies to:

- `deployments` cascades on `node` hard-delete.
- `agents` cascades on `node` hard-delete.
- `image_tag_cache` cascades on `image` delete.
- `workspace_members` cascades on `workspace` delete.

**Never-deleted (immutable history).** Append-only tables that
are never deleted individually; retention is window-based.
Applies to:

- `journal_events` (paramodel) — retention per paramodel policy.
- `system_events` — retention per SRD-0111.
- `audit_events` — retention per SRD-0114 (typically indefinite;
  legal/compliance concern).

**Cascade direction.** Paramodel rows never cascade-delete
hyperplane rows directly; the `paramodel-store-sqlite` layer
does not know about hyperplane tables. Hyperplane enforces its
own cascade policy in application code (on delete of a paramodel
entity, hyperplane's own consistency checker removes stale
bridge rows as a follow-up transaction).

**Hard-delete requires an admin scope.** Per SRD-0114, the
`resource:delete:hard` scope gates every hard-delete. Soft-delete
is the default; hard-delete is an opt-in admin path.

## D5 — Migration ownership

**Paramodel owns its schema.** Migrations for paramodel-owned
tables live in `paramodel-store-sqlite`. Hyperplane upgrades by
bumping the paramodel dependency; the paramodel crate carries
its own migration runner.

**Hyperplane owns its schema.** Hyperplane migrations live in a
`hyperplane-store` crate (internal to the controller). Each
migration is a versioned SQL script + a Rust verification pass.

**Run order on controller startup.**

1. `paramodel-store-sqlite` runs its migrations to bring
   paramodel tables to the current version.
2. `hyperplane-store` runs its migrations to bring hyperplane
   tables to the current version.
3. Controller performs a consistency check (bridge tables have
   no dangling references) and fails startup loudly if it
   finds one.

Both migration runners are re-entrant: running against an
already-current database is a no-op.

**Compatibility window.** A controller of version N must open a
database last written by version N-1 and run migrations forward.
Backward compatibility (version N writing a database version
N+1 will later open) is not required — upgrades are one-way. An
operator who rolls back the binary must also restore the
database from backup.

**No cross-crate migration.** A hyperplane migration does not
touch paramodel tables, and vice versa. A change that affects
both sides (e.g. a new bridge table referencing a paramodel
column) requires paramodel's migration to land first in a
released version, then hyperplane consumes it.

## D6 — Physical file layout

- **File.** Single SQLite file at the path defined by SRD-0112's
  config directory layout (default
  `$XDG_DATA_HOME/hyperplane/hyperplane.db`).
- **WAL mode.** `PRAGMA journal_mode=WAL` — concurrent reads
  alongside writes, safer crash semantics.
- **Foreign keys.** `PRAGMA foreign_keys=ON` — enforced for
  hyperplane-internal references. Paramodel-side FKs are
  configured by paramodel.
- **Single-writer.** Per `INV-CTL-SOLE-WRITER`, only the
  controller process holds a writable connection. Read
  connections for diagnostics are permitted (e.g. `sqlite3`
  CLI) but must use `PRAGMA query_only=1`.
- **Backup.** Documented in SRD-0112. A live-backup story uses
  SQLite's online backup API; operators may also take a filesystem
  snapshot with the controller stopped.

## D7 — New invariants

| Code | Invariant |
|---|---|
| `INV-STATE-PARAMODEL-FIRST` | A concept expressible via an existing paramodel abstraction lives in paramodel, not in a hyperplane shadow table. |
| `INV-STATE-SINGLE-FILE` | Paramodel + hyperplane share one physical SQLite file per deployment. |

These extend the SRD-0100 catalogue.

## Open questions

None. The original open questions (unified file vs separated
schemas; per-execution deployment manifest location; cascade
regime) are resolved in D6, D3, and D4 respectively.

## Reference material

- Paramodel SRD-0012 — persistence traits.
- `~/projects/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/storage/` —
  Java table layout. The Rust port drops the one-table-per-concern-class
  duplication (Java had separate DAO classes creating parallel
  tables); consolidated into the `paramodel-store-sqlite` pattern.
