<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0106 — Docker Service Element

## Purpose

Specify service-mode Docker containers as a concrete
`ElementKind<HyperplaneRuntimeContext>`. Services are
long-running containers (HTTP APIs, databases, collectors)
with lifecycle `materialize` (pull + run) → `status_check` →
`dematerialize` (stop + rm). Covers the parameter schema,
required image labels, deployment sequence through the agent
control channel, parameter binding rules, health-check
semantics, log capture, naming + labeling conventions, and
image-registry policy.

The paramodel metadata for this kind is pinned by SRD-0102
D3: `TypeId = "service_docker"`,
`provides_infrastructure = false`, `shutdown_semantics =
Service`, plugs `commands-on` into Agent's socket, offers
`depends-on-service` to CommandDocker and other Services.
This SRD specifies runtime behaviour behind that shape.

## Scope

**In scope.**

- Parameter schema — required + optional parameters.
- Required image labels — `hyperplane_mode=service`,
  `hyperplane_api`.
- Deployment sequence — controller emits declarative
  `EnsureContainerRunning` command to the agent; agent pulls
  image, `docker run -d`, reports health.
- Parameter binding — how paramodel `BoundParameters` map to
  env vars, CLI args, volume mounts, port publishes.
- Health-check semantics — Dockerfile `HEALTHCHECK` + optional
  element-level override; agent polls, reports status upstream.
- Paramodel runtime binding — `materialize`, `status_check`,
  `dematerialize`, trial hooks.
- Log + artifact capture — stdout/stderr captured into
  paramodel `ArtifactStore`.
- Container naming convention (deterministic pattern).
- Runtime label catalogue applied at `docker run`.
- Image registry policy — where images come from,
  authentication.
- Error + recovery paths.

**Out of scope.**

- Command (run-to-completion) containers — SRD-0107.
- Agent wire protocol internals (SRD-0105).
- Parameter extraction from Dockerfiles (SRD-0103).
- EC2Node provisioning (SRD-0104).

## Depends on

- SRD-0100 (naming conventions; `INV-HYPERPLANE-NAMESPACE`).
- SRD-0102 (element kind registry — pins the paramodel shape
  of ServiceDocker).
- SRD-0103 (param extraction — the `ParamSpace` of a specific
  image).
- SRD-0105 (agent control channel — commands ride this
  transport).
- SRD-0106 (self — this SRD).
- SRD-0114 (principals — the `user` in the container name
  stem).

---

## Deploy sequence at a glance

```
Executor      Controller          Agent              Docker daemon
   │              │                 │                     │
   │── Deploy ───▶                  │                     │
   │              │                 │                     │
   │              │─ resolve digest │                     │
   │              │─ build spec     │                     │
   │              │                 │                     │
   │              │── EnsureContainerRunning ──▶          │
   │              │                 │                     │
   │              │                 │── docker pull ─────▶│
   │              │                 │◀─── image ready ────│
   │              │                 │                     │
   │              │                 │── docker run -d ───▶│
   │              │                 │◀── container_id ────│
   │              │                 │                     │
   │              │                 │── start log tail ──▶│
   │              │                 │◀── stdout chunks ───│
   │              │◀─── LogChunk ───│                     │
   │              │                 │                     │
   │              │                 │◀── health event ────│
   │              │◀─ EventPush ────│ (healthy)           │
   │              │                 │                     │
   │              │◀─ CommandResponse: ok                 │
   │              │                 │                     │
   │◀─ materialize outputs ─────────                     │
```

Runtime labels applied at `docker run --label ...`:
`hyperplane_user`, `.study`, `.trial`, `.execution`,
`.element`, `.instance`. Container name follows SRD-0106 D9
pattern.

## D1 — Parameter schema

| Parameter | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `image` | string | yes | — | OCI reference. Tag or digest. Resolved at materialization time (D3). |
| `command` | list<string> | no | image default | Overrides the image's `CMD`. |
| `entrypoint` | list<string> | no | image default | Overrides the image's `ENTRYPOINT`. |
| `env` | map<string,string> | no | `{}` | Environment variables. Merged with image defaults; user-supplied wins on collision. |
| `ports` | list<port-spec> | no | `[]` | Port publishes. Each spec is `{ name, container_port, protocol, host_port? }`. Omitted `host_port` means "ephemeral, allocated by the agent." |
| `volumes` | list<volume-spec> | no | `[]` | Volume mounts. Each `{ source, target, readonly?, kind }` where `kind` is `bind` / `named` / `tmpfs`. |
| `resource_limits` | object | no | — | `{ cpu_shares?, memory_mb?, pids?, ... }`. Maps to `docker run` resource flags. |
| `hyperplane_api` | string | no | — | Declares the API this container implements (SRD-0102 D4). Carried as a runtime label too. |
| `restart_policy` | enum | no | `unless-stopped` | `no` / `on-failure` / `always` / `unless-stopped`. |
| `healthcheck_override` | object | no | — | Element-level override of the image's `HEALTHCHECK` — same shape as Docker's. |

**Result parameters** (materialization outputs):

| Result | Type | Meaning |
|---|---|---|
| `container_id` | string | Docker container ID. |
| `container_name` | string | Resolved hyperplane container name (D9). |
| `resolved_ports` | map<name, int> | `ports[].name` → host port actually bound (post-ephemeral-allocation). |
| `endpoint_url` | string | Derived: `http://{node_private_ip}:{resolved_ports.<primary>}`, where `<primary>` is the port named in `hyperplane_api` metadata or the first port if none. |

**Parameter-to-`ParamSpace` binding.** Every parameter with a
`hyperplane_api`-extracted `ParamSpace` (per SRD-0103) is
validated against the image's declared domain at
plan-compile. An `image=` value whose extracted `ParamSpace`
contains a required `@param` unbound on the element fails
compile with `RequiredParamUnbound`.

## D2 — Required image labels

Every image referenced as a ServiceDocker must carry:

- `hyperplane_mode=service` — asserts the image is authored
  as a long-running service. The ServiceDocker kind rejects
  images lacking this label at extraction time (SRD-0103
  step 4).
- `hyperplane_api=<opaque-string>` — identifies which
  hyperplane API the container implements (SRD-0102 D4).
  Recorded and passed through; hyperplane treats the value as
  opaque.

Plus OCI standard labels (`org.opencontainers.image.*`) by
convention.

Labels that were previously under `com.hyperplane.*` now use
the `hyperplane_*` snake_case convention per SRD-0100 D14.

## D3 — Deployment sequence

Triggered by `AtomicStep::Deploy` against a ServiceDocker
element instance. Per SRD-0105 D12 commands are declarative +
idempotent; the agent reconciles.

1. **Resolve image digest.** Controller resolves the `image`
   parameter to a digest (per SRD-0103 D8 tag-to-digest flow).
   The `ParamSpace` is confirmed cached; the resolved digest
   plus `BoundParameters` form the container spec.
2. **Select host.** Paramodel's algebra has already paired the
   ServiceDocker instance with its target EC2Node via the
   `commands-on` plug → Agent → `deploys-onto` plug → EC2Node
   chain (SRD-0102 D3). The controller looks up the agent on
   the selected node.
3. **Build container spec.** Compose the declarative
   `ContainerSpec` payload:
   - Digest (not tag — ensures agent pulls the exact image).
   - Resolved env, command, entrypoint, ports, volumes,
     resource limits, restart policy.
   - Container name per D9.
   - Runtime label set per D10.
   - Health-check override if present.
4. **Send `EnsureContainerRunning`.** Controller sends the
   `CommandRequest { kind: EnsureContainerRunning, spec }`
   over the agent's WebSocket (SRD-0105 D6).
5. **Agent reconciles.** The agent:
   - Checks whether a container with the resolved name is
     already running matching `spec`. If yes, no action.
   - If absent: `docker pull` (using digest), `docker run -d`
     with the full spec.
   - If present-but-mismatched: `docker stop && docker rm`,
     then pull-and-run as above.
   - Starts log capture (D7).
   - Responds `CommandResponse { status: ok, result:
     { container_id, resolved_ports } }`.
6. **Health gate.** If the image defines `HEALTHCHECK` (or the
   element supplies `healthcheck_override`), the agent polls
   and reports each transition. Controller blocks `materialize`
   return until the container reaches `healthy` (or the
   10-minute default timeout elapses → `materialize` fails).
7. **Materialization outputs.** Controller computes
   `endpoint_url` from `resolved_ports` + node's `private_ip`;
   assembles the outputs; returns to paramodel.

**Why declarative.** Per SRD-0105 D12 / `INV-COMMAND-IDEMPOTENT`,
the same command can be re-issued any number of times; the
agent's reconcile loop converges to the declared state. This
handles agent-reconnect mid-deploy (agent rejoins and the
controller resends its pending commands without worrying about
dedup).

## D4 — Parameter binding

**Env vars.** Element's `env` merged with the image's default
ENV (from the Dockerfile). User-supplied wins on collision.
Token expressions in env values (e.g.
`${target_service.endpoint_url}`) resolve at materialization
via paramodel's token resolution (SRD-0010).

**CLI args.** `command` + `entrypoint` passed through to
Docker. An unset `command` preserves the image's `CMD`; an
empty-list `command` overrides to no args.

**Volume mounts.** `volumes[].source` is interpreted per
`kind`:
- `bind` — host path on the node. Must be within a node
  allow-list (operator config; prevents plans from mounting
  arbitrary host directories).
- `named` — a Docker named volume; the agent creates it lazily
  if missing.
- `tmpfs` — in-memory mount.

**Port publishes.** Each `ports[]` spec translates to
`-p {host_port}:{container_port}/{protocol}`. An omitted
`host_port` becomes `-P`-style ephemeral allocation; the agent
reads the actual host port post-`docker run` and reports back
in `resolved_ports`.

**Resource limits.** Mapped to `docker run` flags
(`--cpu-shares`, `--memory`, `--pids-limit`).

## D5 — Health-check semantics

```
  Resolution priority (first wins):

    element.healthcheck_override          ← plan author's explicit override
         │
         └── else: image HEALTHCHECK      ← Dockerfile-declared default
              │
              └── else: no health polling ← container "healthy" when "running"

  materialize gate:
    healthy reached → materialize returns outputs
    timeout exceeded → materialize fails (container left running for inspection)
    opt-out: healthcheck_override=null AND image has no HEALTHCHECK
             → materialize unblocks at "running"
```


Two sources of health-check configuration, in priority order:

1. **Element-level `healthcheck_override`** on the element
   instance. Full shape:
   ```
   { command: ["CMD", "curl", "-f", "http://localhost/health"],
     interval_s: 5,
     timeout_s: 3,
     retries: 3,
     start_period_s: 10 }
   ```
   Takes precedence over image defaults.
2. **Image's `HEALTHCHECK` directive** (authored in the
   Dockerfile). Used as-is if no override.

If neither is present, the container is considered "healthy"
as soon as it's `running` (Docker state) — no health polling.

**Reporting.** The agent's Docker event stream (SRD-0105 D10)
carries `health_status` transitions (`starting` → `healthy` /
`unhealthy`). Each transition is pushed to the controller as
an `EventPush`, which raises a `CONTAINER_HEALTH_CHANGED`
system event (SRD-0111) and updates the container's visible
status.

**`materialize` health gate.** By default, `materialize`
blocks until the container reports `healthy`. An element
may opt out by setting `healthcheck_override = null` explicitly
plus an `image` that has no `HEALTHCHECK`, in which case
`materialize` returns as soon as the container is `running`.

## D6 — Paramodel runtime binding

| Hook | Behaviour |
|---|---|
| `materialize(resolved) -> MaterializationOutputs` | Execute D3 sequence; block until `healthy` (D5) or timeout; return outputs from D1. |
| `status_check() -> LiveStatusSummary` | Read the agent's last-known container state (sourced from the Docker event stream); return `(container_state, health_status, last_observed_ts)`. No per-call docker inspect — the agent's event-driven state is authoritative. |
| `dematerialize() -> Result<()>` | Send `EnsureContainerAbsent { name }` command; agent `docker stop` (grace period 10s default, configurable) then `docker rm`; await confirmation; clear bridge rows (SRD-0101 D3). |
| `on_trial_starting(ctx)` | Optional — emits a `CONTAINER_TRIAL_BOUND` event linking this container to the active trial. |
| `on_trial_ending(ctx)` | Optional — emits `CONTAINER_TRIAL_UNBOUND`. |
| `observe_state(listener)` | Subscribes to the container's state stream; delivers synthetic `(Unknown → current)` on subscribe per paramodel contract. |

## D7 — Log + artifact capture

Each running ServiceDocker container has its stdout and
stderr captured.

**Mechanism.** The agent opens a `StreamLogs` command against
its own Docker daemon (via `bollard::logs()`) as soon as the
container starts. Each chunk is forwarded over the WebSocket
as `LogChunk` messages (SRD-0105 D5).

**Persistence.** The controller writes incoming log chunks to
paramodel's `ArtifactStore` (SRD-0012), keyed by
`{container_name}.stdout` / `{container_name}.stderr`.
Artifacts are appendable; the controller writes in
append-chunks, not full rewrites.

**Browse surface.** Logs are browsable through the controller
API (`GET /api/v1/agents/{id}/containers/{name}/logs` for
live tail, `GET /api/v1/artifacts/{id}` for the stored
artifact).

**Retention.** Per SRD-0101's cascade policy, the log
artifacts follow the trial / execution retention rules —
they're not separately tagged for expiry.

## D8 — Image registry policy

- **Registry address.** Operator config (SRD-0112). An image
  reference without an explicit registry host defaults to the
  operator-configured default registry.
- **Trust.** Trusted registries are listed in operator config;
  the agent's Docker daemon is configured with matching
  `insecure-registries` + auth credentials at EC2Node
  cloud-init time (SRD-0104 D5).
- **Digest pinning.** Controller always resolves to digest
  before sending the agent (D3 step 1). Agents never do
  tag-to-digest resolution themselves — eliminates
  time-of-check/time-of-use races.
- **Authentication.** Per-registry credentials live in the
  controller's credential store. When the agent needs a pull,
  the controller includes an auth-header in the command
  payload; the agent uses it for that pull only.

## D9 — Container naming

Deterministic pattern, identical across ServiceDocker and
CommandDocker:

```
hyperplane-{mode}-{user}-{exec-id-8}-{element-name}[_{instance-suffix}]
```

- `{mode}` — `service` (ServiceDocker) or `command`
  (CommandDocker).
- `{user}` — principal username (SRD-0114). Operator-
  eyeballable filter: `docker ps | grep hyperplane-service-jshook`.
- `{exec-id-8}` — first 8 chars of the paramodel execution
  ULID. Scopes the name to one execution.
- `{element-name}` — user-authored paramodel `ElementName`.
- `{instance-suffix}` — instance discriminator (`_000`,
  `_001`, ...) when `max_concurrency > 1`. Omitted for
  single-instance.

**Sanitization.** Element names that violate Docker's naming
rules (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`) have disallowed
characters replaced with `-`. If sanitization causes a
collision under one execution, plan-compile fails with
`ContainerNameCollision` and the author renames the element.

**Deterministic rationale.** Stable across agent/controller
reconnect (the name is re-derived, not looked up); readable
at the node; scoped so overlapping executions don't collide.

## D10 — Runtime label catalogue

Applied by the agent at `docker run --label ...` time, in
addition to the image-inherited labels:

| Label | Value |
|---|---|
| `hyperplane_user` | Principal username (SRD-0114) |
| `hyperplane_study` | Study name |
| `hyperplane_trial` | Trial name |
| `hyperplane_execution` | Execution ULID (full) |
| `hyperplane_element` | Element name |
| `hyperplane_instance` | Instance suffix (omitted if single) |

**Why label even when the name encodes some of this.** Labels
are queryable:
`docker ps --filter label=hyperplane_trial=vector-1m` returns
every container from that trial across every node. The
container name is the eyeball-friendly view; labels are the
programmatic filter. Both mechanisms, same underlying
identifiers.

**Event-stream correlation.** Docker's `/events` (SRD-0105
D10) carries `actor.attributes` with the labels. The
controller's topology projection correlates container events
back to paramodel entities by reading the labels off each
event — no local lookup table.

## D11 — Error + recovery

| Failure | Behaviour |
|---|---|
| Image pull failure (not found, auth denied) | Agent returns `CommandResponse { status: error, code: ImagePullFailed }`. `materialize` fails; paramodel marks the step failed. |
| Container-exits-immediately | Agent observes `die` event < 1s after start, captures logs, reports `CommandResponse { status: error, code: ContainerExitedImmediately, logs_artifact_id }`. |
| Health check times out | `materialize` returns `HealthCheckTimeout`. The container is left running (operator can inspect it) unless the element's `dematerialize` is called. |
| Node goes away mid-run | Agent WebSocket drops; per SRD-0105 D9, controller waits for reconnect; if node terminates, `status_check` surfaces it and the executor re-provisions per plan. |
| `dematerialize` fails (agent unreachable) | Best-effort: if the node's already marked terminated, the container is gone with it. If the node is reachable but the agent is unresponsive, retry with back-off; after a threshold, mark the container orphaned and surface a warning event. |

## D12 — Cross-references

Inherits naming + labeling convention from this SRD; shared by
CommandDocker (SRD-0107 D12 cross-references this SRD). The
two kinds diverge only at the lifecycle layer (`Service` →
Deploy + Teardown vs `Command` → Deploy + Await); identity
conventions are the same.

## Design rulings (resolved)

- **Cardinal relationships are paramodel's purview.** D6
  implementation notes restate; full ruling in SRD-0102 D3
  via plug/socket metadata.
- **Container naming is deterministic with user + execution
  in the name stem.** D9.
- **Deployed containers carry structured runtime labels.**
  D10.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`
  — Java-era label convention ported in D2 with snake_case
  per SRD-0100 D14.
- `~/projects/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/agent/AgentDockerService.java`
  — Java reference implementation of the deploy sequence.
- `bollard` crate — Rust Docker client.
