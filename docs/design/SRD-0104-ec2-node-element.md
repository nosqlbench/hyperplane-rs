<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0104 — EC2 Node Element

## Purpose

Specify the EC2Node as a concrete `ElementKind<HyperplaneRuntimeContext>`.
Covers the full infrastructure lifecycle of an EC2 compute host:
provisioning, cloud-init contract, SSH key management, node
state machine, and the mapping from paramodel's
`AtomicStep::Deploy` and `AtomicStep::Teardown` to the concrete
sequence "launch an instance, install the agent, mark ready;
later, initiate shutdown, observe termination."

The paramodel metadata for this kind is pinned by SRD-0102 D3:
`TypeId = "ec2_node"`, `provides_infrastructure = true`,
`shutdown_semantics = Service`, offers a `hosts` socket that
Agent consumes. This SRD specifies the runtime behaviour
behind that shape.

## Scope

**In scope.**

- Parameter schema — required, optional, default-bearing
  parameters.
- Allow-list contract — operator-supplied list of permitted
  instance types and AMIs.
- Provisioning flow — EC2 `RunInstances` API sequence.
- Cloud-init contract — what the boot script must do, what it
  may not do, what hyperplane injects.
- Node contract — OS version, filesystem layout, required
  packages, observability mesh.
- SSH key management — generation, storage, rotation.
- Node state machine — requested → provisioning → configuring →
  configured → deploying → deployed → registering → registered →
  active_heartbeat → draining → terminated, plus failure/retry
  states.
- Transition triggers and events emitted on each transition.
- Paramodel runtime binding — `materialize`, `status_check`,
  `dematerialize` implementations.
- Agent privileges on the node — the narrow sudo rule granting
  only `shutdown`.
- Error recovery — cloud-init failure, agent install failure,
  SSH reachability loss, registration timeout.

**Out of scope.**

- Agent binary + wire protocol (SRD-0105).
- Docker service/command deployment on the node (SRD-0106 /
  SRD-0107).
- Multi-cloud abstraction (future; EC2-only for v1 — a GCP
  node, Azure VM, bare-metal node would ship as a sibling kind
  in a sibling crate).

## Depends on

- SRD-0100 (invariants — sole-writer, agent isolation,
  lifecycle independence, naming conventions).
- SRD-0102 (element kind registry — pins the paramodel shape
  of EC2Node).
- SRD-0105 (agent + control channel; the ready state needs an
  agent that has registered).
- SRD-0114 (principals — who gets to provision).

---

## Provisioning flow at a glance

```
Controller                           AWS EC2                Node (post-boot)          Agent
    │                                   │                        │                      │
    │── RunInstances ───────────────────▶                        │                      │
    │                                   │ spin up                │                      │
    │◀── instance-id + pending ─────────│                        │                      │
    │                                   │                        │                      │
    │  node row: PROVISIONING           │                        │                      │
    │                                   │                        │                      │
    │── DescribeInstances (5s poll) ───▶│                        │                      │
    │◀── running ───────────────────────│                        │                      │
    │                                   │                        │                      │
    │  PROVISIONED                      │                   cloud-init                  │
    │                                                        starts                     │
    │◀───────── Vector: cloud-init logs WebSocket ───────────│                          │
    │                                                                                   │
    │  CONFIGURING → CONFIGURED                                                         │
    │                                                                                   │
    │─── SSH: scp agent, write config, enable systemd unit ────▶                       │
    │                                                                                   │
    │  DEPLOYING → DEPLOYED                                                             │
    │                                                                         systemd   │
    │                                                                         starts    │
    │◀───────────────── WebSocket open + Register ──────────────────────────────────────│
    │                                                                                   │
    │  REGISTERING → REGISTERED → (first heartbeat) → ACTIVE_HEARTBEAT                  │
    │                                                                                   │
    │  materialize returns outputs (instance_id, public_ip, ...)                        │
```

## Node state machine at a glance

```
         ┌──────────────┐
         │ PROVISIONING │──────────────┐
         └──────┬───────┘              │
                ▼                      │
         ┌──────────────┐              │
         │  CONFIGURING │───┐          │
         └──────┬───────┘   │          │
                ▼           ▼          │
         ┌──────────────┐  (failed)    │
         │ CONFIGURED   │              │
         └──────┬───────┘              │
                ▼                      │
         ┌──────────────┐              │
         │  DEPLOYING   │───┐          │
         └──────┬───────┘   │          │
                ▼           ▼          │
         ┌──────────────┐  (failed)    │
         │  DEPLOYED    │              │
         └──────┬───────┘              │
                ▼                      │
         ┌──────────────┐              │
         │  REGISTERING │───┐          │
         └──────┬───────┘   │          │
                ▼           ▼          │
         ┌──────────────┐  (failed)    │
         │  REGISTERED  │              │
         └──────┬───────┘              │
                ▼                      │
       ┌────────┴────────┐             │
       │ ACTIVE_HEARTBEAT│             │
       └──┬─────┬────────┘             │
          │     │                      │
          │     └─ (30s silence) ─┐    │
          │                       ▼    │
          │            ┌──────────────┐│
          │            │ LOST_HEART-  ││
          │            │ BEAT (retry) ││
          │            └──────────────┘│
          ▼                            ▼
         ┌──────────────────────────────────┐
         │          TERMINATED              │
         └──────────────────────────────────┘
```

Every transition emits `NODE_STATUS_CHANGED`. Retry arrows
omitted for clarity; full set in D7.

## D1 — Parameter schema

The EC2Node element exposes parameters that plan authors bind
per-execution. Every parameter's type + domain is validated by
paramodel's algebra (SRD-0004) against the kind's declared
`ParamSpace`.

| Parameter | Type | Required | Default | Meaning |
|---|---|---|---|---|
| `instance_type` | string | yes | — | EC2 instance type (`m7i.xlarge`, `g5.2xlarge`, etc.). Constrained to the operator-supplied allow-list (D2). |
| `ami` | string | yes | — | AMI identifier (`ami-0abc...`). Constrained to the operator-supplied allow-list (D2). |
| `region` | string | yes | — | AWS region (`us-east-1`, etc.). |
| `purchasing` | enum | no | `on_demand` | `on_demand` or `spot`. Spot adds price-cap + interruption handling (D4). |
| `spot_max_price` | string | no | — | Required when `purchasing=spot`; otherwise must be unset. Decimal USD per hour (`0.50`). |
| `subnet` | string | no | operator default | VPC subnet; falls back to the subnet the operator configured as the default provisioning subnet. |
| `key_pair` | string | no | operator default | AWS key-pair name used for the provisioning SSH connection (distinct from the runtime deploy key — D6). |
| `tags` | map<string,string> | no | `{}` | Extra AWS resource tags beyond the ones hyperplane applies itself. |
| `root_volume_gb` | integer | no | AMI default | Override the root-volume size. |
| `instance_store_formatting` | enum | no | `lvm_concat` | How to handle ephemeral NVMe. `lvm_concat` concatenates every ephemeral device into one ext4 at `/mnt/data` (the default profile); `none` leaves them unformatted. |

**Result parameters** (materialization outputs a plan author
can consume downstream):

| Result | Type | Meaning |
|---|---|---|
| `instance_id` | string | AWS instance ID (`i-xxx`). |
| `public_ip` | string | Public IP (nullable if subnet has none). |
| `private_ip` | string | Private IP. |
| `node_name` | string | Hyperplane-assigned node name (used as hostname + in agent names). |

**Labels.** Plan authors can apply labels to specialise a node
(`gpu`, `arch=arm64`, `role=harness`). Downstream elements
filter on labels via their `Dependency` selectors — paramodel's
existing algebra, not a hyperplane-specific mechanism.

## D2 — Allow-list contract

The EC2Node kind requires an operator-supplied allow-list at
controller startup. The list has two sections:

```toml
# /etc/hyperplane/ec2-node-allowlist.toml
[[instance_types]]
name = "m7i.xlarge"
architecture = "x86_64"

[[instance_types]]
name = "m7g.xlarge"
architecture = "aarch64"

[[amis]]
id = "ami-0abc1234"
region = "us-east-1"
architecture = "x86_64"
description = "Hyperplane default Ubuntu 24.04 x86_64"

[[amis]]
id = "ami-0def5678"
region = "us-east-1"
architecture = "aarch64"
description = "Hyperplane default Ubuntu 24.04 arm64"
```

**Enforcement.**

- Plan-compile rejects plans referencing `instance_type` or
  `ami` values not in the list, with
  `InstanceTypeNotAllowed` / `AmiNotAllowed`.
- Architecture consistency is enforced: the `ami` and
  `instance_type` must share an `architecture`; a mismatch
  fails compile with `ArchitectureMismatch`.

**Changes to the list** require a controller config reload
(SRD-0112). In-flight executions keep running against the
values they bound at compile time; new compiles pick up the
updated list.

**No "anything Ubuntu 24.04 that passes a contract" fallback.**
The operator owns the list; a plan author cannot supply
arbitrary values. This gives operators a clean control point
for cost, security, and capacity planning.

## D3 — Provisioning flow

Triggered by `AtomicStep::Deploy` against an EC2Node element
instance. Agent-side actions live in SRD-0105; controller-side
EC2 API actions are here.

1. **Validate.** Controller checks that `instance_type`,
   `ami`, `region` match the allow-list (D2). Reject
   `400 InstanceTypeNotAllowed` / `AmiNotAllowed` / etc.
2. **Generate node name.** A controller-assigned unique name
   (ULID-derived, e.g. `hp-01HZX5A2`). Used for hostname,
   resource tags, and agent identification.
3. **Generate SSH deploy key.** An ephemeral keypair for the
   SSH deploy channel (D6). Private key stored in the
   controller's credential store (SRD-0101, encrypted at
   rest); public key goes on the instance via user-data.
4. **Tag + request.** `RunInstances` call:
   - `ImageId = ami`
   - `InstanceType = instance_type`
   - `MinCount = MaxCount = 1`
   - `KeyName = key_pair` (if set)
   - `SubnetId = subnet` (if set)
   - `UserData = cloud-init-YAML` (D5)
   - `TagSpecifications` including `Name = node_name`,
     `hyperplane_managed = true`,
     `hyperplane_study = {study}`,
     `hyperplane_execution = {exec_id}`, plus operator tags
     from the `tags` parameter.
   - `InstanceMarketOptions.SpotOptions` when
     `purchasing=spot`, with `MaxPrice = spot_max_price` and
     `InstanceInterruptionBehavior = terminate`.
5. **Insert node row.** Controller writes a new `nodes` row
   (SRD-0101) in state `PROVISIONING`.
6. **Emit events.** `NODE_ADDED` + `NODE_STATUS_CHANGED
   (none → provisioning)` (per SRD-0111).
7. **Sync loop takes over.** A background task polls
   `DescribeInstances` every 5s for transitional nodes, advances
   the state machine (D7) as AWS state changes.
8. **Await registration.** Eventually the agent on the node
   opens its WebSocket and registers (SRD-0105 D2 step 7); the
   node transitions to `REGISTERED` then `ACTIVE_HEARTBEAT`.
9. **Step complete.** `materialize` returns the materialization
   outputs; paramodel proceeds to the next atomic step.

**Failure at any stage** transitions to the corresponding
`*_FAILED` terminal per D7. The controller does not auto-retry
— retry is a plan-author or operator decision.

## D4 — Spot handling

When `purchasing=spot`:

- Request adds `InstanceMarketOptions.SpotOptions`. AWS may
  reject if capacity is unavailable at the cap price; this
  surfaces as `PROVISIONING_FAILED` with an AWS error detail.
- Interruption behaviour is fixed at `terminate` (not `stop`
  or `hibernate`) — hyperplane assumes disposable nodes and
  can always re-provision.
- AWS emits a 2-minute spot interruption warning via EC2
  instance metadata; the agent polls metadata endpoint
  `http://169.254.169.254/latest/meta-data/spot/instance-action`
  and, on receipt, surfaces a `SpotInterruptionImminent`
  event through its WebSocket. Downstream consumers (executor,
  UI) react by marking dependent trials for re-run.

When `purchasing=on_demand`:

- `spot_max_price` must be unset — compile fails otherwise.
- No interruption-watcher; the agent skips the metadata poll.

## D5 — Cloud-init contract

Every EC2Node is provisioned with a cloud-init YAML that
hyperplane injects via `UserData`. The contract has three
parts.

**What cloud-init must do** (default-profile obligations):

1. Set hostname to the hyperplane-assigned node name.
2. Install and configure `chrony` for precise time
   synchronisation.
3. Configure the data volume per `instance_store_formatting`
   (default: LVM-concatenate all ephemeral NVMe into one
   `ext4` at `/mnt/data`, symlink `/home` → `/mnt/data/home`
   and `/var/lib/docker` → `/mnt/data/docker`).
4. Install Docker from official upstream repositories,
   configure it to:
   - Trust the configured internal registry.
   - Expose Prometheus metrics on `127.0.0.1:9323`.
   - Add the SSH user to the `docker` group.
5. Install and start the observability mesh:
   - **Vector** — streams cloud-init logs to the controller's
     `/api/v1/nodes/{id}/cloudinit/logs` WebSocket (SRD-0108).
     Enabled at boot; disabled once the agent registers.
   - **Node Exporter** on port 9100.
   - **vmagent** — scrapes Node Exporter (`:9100`) and Docker
     (`:9323`); pushes to the configured VictoriaMetrics
     endpoint.
6. Drop the hyperplane SSH public key into
   `/home/{user}/.ssh/authorized_keys`.
7. Signal cloud-init completion (normal
   `cloud-init status --wait` path).

**What cloud-init must not do:**

- Install or start the hyperplane agent. The agent is deployed
  later, over the SSH channel, per SRD-0105 D2. Cloud-init
  prepares the environment; the agent is a separate step.
- Open inbound SSH to the internet. The instance's security
  group is controlled by the operator; this SRD does not
  prescribe it, but the provisioning-key's purpose is a
  one-shot deploy, not long-term operator access.

**What hyperplane injects into the cloud-init YAML at
provision time:**

- Node name (as hostname).
- Hyperplane SSH public key.
- Controller host + port (for Vector's cloud-init log
  stream).
- Docker registry address (from operator config).
- VictoriaMetrics push URL (from operator config).
- Operator-supplied additional cloud-init fragments (if any)
  — spliced in after the hyperplane-required sections.

**Rust-port divergence from the Java contract.** The Java
contract required OpenJDK 25 on the node (because the Java
agent needed a JVM). The Rust port's agent is a
statically-linked musl binary with no runtime dependencies —
cloud-init does not install a JVM, Java, or any language
runtime for the agent. Workload containers bring their own
runtimes.

## D6 — SSH key management

Two distinct keys are used in different phases:

| Key | Purpose | Lifetime | Storage |
|---|---|---|---|
| Provisioning keypair | User-specified `key_pair` in AWS. Used as the cloud-init boot key. | Long-lived; operator-owned. | AWS (controller does not hold the private key). |
| Hyperplane deploy key | Ephemeral; generated by controller at provisioning time, used to SSH in for the agent deploy (SRD-0105 D2), discarded after deploy completes. | Seconds to minutes. | Controller's credential store while in-use; deleted after deploy or node teardown. |

The deploy-key flow:

1. Controller generates an Ed25519 keypair.
2. Public key goes into the cloud-init user-data (dropped
   into `authorized_keys` on the node).
3. Controller uses the private key for the single SSH deploy
   session (SRD-0105 D2).
4. Post-deploy, controller deletes the private key from the
   credential store. The public key remains in
   `authorized_keys` on the node; revoking access would
   require an SSH session we don't have (per
   `INV-AGENT-SSH-ONCE`), so in practice revocation happens
   at node-termination time.

**Agent privilege on the node.** The agent runs as a
dedicated `hyperplane` system user. It is granted exactly one
sudo rule:

```
%hyperplane ALL=(root) NOPASSWD: /sbin/shutdown -h now, /bin/systemctl poweroff
```

No general sudo. Enough to honour `Shutdown` commands
(SRD-0105 D11) and nothing else.

## D7 — Node state machine

The node's state is a paramodel `OperationalState` projection
shaped by AWS state, cloud-init status, deploy status, and
agent status. Values and transitions are ported from the
Java `NodeStatus` machine (operational order in parentheses):

```
PROVISIONING       (1) → PROVISIONED, PROVISIONING_FAILED, TERMINATED
PROVISIONED        (2) → CONFIGURING, TERMINATED
CONFIGURING        (3) → CONFIGURED, CONFIGURING_FAILED, TERMINATED
CONFIGURING_FAILED (-3)→ CONFIGURING (retry), TERMINATED
CONFIGURED         (4) → DEPLOYING, TERMINATED
DEPLOYING          (5) → DEPLOYED, DEPLOYING_FAILED, TERMINATED
DEPLOYING_FAILED   (-4)→ DEPLOYING (retry), TERMINATED
DEPLOYED           (6) → REGISTERING, DEPLOYING, TERMINATED
REGISTERING        (7) → REGISTERED, REGISTERING_FAILED, TERMINATED, DEPLOYING
REGISTERING_FAILED (-5)→ REGISTERING (retry), TERMINATED, DEPLOYING
REGISTERED         (8) → ACTIVE_HEARTBEAT, LOST_HEARTBEAT, TERMINATED, DEPLOYING
ACTIVE_HEARTBEAT   (9) → AWAITING_HEARTBEAT, LOST_HEARTBEAT, TERMINATED, DEPLOYING
AWAITING_HEARTBEAT (10)→ ACTIVE_HEARTBEAT, LOST_HEARTBEAT, TERMINATED, DEPLOYING
LOST_HEARTBEAT     (-6)→ ACTIVE_HEARTBEAT, REGISTERING, TERMINATED, DEPLOYING
TERMINATED         (0) → (terminal)
```

**Transition triggers (summary).**

| Transition | Trigger |
|---|---|
| `PROVISIONING → PROVISIONED` | AWS `DescribeInstances` shows `running`. |
| `PROVISIONED → CONFIGURING` | Cloud-init log stream opens (Vector connects to controller). |
| `CONFIGURING → CONFIGURED` | Cloud-init `status: done`. |
| `CONFIGURING → CONFIGURING_FAILED` | Cloud-init `status: error`. |
| `CONFIGURED → DEPLOYING` | Controller initiates SSH deploy (SRD-0105 D2). |
| `DEPLOYING → DEPLOYED` | Agent binary uploaded + systemd unit enabled. |
| `DEPLOYED → REGISTERING` | WebSocket connection from the agent arrives. |
| `REGISTERING → REGISTERED` | Agent's `Register` message validated, `agents` row flips. |
| `REGISTERED → ACTIVE_HEARTBEAT` | First heartbeat received. |
| `ACTIVE_HEARTBEAT ↔ AWAITING_HEARTBEAT` | Heartbeat-due vs heartbeat-received (SRD-0105 D8). |
| `* → LOST_HEARTBEAT` | 30s without inbound agent traffic. |
| `* → TERMINATED` | AWS state = `terminated` or `shutting-down`. |

Every transition emits a `NODE_STATUS_CHANGED` event on the
stream (SRD-0111), carrying `from`, `to`, node ID as subject,
and a correlation to the triggering cause (AWS describe
response, agent message id, SSH session id, etc.).

## D8 — Paramodel runtime binding

The EC2Node kind's `ElementRuntime` maps paramodel hooks to
the state machine:

| Hook | Behaviour |
|---|---|
| `materialize(resolved) -> MaterializationOutputs` | Execute D3 provisioning flow; block until the node reaches `ACTIVE_HEARTBEAT`; return `instance_id`, `public_ip`, `private_ip`, `node_name`. Timeout after 10 minutes (configurable); a timeout transitions the node to the appropriate `*_FAILED` state and returns the paramodel error. |
| `status_check() -> LiveStatusSummary` | Returns `(node_state, last_heartbeat_ts)`. Does not make AWS calls (avoids `DescribeInstances` rate-limit pressure); reads the in-controller node record, which is kept current by the sync loop. |
| `dematerialize() -> Result<()>` | Send `Shutdown` command to the agent via WebSocket (SRD-0105 D11); observe the node transition to `TERMINATED` via `DescribeInstances` polling; return when AWS confirms `terminated`. Also deletes the deploy key from the credential store. |
| `on_trial_starting` / `on_trial_ending` | No-op for nodes. Nodes host trials; they don't have per-trial lifecycle hooks. |
| `observe_state(listener)` | Subscribes to the node's state-transition stream (internally sourced from the `NODE_STATUS_CHANGED` events). Delivers a synthetic initial `(Unknown → current)` transition on subscribe, per the paramodel contract. |

## D9 — Error recovery

**Cloud-init failure** (`CONFIGURING_FAILED`). Controller
surfaces the last N lines of the cloud-init log (captured via
Vector) in the error event. Retry path: operator investigates,
either edits the AMI / profile or requests a re-provision
(new instance; the failed one is terminated, not repaired).

**Agent deploy failure** (`DEPLOYING_FAILED`). SSH connection
refused, binary verification mismatch, systemd unit install
error, etc. Controller records the specific step that failed.
Retry: re-run deploy against the same instance (goes back to
`DEPLOYING` per the state machine); if repeated deploys fail,
terminate and re-provision.

**Registration timeout** (`REGISTERING_FAILED`). Agent was
deployed but did not register within 120 seconds (SRD-0105 D2).
Usually indicates a network-reachability issue (instance can't
reach the controller) or agent-crash at startup. Retry:
restart the agent via a second SSH session (requires explicit
operator action since the once-only deploy key is gone —
operator reaches in with their own key) or re-deploy from
scratch.

**Heartbeat loss** (`LOST_HEARTBEAT`). The node-lifecycle
machine does not auto-tear the node down on heartbeat loss;
it marks the node non-ready in the topology view. Recovery
paths: agent reconnects (back to `REGISTERING` → `REGISTERED`
→ `ACTIVE_HEARTBEAT`), or the operator decides to terminate.

**AWS-side failure during `RunInstances`.** Capacity
exhaustion, quota limits, invalid AMI-for-region — surface
as `PROVISIONING_FAILED` with the AWS error detail. No
auto-retry across different AZs or instance types.

## D10 — Observability ties

Events emitted specifically by the EC2Node runtime (in
addition to the generic `NODE_STATUS_CHANGED`):

| Event | When |
|---|---|
| `NODE_ADDED` | Row inserted post-`RunInstances`. |
| `NODE_REMOVED` | Row hard-deleted post-`TERMINATED` + retention window. |
| `SpotInterruptionImminent` | Agent observes the AWS instance-metadata interruption notice. |
| `CloudInitFailure` | Cloud-init log stream emits an `error` status. |
| `DeployFailure` | SSH deploy step fails; payload carries which step. |

All events carry node id as `subject.node_id`. Subscribers
filter by subject to follow a specific node.

## D11 — No new invariants

All invariants that apply to EC2Node are already covered
upstream (`INV-CTL-SOLE-WRITER` for node-row writes,
`INV-AGENT-SSH-ONCE` for post-deploy SSH discipline,
`INV-LIFECYCLE-INDEPENDENT` for not tearing the node down on
controller restart). EC2Node does not add its own.

## Design rulings (resolved)

- **Allowed node types and images come from a required
  allow-list.** See D2.
- **Spot vs on-demand is a configuration parameter.** See
  D4.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/docs/NODE-CONTRACT.md` — Java-era
  node contract, ported in D5 with the Java-runtime
  dependency dropped.
- `~/projects/hyperplane/docs/NODE-LIFECYCLE.md` — Java-era
  state machine, ported verbatim in D7 (Rust-port
  implementation fills in the same transitions).
- `~/projects/hyperplane/cloud-elements-ec2/` — Java
  reference implementation.
- `aws-sdk-ec2` crate — Rust AWS SDK.
