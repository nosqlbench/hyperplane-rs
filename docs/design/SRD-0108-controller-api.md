<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0108 — Controller HTTP + WebSocket API

## Purpose

Specify the canonical HTTP + WebSocket surface the controller
exposes. Every client — CLI (SRD-0109), web server (SRD-0110),
external SDK, and agents (SRD-0105) — consumes this API. The API
is the contract; everything else is a client. If an action isn't
reachable here, it doesn't exist.

Per SRD-0100, the controller serves API protocols only. No htmx
rendering, no static assets, no browser-facing routes. The web
server owns the browser; the controller owns the data.

This SRD pins the auth model, the endpoint-group taxonomy, the
agent WebSocket handshake, the event-stream protocol, the error
model, and the versioning policy. It does not enumerate every
endpoint's request/response schema — that detail lives in a
machine-readable companion (D10) that clients can regenerate
from.

## Scope

**In scope.**

- **Three auth tiers** — bearer token (CLI/SDK), system API key
  (web server), `auth-token` (agent WebSocket).
- **User-facing endpoint groups** — the taxonomy (health, events,
  topology, nodes, agents, images, plans, executions, results,
  checkpoints, artifacts, metadata, provisioning, system,
  deployments).
- **Agent-facing endpoint** — a single long-lived WebSocket.
  Handshake, connection lifecycle. The message catalogue riding
  on it is in SRD-0105.
- **Event stream protocol** (`/api/v1/events/subscribe`) —
  semantics per SRD-0100 D10, filters, reconnect with `since`.
- **Log-stream endpoints** — per-container, per-cloudinit,
  per-deployment. All WebSocket.
- **Error-response schema** — typed codes, structured body.
- **Versioning** — `/api/v1/` URL prefix + deprecation policy.
- **Principal plumbing** — how the controller sees end-user
  identity behind every request (see D3 and SRD-0114).
- **Conformance harness** — TCK-style tests per endpoint.

**Out of scope.**

- CLI command tree (SRD-0109).
- Web UI rendering + BFF composition patterns (SRD-0110).
- Agent wire protocol internals — SRD-0105 owns the per-channel
  framing; this SRD owns the endpoint the agent connects to.
- Endpoint-by-endpoint schema detail — captured in the OpenAPI
  document (D10), not inlined here.
- Multi-user permission detail (SRD-0114).

## Depends on

- SRD-0100 (invariants — sole-writer, browser isolation, event
  semantics, parity).
- SRD-0101 (state boundaries — every write endpoint knows which
  store it hits).
- SRD-0105 (agent WebSocket message catalogue).
- SRD-0114 (principals, roles, token scopes).

---

## Auth tiers at a glance

![Three auth tiers against the Controller: CLI and SDK present Bearer &lt;token&gt; as the end-user principal on /api/v1/*; Web server presents Bearer &lt;system-api-key&gt; + X-On-Behalf-Of to impersonate an end-user; Agent presents auth-token on the WebSocket handshake at /agent/ws as the agent principal. INV-API-SINGLE-CRED and INV-PRINCIPAL-AT-BOUNDARY apply across the board.](diagrams/SRD-0108/auth-tiers.png)

## Endpoint taxonomy at a glance

```
  /api/v1/
  ├── health               unauthenticated
  ├── topology             aggregate view
  ├── nodes                node CRUD + logs
  ├── agents               agent directory + admin broadcast
  ├── provisioning         provisioning-request lifecycle
  ├── images               param-space extraction + validate
  ├── plans                TestPlan CRUD
  ├── executions           run plans, control lifecycle
  ├── results              ResultStore projection
  ├── checkpoints          CheckpointStore
  ├── artifacts            ArtifactStore
  ├── metadata             MetadataStore
  ├── events/subscribe     (WS) event stream
  ├── deployments          agent bootstrap bookkeeping
  └── system               config kv + services + bootstrap

  /agent/ws                (WS) single endpoint for every agent
```

Writes in every group emit events (`INV-WRITE-EMITS-EVENT`).
Schema detail lives in OpenAPI (D10).

## D1 — Auth tiers

Three mutually exclusive auth tiers. Every request on every
endpoint carries exactly one.

| Tier | Credential | Issued by | Principal | Scope |
|---|---|---|---|---|
| **Bearer token** | Opaque random string, `Authorization: Bearer <tok>` | `hyper login` or admin mint | End-user (or API client acting as an end-user) | Scoped set (SRD-0114 D5) |
| **System API key** | Long-lived opaque string, same `Bearer` header | Minted at `hyper system init` time | System account (D3) | Full scope minus admin token ops |
| **`auth-token`** | Opaque string, agent WebSocket handshake header | Per-agent, minted at SSH deploy | Agent-principal (bound to one `agent_id`) | Only the agent WebSocket endpoint |

**Transport.** TLS mandatory everywhere. Plain HTTP / WS is
rejected with `426 Upgrade Required` from an auxiliary HTTP
listener or simply not bound; operator choice.

**Tier mismatch.** A bearer token on the agent WebSocket endpoint
is rejected. An `auth-token` on any user-facing endpoint is
rejected. A system API key attempting admin-token operations is
rejected. Mismatches return `403 ForbiddenCredentialTier`.

**No cookies.** The controller has no session concept. Cookies
live only between the browser and the web server (SRD-0110).
When the web server proxies to the controller it uses the system
API key plus the impersonation header (D3). This is non-negotiable:
a cookie at the controller boundary would tie controller
authentication to browser-specific state, violating
`INV-BROWSER-BYPASS` by making the controller browser-aware.

**INV-API-SINGLE-CRED.** Every request carries exactly one
credential. Multi-credential requests (e.g. both a bearer token
and a system API key) are rejected `400 MalformedAuth`.

## D2 — URL layout + versioning

All endpoints live under `/api/v1/` except the agent WebSocket,
which lives at `/agent/ws` (a separate path so agent and user
traffic can be load-balanced / audited separately).

**Versioning policy.**

- `v1` is the current and only version. Breaking changes mint
  `v2` on a separate prefix; both serve in parallel during a
  deprecation window.
- **Additive changes** (new fields, new optional query params,
  new endpoints) are not version bumps. Clients must ignore
  unknown fields.
- **Breaking changes** (removed fields, type changes, changed
  semantics) require a new `v` prefix.
- **Deprecation.** A breaking change announces the `v1`
  deprecation with a minimum 3-month overlap window, a
  `Deprecation` HTTP header on every `v1` response after that
  announcement, and a `Sunset` header giving the end-of-service
  date. Java Hyperplane's unversioned `/api/` path is not
  carried forward — the Rust port starts versioned.

**URL conventions.**

- Plural resource nouns (`/nodes`, `/agents`, `/plans`), never
  `/node` or `/getNode`.
- Sub-resources nested (`/nodes/{id}/cloudinit/logs`,
  `/plans/{id}/executions`).
- Action endpoints (rare, for non-REST operations) use a verb
  suffix: `POST /api/v1/nodes/sync`, `POST /api/v1/provisioning/requests/{id}/register-nodes`.
- IDs in paths are ULIDs except where an external system owns
  the ID namespace (e.g. EC2 instance ids `i-xxx`).

## D3 — Principal plumbing

Controller sees *principals*, not just channels. Every
authenticated request carries a principal (end-user, agent, or
system account), not just the transport credential. End-user
identity is first-class at the controller boundary.

Three principal kinds (mirroring SRD-0114 D1):

- **End-user** — authenticated via a bearer token belonging to
  a specific `user_id`.
- **Agent** — authenticated via an `auth-token` bound to a
  specific `agent_id`.
- **System account** — authenticated via the system API key;
  represents the web server (or operator-script) acting on
  behalf of *someone*, who is carried in an impersonation header.

**Impersonation header.** When the web server proxies a browser
request to the controller, it sends:

```
Authorization: Bearer <system-api-key>
X-On-Behalf-Of: <user-id>
```

The controller validates the system API key, confirms the key is
authorised to impersonate, looks up the `user_id`, and runs the
request with the end-user as effective principal. SRD-0114 D11
owns the full rule.

**Why header, not token exchange.** Token-exchange (OAuth
on-behalf-of style) requires round-tripping the controller on
every browser request to mint a user-scoped token. An explicit
impersonation header short-circuits that, keeping the per-request
path at one hop. The system API key is the capability; the
header is the parameter.

**`RequestContext`.** Internally every handler sees a
`RequestContext { principal, effective_principal, active_roles,
token_scopes }` value. `principal` is who presented the
credential; `effective_principal` is who the controller treats
the request as coming from (differs only for impersonation).
Permission checks use `effective_principal`.

**INV-PRINCIPAL-AT-BOUNDARY.** No handler runs without a resolved
`RequestContext`. Anonymous access is not a supported state;
every request is either authenticated or 401-rejected before a
handler fires.

## D4 — User-facing endpoint groups

The taxonomy. One row per group; detail schemas live in the
OpenAPI document.

| Group | Prefix | Reads | Writes | Notes |
|---|---|---|---|---|
| Health | `/api/v1/health` | ✓ | — | Unauthenticated liveness probe; version + build hash |
| Topology | `/api/v1/topology` | ✓ | — | Snapshot of live nodes + agents + services; aggregation over other groups |
| Nodes | `/api/v1/nodes` | ✓ | ✓ | Node CRUD, cloud-init log link, status, sync |
| Agents | `/api/v1/agents` | ✓ | ✓ | Agent directory, container directory by agent, per-agent command dispatch (admin), broadcast |
| Provisioning | `/api/v1/provisioning` | ✓ | ✓ | Provisioning-request lifecycle; EC2-flavoured detail in SRD-0104 |
| Images | `/api/v1/images` | ✓ | ✓ | Image registration, dockerfile retrieval, param-space extraction (SRD-0103) |
| Plans | `/api/v1/plans` | ✓ | ✓ | TestPlan CRUD — paramodel `PlanSpec` |
| Executions | `/api/v1/executions` | ✓ | ✓ | Run plans; per-execution control (pause, resume, cancel) |
| Results | `/api/v1/results` | ✓ | — | Paramodel `ResultStore` projection |
| Checkpoints | `/api/v1/checkpoints` | ✓ | ✓ | Paramodel `CheckpointStore` |
| Artifacts | `/api/v1/artifacts` | ✓ | ✓ | Paramodel `ArtifactStore` |
| Metadata | `/api/v1/metadata` | ✓ | ✓ | Paramodel `MetadataStore` |
| Events | `/api/v1/events` | ✓ | — | Historic listing + typed-event query |
| System | `/api/v1/system` | ✓ | ✓ | Config kv, services, database info, console status, admin bootstrapping |
| Deployments | `/api/v1/deployments` | ✓ | ✓ | Deploy-job lifecycle (agent SSH-bootstrap bookkeeping, per SRD-0104/SRD-0105) |

**Taxonomy rules.**

- Every group that writes emits events (D7) — the event stream
  is the authoritative async projection of every write.
- Every group's reads are paginatable when unbounded. Pagination
  is cursor-based (opaque `next` token), not offset — offsets
  don't survive inserts cleanly.
- Every group that references a paramodel store (Results,
  Checkpoints, Artifacts, Metadata) is a thin shell over the
  paramodel trait — reshape for HTTP but preserve semantics.

**Stream endpoints inside groups.** Log streams live inside the
owning group, not in a flat `/logs` tree:

- `GET /api/v1/agents/{id}/containers/{name}/logs` (WebSocket) —
  live container logs via the managing agent.
- `GET /api/v1/nodes/{id}/cloudinit/logs` (WebSocket) — cloud-init
  log tail while provisioning.
- `GET /api/v1/deployments/{job}/log` (WebSocket) — deploy-job
  output tail.

WebSocket log endpoints close on EOF (container exit / cloud-init
complete / job complete) or on an explicit client close.

## D5 — Agent WebSocket endpoint

A single endpoint, consumed only by the agent binary, per
SRD-0105.

```
WS /agent/ws
```

**Handshake.** HTTP upgrade with:

- `Authorization: Bearer <auth-token>` — the token delivered over
  SSH deploy.
- `Hyperplane-Agent-Version: <semver>` — for pre-upgrade version
  gate.
- `Hyperplane-Protocol-Version: 1` — message envelope version.

Controller validates the token against the `agents` table. On
success, upgrades to WebSocket. On failure: `401
InvalidAuthToken`, `403 RevokedAuthToken`, `426 UnsupportedVersion`,
or `409 AgentAlreadyConnected` (if another live WebSocket exists
for this `agent_id`; the controller closes the old one with
`Superseded` before this new handshake is allowed to complete —
see SRD-0105 D9).

**Authentication is handshake-only.** Once the WebSocket is open
every message is authoritative by virtue of the connection.
Messages don't re-send the token. Token rotation (SRD-0105 D3)
runs *on* the authenticated WebSocket.

**Close codes.** Custom close codes extend the WebSocket standard
set:

| Code | Meaning |
|---|---|
| 4001 | `HeartbeatTimeout` — agent didn't heartbeat in 30s |
| 4002 | `Superseded` — a new connection from the same agent took over |
| 4003 | `RevokedAuthToken` — token was revoked |
| 4004 | `UnsupportedVersion` — protocol version mismatch detected post-handshake |
| 4005 | `ShutdownAck` — agent acknowledged `Shutdown` and is leaving |
| 4009 | `InternalError` |

**This is the only agent-facing endpoint.** Agents do not hit
`/api/v1/*` URLs. If a future need arises for agents to call
user-facing APIs (unlikely — the WebSocket command catalogue is
the right abstraction), it requires a dedicated amendment here
plus a scope extension in SRD-0114.

## D6 — Event stream

The controller emits a monotonic event stream per SRD-0100 D10.
Clients subscribe.

**Endpoint.** `WS /api/v1/events/subscribe`. WebSocket. Bearer
token or system API key (with impersonation header for
effective-user filtering).

**Subscribe message.** Client sends a single subscription JSON
frame after handshake:

```json
{
  "since": "2026-04-24T14:00:00Z",
  "filters": {
    "type": ["NODE_STATUS_CHANGED", "AGENT_CONNECTED"],
    "category": ["lifecycle"],
    "source": ["node:node-abc"]
  }
}
```

- `since` — optional. Omitted means "from now" (per
  `INV-EVENT-DEFAULT-NOW`). Present (RFC-3339 timestamp or
  opaque sequence cursor) means "historical replay from that
  point, then live."
- `filters` — each field optional; values are OR-within, AND-across.
  Omitted filter = no constraint on that axis.

**Event frame.** Each subsequent frame is one event:

```json
{
  "v": 1,
  "id": "evt-01HZX...",
  "seq": 104837,
  "ts": "2026-04-24T14:05:33.123Z",
  "type": "NODE_STATUS_CHANGED",
  "category": "lifecycle",
  "source": "node:node-abc",
  "subject": { "node_id": "node-abc" },
  "body": { "from": "registered", "to": "active" }
}
```

`seq` is a per-deployment monotonic counter; clients reconnecting
with `since=<seq>` resume without gaps.

**Reconnect.** Clients reconnecting after drop pass the last `seq`
they saw as `since`. The controller replays any events with
higher `seq` from its retention window, then switches to live.

![Event-stream subscribe + replay sequence: client opens the WebSocket, sends a subscribe frame with since and filters; controller replays events from retention up to the current cursor, then streams live events as they arrive. Edge cases: since absent starts "now"; since evicted returns SinceOutOfRetention (4010).](diagrams/SRD-0108/subscribe-replay.png)


**Retention.** Policy owned by SRD-0111. If a `since` references
a dropped event, the controller returns `4010 SinceOutOfRetention`
and the client must fall back to a fresh "now" subscription +
reconcile any stale projections via standard GET endpoints.

**Multiple subscriptions.** Per `INV-EVENT-INDEPENDENT` each
subscriber is independent. Clients may open multiple simultaneous
subscriptions with different filters; the controller treats each
as its own cursor.

**Permissions.** Each event is visibility-checked against the
subscriber's effective principal before being pushed. SRD-0114
D10 owns the rules. Events the principal cannot see are silently
omitted — they don't throw errors, they just don't appear.

## D7 — Writes emit events

Every endpoint that writes persistent state emits at least one
event. The event is emitted *after* the write commits; clients
see the write via the event stream if they're subscribed.

**Contract.**

- A `2xx` response to a write implies the write committed *and*
  the event was enqueued. The event may not have been delivered
  yet (subscriber lag), but it will be delivered.
- Events carry a `subject` that identifies the changed entity.
  Clients that care about a specific entity can filter by
  `subject.<field>`.
- Event types are enumerated in SRD-0111.

**Why this matters.** A client that mutates via the API and
observes via the event stream needs to know events for its
write will arrive; it should not poll for confirmation. This is
the contract that makes the htmx SSE pattern (SRD-0110) and the
CLI live-tail (SRD-0109) feasible without bespoke per-endpoint
ack machinery.

**INV-WRITE-EMITS-EVENT.** A `2xx` write response guarantees an
event enqueued for delivery. Write endpoints missing their event
emission are a violation detected in the conformance harness
(D10).

## D8 — Error model

Every non-2xx response carries a structured body, not just a
status code:

```json
{
  "error": {
    "code": "NodeNotFound",
    "message": "no node with id 'node-xyz'",
    "details": { "node_id": "node-xyz" },
    "request_id": "req-01HZX..."
  }
}
```

```
  Error body anatomy:

  ┌── code ──── stable identifier; clients branch on this
  │             (CamelCase, cataloged at /api/v1/system/errors)
  │
  ├── message ─ human-readable; may evolve between versions
  │
  ├── details ─ structured supplement; schema per code
  │             (what fields carry what — part of each code's spec)
  │
  └── request_id ── echoes X-Request-Id response header;
                    ties this response to the audit journal
                    (SRD-0114 D12) and controller logs
```


- `code` — stable identifier (CamelCase). Clients branch on
  this, never on `message`.
- `message` — human-readable, may evolve.
- `details` — structured supplementary data; schema per code.
- `request_id` — echoed from the `X-Request-Id` response header;
  ties to audit journal (SRD-0114 D12) and server logs.

**Status-code conventions.** Standard HTTP semantics, with a
small constrained set:

| Status | When |
|---|---|
| 400 | Malformed request (bad JSON, wrong shape) |
| 401 | Missing or invalid credential |
| 403 | Credential valid but lacks scope / role / visibility |
| 404 | Resource not found (post-auth) |
| 409 | Conflict — state doesn't permit the operation |
| 422 | Validation error — request well-formed but semantically invalid |
| 429 | Rate limited (reserved for future use — see design ruling) |
| 500 | Internal |
| 503 | Controller in degraded mode (DB unavailable, etc.) |

**Error codes.** Stable catalogue kept in `/api/v1/system/errors`
for client introspection. New codes added additively; removed
codes go through deprecation like any other surface change.

**INV-ERROR-STABLE-CODE.** Error `code` values are stable across
versions until deprecated. A client matching on `code` must keep
working; a client matching on `message` is broken by design.

## D9 — Rate limiting (deferred)

Per SRD-0108 stub ruling: no rate limiting at v1. Controller-
native throttling adds operational surface area (config,
exhaustion errors, exemption rules for privileged callers) with
no concrete contention scenario driving it. Revisit when a real
adopter's traffic pattern demands it.

When it lands, it lands as a `429 TooManyRequests` with a
`Retry-After` header and a `details.budget_reset_at` timestamp.
Rate-limit state lives in the controller (per
`INV-CTL-SOLE-WRITER`), keyed by principal.

## D10 — OpenAPI + conformance

**OpenAPI as source of truth for schemas.** The endpoint-by-
endpoint schema detail lives in a machine-readable `openapi.yaml`
checked into the repo. Client SDKs (the Rust CLI, any future
external SDK) generate from it. Review of SRD-0108 doesn't mean
review of schema-level changes; those ride through the OpenAPI
document's own change process.

**Why OpenAPI over a bespoke format.** The ecosystem has it.
`utoipa` generates the schema from Rust handler annotations at
build time; no hand-maintained document. Generators exist for
every language we'd ship an SDK in.

**Conformance harness.** A `hyperplane-api-tck` crate that, for
each `(endpoint, auth-tier, happy-path)` tuple, round-trips a
real request against the controller running with the SQLite
backend. The harness also asserts:

- `INV-WRITE-EMITS-EVENT` — every write endpoint actually emits.
- `INV-API-SINGLE-CRED` — tier-mismatch rejections.
- `INV-ERROR-STABLE-CODE` — known-code catalogue matches
  `/api/v1/system/errors`.
- `INV-PRINCIPAL-AT-BOUNDARY` — no endpoint responds 2xx without
  a resolved principal.

Adding an endpoint requires adding a TCK case. Missing TCK is a
CI failure, not a review comment.

## D11 — New invariants

| Code | Invariant |
|---|---|
| `INV-API-SINGLE-CRED` | Every request carries exactly one credential. |
| `INV-PRINCIPAL-AT-BOUNDARY` | Every handler runs with a resolved principal; no anonymous access. |
| `INV-WRITE-EMITS-EVENT` | Every 2xx write response implies an event enqueued. |
| `INV-ERROR-STABLE-CODE` | Error `code` values are stable across versions. |

These extend the SRD-0100 catalogue. Cited in TCK cases by name.

## Design rulings (resolved)

- **Controller sees principals, not just channels.** Captured
  in D3.
- **No idempotency-key machinery.** Per SRD-0105's declarative-
  and-idempotent command ruling, writes against the controller
  API are declarative goal-state assertions — retry of the same
  write lands on the same end state by construction. No
  per-request idempotency key headers are needed. If a specific
  endpoint cannot be expressed as a declarative write (rare),
  that endpoint documents its own retry safety story in its own
  schema annotation.
- **Rate limiting deferred.** D9.
- **OpenAPI as schema SoT.** D10.
- **Versioned from day one.** `/api/v1/` prefix; no unversioned
  `/api/` path. Java Hyperplane's unversioned API is not carried
  forward.
- **Event stream is WebSocket, not SSE.** SSE would also work
  for a one-way push, but the controller already operates a
  WebSocket server for agents (D5) — standardising on one
  bidirectional transport keeps the dependency graph small.
  The web server (SRD-0110) translates this to browser SSE at
  its BFF boundary since SSE is more htmx-native.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/hyperplane-controller/CONTROLLER-API.md`
  — the Java endpoint catalogue. The Rust port reshuffles into
  versioned `/api/v1/` paths and folds several ad-hoc endpoints
  into the paramodel-store-shell pattern, but the group
  taxonomy carries over.
- `~/projects/hyperplane/docs/ARCHITECTURE.md` §§ 4a, 4b — event
  streaming + browser-isolation invariants.
- `utoipa` crate — OpenAPI generation from Rust handler
  annotations.
