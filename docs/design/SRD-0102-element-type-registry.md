<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0102 — Hyperplane First-Party Element Kinds

## Purpose

Specify the first-party element kinds hyperplane ships, their
paramodel metadata (descriptor, `ShutdownSemantics`, plugs,
sockets), the hyperplane-side ambient context
(`HyperplaneRuntimeContext`) those kinds consume, and the
`hyperplane_api` parameter convention adopters use for
container-implemented APIs.

The registration mechanism itself — `ElementKind<Ctx>`, the
`inventory`-based seam, the `KindRegistry` bridge to paramodel's
existing registry traits, the compile-time `ShutdownSemantics`
check — lives in paramodel SRD-0014 and is not restated here.
Hyperplane is a paramodel adopter with respect to element
registration; this SRD covers only what hyperplane supplies as
an adopter.

Hyperplane ships four first-party kinds in the
`hyperplane-elements-ec2` crate: **EC2Node**, **Agent**,
**ServiceDocker**, **CommandDocker**. The crate is EC2-native;
a future non-EC2 stack (GCP, Azure, bare metal, Kubernetes)
would ship as a sibling crate with its own counterparts, not
as a generalisation of these types.

## Scope

**In scope.**

- `HyperplaneRuntimeContext` — the ambient context type
  (paramodel's `Ctx`) hyperplane's kinds consume.
- First-party kinds: EC2Node, Agent, ServiceDocker,
  CommandDocker.
- Each kind's paramodel metadata: descriptor fields,
  `ShutdownSemantics`, required/optional parameters,
  labels/tags the kind declares or expects, plug + socket
  declarations.
- `hyperplane_api` parameter convention.
- Crate layout for the first-party set.
- Extension-author pointer: third-party kinds use the same
  SRD-0014 seam, parameterised on whatever `Ctx` the adopter
  chooses.

**Out of scope.**

- `ElementKind<Ctx>` trait itself (SRD-0014).
- `inventory` auto-registration mechanism (SRD-0014).
- `ShutdownSemantics` compile-time validation (SRD-0014 D4).
- Per-kind runtime behaviour in depth (SRD-0104 EC2Node,
  SRD-0105 Agent protocol, SRD-0106 ServiceDocker, SRD-0107
  CommandDocker).
- Parameter extraction from container images (SRD-0103).
- Controller API endpoints that expose registered kinds
  (SRD-0108).

## Depends on

- SRD-0014 (paramodel element kind registry) — the trait +
  registration mechanism hyperplane's kinds implement.
- SRD-0101 (state boundaries).
- Paramodel SRD-0005 (Labels, Plugs, Sockets) — compatibility
  metadata.
- Paramodel SRD-0007 (Elements and Relationships) —
  `ElementTypeDescriptor`, `ShutdownSemantics`.

---

## First-party kinds at a glance

![First-party element wiring: EC2Node offers a hosts socket; Agent plugs into it via Dedicated relationship and offers a commands-on socket; ServiceDocker and CommandDocker each plug into Agent's commands-on via Dedicated; CommandDocker optionally plugs into ServiceDocker's depends-on-service via Linear relationship.](diagrams/SRD-0102/kinds-wiring.png)

**What the wiring means:**

- EC2Node is a leaf — it provides `hosts`, depends on nothing.
- Agent plugs into EC2Node's `hosts` socket (one agent per
  node). It offers `commands-on` to whatever runs on this
  node.
- ServiceDocker / CommandDocker plug into the agent's
  `commands-on` socket. Transitively they run on the agent's
  node.
- A CommandDocker may optionally depend on a ServiceDocker
  via the `depends-on-service` plug/socket pair with a
  `Linear` relationship — "wait until the harness is healthy,
  then fire the client."
- All compatibility rules ride the plug/socket metadata. There
  is no central compatibility table (per SRD-0014 D5).

## D1 — Crate layout

The first-party kinds are EC2-native and ship together in
`hyperplane-elements-ec2`.

```
crates/
├── hyperplane-elements-ec2/        # First-party kinds (EC2Node, Agent, ServiceDocker, CommandDocker)
├── hyperplane-controller/          # Depends on the elements crate + registers them
└── ...
```

**`hyperplane-elements-ec2` crate.** The four kinds live
together because they are co-designed to compose: EC2Node
provisions the host, Agent runs on the host and brokers
commands, ServiceDocker and CommandDocker run as workloads
that the Agent launches. Sharing internal helpers (EC2 API
wrappers, Docker client plumbing, container-spec builders,
log-capture utilities) without promoting those helpers to a
public crate boundary is the payoff. Each kind is still a
decoupled module with its own `ElementKind<HyperplaneRuntimeContext>`
impl and its own `inventory::submit!`; the crate is a cohesive
container, not a coupled blob.

Crate-name `-ec2` suffix makes the EC2 binding explicit at the
dependency layer. A non-EC2 stack later ships as its own
sibling crate (`hyperplane-elements-gcp`,
`hyperplane-elements-k8s`, etc.) with matching counterparts —
no attempt to pretend EC2 is a generic abstraction at the
type level.

Structure inside the crate:

```
hyperplane-elements-ec2/src/
├── lib.rs                  # Re-exports, crate-wide wiring
├── shared/                 # Internal helpers (EC2 API, Docker, spec builders)
├── ec2_node/               # EC2Node kind
├── agent/                  # Agent kind
├── service_docker/         # ServiceDocker kind
└── command_docker/         # CommandDocker kind
```

Dependencies for the whole crate (`aws-sdk-ec2`, `bollard`,
etc.) are declared once; a deployment that links
`hyperplane-elements-ec2` gets all four kinds.

**Third-party kind crates.** Same shape as
`hyperplane-elements-ec2` from the registry's perspective:
depend on paramodel (for `ElementKind<Ctx>` + `inventory`
entry), declare a `Ctx`, call `inventory::submit!`. They may
share hyperplane's `HyperplaneRuntimeContext` (and so
co-habit with the first-party kinds) or declare a different
`Ctx`. The registry treats both symmetrically — no privileged
path for first-party kinds.

## D2 — `HyperplaneRuntimeContext`

The adopter-supplied `Ctx` paramodel expects (SRD-0014 D6).
Every kind's `build_runtime(element, ctx: &HyperplaneRuntimeContext)`
receives this value; it carries the ambient capabilities every
hyperplane runtime needs.

```rust
pub struct HyperplaneRuntimeContext {
    /// Handle to the agent connection registry. Used by kinds
    /// that deploy to a node (ServiceDocker, CommandDocker);
    /// used by EC2Node to mint + deliver `auth-token`s at deploy.
    pub agents: Arc<dyn AgentDirectory>,

    /// Paramodel artifact-store handle for capturing
    /// stdout/stderr, container logs, diagnostic dumps.
    pub artifacts: Arc<dyn paramodel::ArtifactStore>,

    /// Paramodel metadata-store handle for resolving element
    /// cross-references (e.g. "what node does this service
    /// target?").
    pub metadata: Arc<dyn paramodel::MetadataStore>,

    /// EC2-specific client. Only EC2Node uses this; packaged in
    /// the context because the first-party crate is EC2-bound.
    pub ec2: Arc<dyn Ec2Client>,

    /// Image cache from SRD-0103. Used by ServiceDocker +
    /// CommandDocker when pulling images.
    pub images: Arc<dyn ImageCache>,
}
```

The shape is stable — changes here require an SRD-0102 amendment,
not a per-kind workaround. A third-party kind that needs a new
ambient capability raises an SRD to extend the context rather
than reaching into crate internals.

```
  HyperplaneRuntimeContext ── consumed by which kind?

                    agents  artifacts  metadata  ec2   images
                    ──────  ─────────  ────────  ────  ──────
  EC2Node            ✓         ·          ✓      ✓       ·
  Agent              ✓         ·          ✓      ·       ·
  ServiceDocker      ✓         ✓          ✓      ·       ✓
  CommandDocker      ✓         ✓          ✓      ·       ✓

  legend:  ✓ actively used     · provided but unused
```

Every kind receives the full context; kinds ignore fields they
don't need. Adding a new capability means adding one field —
existing kinds are unaffected.

## D3 — First-party kinds

Each kind has its own SRD for runtime detail. This SRD pins
the paramodel metadata visible to plan authors: descriptor
fields, `ShutdownSemantics`, parameters, plugs, sockets, and
what the kind accepts on its incoming/outgoing relationships.

Compatibility — who may depend on whom — rides the plug/socket
metadata on each kind (per SRD-0014 D5). Paramodel's algebra
validates that plug/socket pairs match at plan-compile time.
There is no cross-kind compatibility table in this SRD.

### EC2Node (SRD-0104)

- **`TypeId`:** `"ec2_node"`.
- **`provides_infrastructure`:** `true`.
- **`shutdown_semantics`:** `Service`. A node is long-running;
  its teardown is an explicit `Shutdown` directive (SRD-0105).
- **Required parameters.** `instance_type` (string, from AWS
  catalogue), `region` (string), `ami` (string). Optional:
  `subnet`, `key_pair`, `tags` (map<string,string>).
- **Labels.** Plan authors may specialise with labels (e.g.
  `gpu`, `arch=arm64`) that downstream elements match against
  via their own `Dependency` selectors.
- **Plugs:** none. An EC2Node is a host — it is depended on,
  it does not depend.
- **Sockets:**
  - `hosts` — offered to any element that needs a node to run
    on. Consumed by Agent, ServiceDocker, CommandDocker
    through their `deploys-onto` plug.
- **Materialization outputs.** `instance_id`, `public_ip`,
  `private_ip`.

### Agent (SRD-0105)

- **`TypeId`:** `"agent"`.
- **`provides_infrastructure`:** `false`.
- **`shutdown_semantics`:** `Service`. The agent is
  long-running for the lifetime of its node. When the node
  shuts down, the agent dies as a consequence (not as a
  separate teardown step — `Shutdown` is a node-level
  directive per SRD-0105 D11).
- **Required parameters.** None at the plan layer; the agent's
  binary + config are controller-managed.
- **Plugs:**
  - `deploys-onto` — consumes an EC2Node's `hosts` socket.
    `Dedicated` relationship: one agent per node.
- **Sockets:**
  - `commands-on` — offered to ServiceDocker + CommandDocker.
    Each Docker workload binds to the agent on its node
    through this socket.
- **Materialization outputs.** `agent_id`, `auth_token_hash`
  (for audit; plaintext is never on the plan plane).

**Why surface Agent as an element kind.** An agent on a node
is internal wiring from the "how do containers get launched"
perspective, but it is *tactile* from the plan author's
perspective: labelling the agent explicitly lets a plan depend
on agent presence or agent version, lets the topology view
show agents as distinct entities, and opens the door to
reusing the agent kind under non-EC2 hosts later (a future
bare-metal node kind could offer the same `hosts` socket and
Agent would deploy onto it unchanged). Tactile types are the
point of hyperplane.

### ServiceDocker (SRD-0106)

- **`TypeId`:** `"service_docker"`.
- **`provides_infrastructure`:** `false`.
- **`shutdown_semantics`:** `Service`. A long-running
  container; paramodel reducto emits Deploy + Teardown.
- **Required parameters.** `image` (string, OCI reference).
- **Optional parameters.** `command` (list<string>), `env`
  (map<string,string>), `ports` (list<port-spec>), `volumes`
  (list<volume-spec>), `resource_limits` (object),
  `hyperplane_api` (string — D4).
- **Required image label** (on the Docker image, not the
  element): `hyperplane_mode=service`. The ServiceDocker
  kind rejects images lacking this label at
  registration/extraction time (SRD-0103).
- **Plugs:**
  - `commands-on` — consumes an Agent's `commands-on` socket.
    `Dedicated` relationship: the service binds to the agent
    on its node.
  - (Transitively targets an EC2Node via the Agent's
    `deploys-onto` edge; plan authors express "this service
    runs on that node" by wiring through the Agent.)
- **Sockets:**
  - `depends-on-service` — offered to CommandDocker (and to
    other ServiceDocker instances with startup-ordering
    requirements). Matched by `Linear` relationship for
    "start me after the target is healthy."
- **Materialization outputs.** `container_id`,
  `resolved_ports` (map<name, host-port>), `endpoint_url`
  (optional; derived from `hyperplane_api` + resolved port +
  node IP).

### CommandDocker (SRD-0107)

- **`TypeId`:** `"command_docker"`.
- **`provides_infrastructure`:** `false`.
- **`shutdown_semantics`:** `Command`. A run-to-completion
  container; paramodel reducto emits Deploy + Await.
- **Required parameters.** `image` (string, OCI reference).
- **Optional parameters.** `command` (list<string>), `env`
  (map<string,string>), `volumes` (list<volume-spec>),
  `timeout` (duration), `hyperplane_api` (string — D4).
- **Required image label** (on the Docker image, not the
  element): `hyperplane_mode=command`. The CommandDocker
  kind rejects images lacking this label at
  registration/extraction time.
- **Plugs:**
  - `commands-on` — consumes an Agent's `commands-on` socket.
    Same pattern as ServiceDocker.
  - `depends-on-service` (optional) — consumes a
    ServiceDocker's `depends-on-service` socket via `Linear`
    relationship, for benchmark clients that wait for a
    harness to be healthy before firing.
- **Sockets:** none. A command is a leaf — it terminates; it
  is not depended on.
- **Materialization outputs.** `exit_code` (int),
  `stdout_artifact_id`, `stderr_artifact_id`,
  `result_parameters` (map — parsed from the command's output
  contract per SRD-0107).

## D4 — `hyperplane_api` parameter convention

The `hyperplane_api` parameter is a plain parameter, not a
reverse-DNS label namespace, not a trait the container
implements. It is a string value that declares which hyperplane
API a container implements — a convention by which adopters
and UIs discover "what does this container expose?"

**Hyperplane's treatment.** Opaque. Hyperplane records the
value, exposes it through the API and event stream, and
passes it through to UIs. No magic, no interpretation.

**Adopter conventions.** Adopters pick values that mean
something to their own tooling. Examples:

- `hyperplane_api=nb-jupyter` — a Jupyter notebook server.
- `hyperplane_api=api-openai` — an OpenAI-compatible endpoint.
- `hyperplane_api=prometheus-scrape` — a Prometheus-metrics
  endpoint.

The value is carried as an element parameter (not a Docker
label) so it survives through paramodel's parameter
resolution and is available on the element description.

**Why not Docker labels?** Per SRD-0103's ruling on parameter
authorship (Dockerfile `# @param` comments, not labels), the
label namespace is reserved for runtime metadata
(`hyperplane_mode`, image-digest pins). Parameter-level
concerns live in parameters.

**Why not a discriminated enum?** Enforcing "`hyperplane_api`
must be one of N known values" would make this a closed set
and require central coordination. Keeping it open-string is
the same open-for-extension principle paramodel's kind
registry (SRD-0014) builds on: third parties invent values
for their own concerns without asking permission.

## D5 — What a third-party hyperplane-kind crate looks like

A third-party kind that wants to co-habit with the first-party
set (same `HyperplaneRuntimeContext`, same inventory collection)
ships a crate with:

1. A dependency on paramodel (for `ElementKind<Ctx>` +
   `inventory::submit!`) and on `hyperplane-elements-ec2` (for
   `HyperplaneRuntimeContext`).
2. A struct implementing
   `ElementKind<HyperplaneRuntimeContext>`, including the
   mandatory `shutdown_semantics()` method.
3. An `inventory::submit!` call registering the kind.
4. A README documenting:
   - `shutdown_semantics` + the rationale.
   - Required parameters.
   - Materialization outputs.
   - Plug + socket declarations (what the kind offers to
     depend on; what it requires to depend on).
   - Any new `HyperplaneRuntimeContext` capabilities needed
     (if the current context is insufficient, that's an
     SRD-0102 extension, not a per-kind workaround).

**What the author gets for free.**

- Registration pickup at controller startup (SRD-0014 seam).
- Paramodel compile-time validation: `ShutdownSemantics`,
  label rules, plug/socket matching, relationship type
  compatibility.
- Exposure through the controller API (SRD-0108) — the new
  kind shows up in `/api/v1/system/element-kinds` with no
  code change in the API layer.
- Exposure through CLI (SRD-0109) + web UI (SRD-0110) — both
  discover kinds through the API, no hardcoded list.

**Third-party kinds with a different `Ctx`.** A kind whose
runtimes don't need the EC2/Docker/agent capabilities can
declare its own `Ctx` and live in a fully independent crate.
The controller binary then runs two inventory collections
side-by-side (one per `Ctx`). Supported; inventory handles
this through its per-type collection model.

## D6 — No new invariants

All kind-registration invariants are paramodel-tier (SRD-0014
D7): `INV-ELEMENT-KIND-OPEN`, `INV-ELEMENT-KIND-REGISTRATION`,
`INV-ELEMENT-KIND-SHUTDOWN-SEMANTICS`,
`INV-ELEMENT-KIND-TYPE-UNIQUE`. Hyperplane inherits them by
consuming the paramodel layer; it doesn't add its own.

## Open questions

None remaining.

## Reference material

- Paramodel SRD-0014 — `ElementKind<Ctx>` trait, inventory
  seam, registry bridges, compile-time `ShutdownSemantics`
  check. The mechanism this SRD consumes.
- Paramodel SRD-0005 — Labels, Plugs, Sockets. The metadata
  this SRD's kinds declare.
- Paramodel SRD-0007 — `ElementTypeDescriptor`,
  `ShutdownSemantics`, `ElementRuntime`.
- `~/projects/hyperplane/docs/architecture/planning_and_execution.md`
  section "The Hyperplane Bridge" — intent ported, namespace
  reverse-DNS `com.hyperplane.*` namespace dropped in favour of
  plain `hyperplane_*` snake_case (we don't own a DNS namespace,
  so we don't pretend to by using one).
- `~/projects/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`
  — image-label conventions; ported in SRD-0106 / SRD-0107.
