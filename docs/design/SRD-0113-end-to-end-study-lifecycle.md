<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0113 — End-to-End Study Lifecycle

## Purpose

Tie every other hyperplane SRD together into a single
coherent user story: from `hyper system init` through
authoring a test plan through provisioning, execution,
observation, result persistence, and finally result query.
Functions as the integrative conformance target — if the
stack passes this, the stack works.

This SRD is a narrative. It cross-references every SRD it
touches rather than restating content. Its job is to show
that the pieces fit; the pieces themselves are owned by
their SRDs.

## Scope

**In scope.**

- Actor vocabulary grounded in SRD-0114's role model.
- The golden-path scenario — one study, one plan, one
  execution, a queryable result set.
- State transitions across subsystems as the scenario
  proceeds.
- Failure-mode catalogue + expected system response.
- The conformance suite — a multi-element study exercising
  every first-party element kind, retry + timeout + skip +
  checkpoint + resume cascades, ending with queryable
  results.

**Out of scope.**

- Implementation details of any single subsystem (those are
  the other SRDs).
- Report generation — "reports" are externally produced from
  queried result data (D7); hyperplane's owned surface is
  the result + artifact + metric stores.

## Depends on

Every other hyperplane SRD. This one's the capstone.

- SRD-0100 (invariants).
- SRD-0101 (state boundaries).
- SRD-0102 (element kinds).
- SRD-0103 (image param extraction).
- SRD-0104 (EC2Node).
- SRD-0105 (agent + control channel).
- SRD-0106 (ServiceDocker).
- SRD-0107 (CommandDocker).
- SRD-0108 (controller API).
- SRD-0109 (CLI).
- SRD-0110 (web UI).
- SRD-0111 (events + observability).
- SRD-0112 (system service + config).
- SRD-0114 (principals + roles).
- Paramodel SRDs 0002–0014.

---

## Golden path at a glance

![Golden-path study lifecycle in six phases: (1) authoring — register image, write plan, validate, submit; (2) execution start — compile Atomic Step Graph, EXECUTION_STARTED; (3) provisioning — Deploy host (EC2Node materialize, RunInstances), Deploy host-agent (SSH deploy once, WebSocket opens, Register); (4) workload — Deploy harness (ServiceDocker with healthcheck gate), Deploy client (CommandDocker), Await client exit; (5) teardown — SaveOutput, Teardown harness, Teardown host (Shutdown, AWS terminates); (6) query — inspect via CLI or web UI.](diagrams/SRD-0113/golden-path.png)

## Subsystem coupling at a glance

![Subsystem coupling within one execution: paramodel state machine drives three hyperplane subsystems — node lifecycle (SRD-0104), agent lifecycle (SRD-0105), container state (SRD-0106/0107). All three emit events to the unified event stream (SRD-0111). End-users observe via CLI (tail + query), Web UI (topology + execution detail), or SDK (programmatic). Authz gates everything via principal (SRD-0114), token scope, and resource_shares.](diagrams/SRD-0113/subsystem-coupling.png)

## D1 — Actors

Grounded in SRD-0114's role model — no separate persona
vocabulary, no role duplication. SRD-0114 defines three
role values (viewer, user, admin) that any principal may
toggle among, additive + non-hierarchical. Personas in this
SRD describe *what a principal is doing at a moment*, not
separate auth identities.

| Persona | Active role | Entry points | What they do |
|---|---|---|---|
| Operator | admin | CLI + web UI; direct system access | Bootstraps the install (`hyper system init`), manages users, provisions allow-lists, responds to incidents. |
| Study author | user | CLI + web UI | Authors test plans, registers images, submits executions. |
| Result consumer | viewer / user | Web UI (primary), CLI | Queries results, inspects executions, tails logs. |
| Agent-principal | agent (machine role) | WebSocket handshake | Executes controller commands against its node's Docker daemon; reports back. |

Same principal, different active roles at different moments.
An operator can toggle to `user` to author a plan, back to
`admin` for an administrative action. The CLI + web UI
surface the role toggle (SRD-0114 D4).

## D2 — Prerequisites

Before any study runs, the install is prepared:

1. **Install the stack.** Operator runs
   `hyper system install` (SRD-0112 D6) — creates service
   user, drops systemd units, sets up directories.
2. **Initialize.** `hyper system init` (SRD-0112 D7) — runs
   migrations, mints the system API key + bootstrap admin
   token, creates the initial admin user.
3. **Operator logs in.** `hyper login` with the bootstrap
   token credentials (SRD-0109 D3) stores a bearer token in
   XDG credentials.
4. **Allow-lists.** Operator edits
   `ec2-node-allowlist.toml` (SRD-0104 D2) with permitted
   instance types + AMIs.
5. **AWS credentials.** Configured per SRD-0112 D5 —
   environment variables, profile, or instance role.
6. **Users created.** Operator creates study-author user
   accounts via `hyper admin users create` (SRD-0114).
7. **Start.** `hyper system start` (SRD-0112 D3) brings the
   stack up: controller, web server, optional co-resident
   registry.

Invariants checked at start: compile-time dependency-graph
enforcement (`INV-CTL-SOLE-WRITER`,
`INV-NON-CTL-NO-PERSISTENCE`, `INV-WEB-DEP-GRAPH`); runtime
port binding (`INV-PORT-BIND-FATAL`); config strictness
(`INV-CONFIG-STRICT`); migrations succeeded.

## D3 — The golden path

Scenario: a study author runs one study that provisions one
EC2Node, launches one ServiceDocker (a vector database),
runs a CommandDocker (a benchmark client) against it,
captures results.

### Phase 1: Plan authoring

1. Study author, logged in via `hyper login`, registers two
   images:
   - `hyper image register vector-harness:1.0` — a
     ServiceDocker image with `hyperplane_mode=service`,
     `hyperplane_api=vector-harness`. Per SRD-0103, the
     controller pulls the image manifest, extracts
     `@param` annotations, caches the `ParamSpace` by
     digest.
   - `hyper image register vector-client:1.0` — a
     CommandDocker image with `hyperplane_mode=command`.
     `@param` + `@result` annotations extracted; cached by
     digest.
2. Study author writes a TestPlan YAML declaring:
   - One EC2Node element `host` with `instance_type`,
     `ami`, `region` from the allow-list, a `hosts`
     socket.
   - One Agent element `host-agent`,
     `commands-on` socket offered, `deploys-onto` plug
     into `host.hosts`.
   - One ServiceDocker element `harness`, image
     `vector-harness:1.0`, `commands-on` plug into
     `host-agent.commands-on`, `depends-on-service`
     socket offered.
   - One CommandDocker element `client`, image
     `vector-client:1.0`, `commands-on` plug into
     `host-agent.commands-on`, `depends-on-service` plug
     into `harness.depends-on-service` via `Linear`
     relationship.
   - Axes: parameter sweeps (e.g. dataset sizes, index
     configurations) on client parameters.
3. Study author validates:
   `hyper plan validate plan.yaml` — paramodel's compiler
   (SRD-0010) checks labels, plug/socket compatibility,
   parameter domains, `ShutdownSemantics` match (per
   SRD-0014 D4). Compile errors surface inline.
4. Study author submits: `hyper plan submit plan.yaml` —
   persists as a paramodel `PlanSpec` (SRD-0101 D1).

### Phase 2: Execution start

1. Study author starts: `hyper execution start <plan-id>`.
2. Controller's executor reads the plan, compiles it to the
   Atomic Step graph (SRD-0009 / SRD-0010). Each trial's
   sequence is:
   - `Deploy(host)` → `Deploy(host-agent)` →
     `Deploy(harness)` → `Await(harness healthy)` →
     `Deploy(client)` → `Await(client exits)` →
     `SaveOutput(client)` → `Teardown(harness)` →
     `Teardown(host-agent via host)` → `Teardown(host)`.
3. `EXECUTION_STARTED` event emitted (SRD-0111 D3).
4. Executor invokes each step in order. For trial #1:

### Phase 3: Provisioning + registration

1. `Deploy(host)` triggers EC2Node.materialize (SRD-0104
   D3, D8):
   - `RunInstances` with the resolved params.
   - Node row inserted, state
     `PROVISIONING`.
   - Every transition emits
     `NODE_STATUS_CHANGED`.
2. Cloud-init runs per SRD-0104 D5: LVM, Docker,
   observability mesh, chrony, SSH public key. Vector
   streams cloud-init logs to the controller's
   `/api/v1/nodes/{id}/cloudinit/logs` WebSocket
   (SRD-0108) — visible in the web UI node detail page.
3. `Deploy(host-agent)` triggers the SSH deploy (SRD-0105
   D2):
   - Controller mints `auth-token`, connects via SSH,
     uploads `hyperplane-agent` binary, writes
     `agent.toml`, installs + starts the systemd unit.
   - Agent comes up, opens WebSocket to the controller
     (SRD-0105 D4), sends `Register` (D5).
   - Controller records the agent, node transitions to
     `REGISTERED` then (on first heartbeat)
     `ACTIVE_HEARTBEAT`.

### Phase 4: Workload deployment

1. `Deploy(harness)` triggers ServiceDocker.materialize
   (SRD-0106 D3):
   - Controller resolves image digest, constructs
     `EnsureContainerRunning` spec with container name
     `hyperplane-service-{user}-{exec-id-8}-harness` and
     the full runtime label set (SRD-0106 D10).
   - Agent pulls image, `docker run -d`, starts log
     capture.
   - `HEALTHCHECK` polls until the container reports
     `healthy`; materialize unblocks.
2. `Deploy(client)` triggers CommandDocker.materialize
   (SRD-0107 D3):
   - Similar spec; agent runs container foreground, tees
     stdout + stderr to files, heartbeats resource state
     to the controller.
3. `Await(client exits)` blocks until the client exits.
   Agent packages payload (SRD-0107 D3 step 8):
   - Uploads `stdout.log`, `stderr.log`, any other files
     under `/hyperplane/out/` as paramodel artifacts.
   - Reads `result_parameters.json`, parses against the
     image's `@result` declarations.
   - Returns `CommandResponse` with `exit_code=0` +
     `result_parameters`.

### Phase 5: Save output + teardown

1. `SaveOutput(client)` persists `result_parameters` as
   `TrialMetrics` via paramodel's `ResultStore` (SRD-0012).
2. `Teardown(harness)` triggers
   ServiceDocker.dematerialize: agent stops + removes the
   container.
3. `Teardown(host-agent via host)` is a no-op — Agent's
   lifecycle is bound to its node, so tearing the Agent
   happens as a side-effect of tearing the node.
4. `Teardown(host)` triggers EC2Node.dematerialize
   (SRD-0104 D8): controller sends `Shutdown` to the
   agent's WebSocket, agent invokes
   `systemctl poweroff`, AWS state transitions to
   `terminated`, node row marked
   `TERMINATED`.

### Phase 6: More trials

The above cycle repeats per axis-enumerated trial (per
paramodel's reducto; SRD-0002 / SRD-0009). Every trial gets
its own EC2Node instance (fresh `instance_id`) and fresh
container names (`{exec-id-8}` is constant; element
instance suffix varies if `max_concurrency > 1`).

`max_group_concurrency` / `max_concurrency` (paramodel
algebra) caps parallel trials. Operators see a running
topology view in the web UI with N active nodes + their
containers.

### Phase 7: Result query

After the execution completes:

1. Study author / result consumer runs `hyper execution
   show <id>` for summary metrics or opens the execution
   detail page in the web UI.
2. Result consumer queries results:
   - `hyper execution results <id>` → JSON stream of
     `TrialResult` rows.
   - Web UI `/executions/{id}` → tabular view + artifact
     links.
   - `hyper artifact get <id>` to pull an individual
     artifact (stdout.log, result dumps).
3. External analysis tools (notebooks, BI dashboards)
   consume the API as an SDK (SRD-0108's bearer-token
   model) for cross-execution queries.

## D4 — State transitions summary

Six subsystems in parallel, each with its own state
discipline:

| Subsystem | States | Source SRD |
|---|---|---|
| Node | PROVISIONING → CONFIGURING → CONFIGURED → DEPLOYING → DEPLOYED → REGISTERING → REGISTERED → ACTIVE_HEARTBEAT → (LOST_HEARTBEAT) → TERMINATED | SRD-0104 D7 |
| Agent process | starting → registering → ready → (draining) → dead | SRD-0105 D7 |
| Service container | (absent) → pulling → running → healthy → (dematerializing) → (absent) | SRD-0106 D6 |
| Command container | (absent) → pulling → running → exited | SRD-0107 D8 |
| Execution | submitted → running → (paused) → completed / failed / cancelled | paramodel SRD-0011 |
| Trial | pending → starting → running → completed / failed / skipped | paramodel SRD-0011 |

Each transition emits an event (SRD-0111 D3). The web UI's
topology view (SRD-0110 D6) renders all six streams
composed — a single browser view shows "trial 7 is
running, on node-abc, in container hyperplane-command-..."

## D5 — Failure modes

### Node provisioning failure

AWS rejects `RunInstances` (capacity, bad AMI, quota). Node
transitions to `PROVISIONING_FAILED`. Executor marks the
step failed; per plan retry policy (paramodel), trial may
retry on a different AZ or skip.

### Cloud-init failure

Boot script errors (LVM failure, package install error).
Vector captured the logs before failure; node state
transitions to `CONFIGURING_FAILED`. Operator investigates
via the captured cloud-init artifacts; re-provision is
typically the fix.

### Agent deploy failure

SSH connect fails, binary verification mismatches, systemd
install errors. Node transitions to `DEPLOYING_FAILED`.
Retry path: re-attempt the deploy (requires operator
intervention since deploy key is ephemeral).

### Agent heartbeat loss

30 seconds without agent traffic → `LOST_HEARTBEAT`
(SRD-0105 D8). The node is not auto-torn-down
(per `INV-LIFECYCLE-INDEPENDENT`); it's marked non-ready.
The agent reconnects automatically per SRD-0105 D9's 15-
minute budget; if it doesn't, the node is marked failed
and the operator decides whether to terminate.

### Controller restart mid-run

`INV-LIFECYCLE-INDEPENDENT` holds: running containers on
nodes keep running under Docker's own supervision; agents
keep their WebSockets (or reconnect, per SRD-0105 D9);
paramodel's resume story (SRD-0011) picks up where it left
off using checkpoints. No double-provisioning, no orphaned
containers.

### Image pull failure

ServiceDocker or CommandDocker can't pull — auth denied,
image gone from registry, network to registry blocked.
Trial fails with `ImagePullFailed`. Per SRD-0106 D11 the
error event surfaces on the topology view; subsequent
trials against the same image also fail until the registry
is fixed.

### Health-check timeout

ServiceDocker doesn't report healthy within the configured
timeout. `materialize` returns `HealthCheckTimeout`; the
container is left running for operator inspection (until
`dematerialize` is called). Trial fails.

### Command exit non-zero

CommandDocker exits with code ≠ 0. `Await` returns
`StepOutcome::Failed { code: ExitCode(n) }`. Per plan
retry policy, trial may retry; stdout/stderr artifacts
remain for diagnosis.

### Spot interruption

Agent sees AWS's 2-minute warning via instance-metadata
poll. Emits `SpotInterruptionImminent`. Per operator
policy, executor may re-run affected trials on new nodes.

### Disk / volume full

Agent surfaces via Docker events (container OOM, disk
pressure). Typically surfaces as a container exit; same
path as other command exits.

## D6 — Conformance scenario

The acceptance test for the Rust port: a concrete plan
exercising every element kind + several failure paths, run
end-to-end, asserted on.

**Plan shape.**

- 3 trials, each binding 2 axes (e.g. dataset size ×
  concurrency level).
- 1 EC2Node per trial (allow-listed instance type).
- 1 Agent per node.
- 1 ServiceDocker (long-running vector harness).
- 1 CommandDocker (benchmark client, declares 3 `@result`
  parameters).

**Assertions.**

1. Plan validates cleanly.
2. Execution starts; event stream emits
   `EXECUTION_STARTED`.
3. Three nodes provision concurrently (per paramodel's
   concurrency model; `max_concurrency=3` at the node
   level).
4. All three agents register within 120s each.
5. All three harnesses report healthy within their
   respective timeouts.
6. All three clients run to exit; exit codes all 0.
7. All three `SaveOutput` steps produce complete
   `result_parameters`.
8. All three trials teardown to `TERMINATED`.
9. Event stream contains every expected `STATUS_CHANGED`
   + `CONTAINER_STATUS_CHANGED` + `STEP_*` transition;
   replay from `since=0` reconstructs the full state.
10. Artifact store contains stdout.log, stderr.log, and
    any declared `output_files` for each command.
11. Result store contains three `TrialResult` rows, each
    with the declared `result_parameters` as typed
    metrics.
12. Every TCK invariant (SRD-0100 D11 + per-SRD
    extensions) passes its conformance check.

**Failure-path additions** (run as separate scenarios):

- Inject an image-pull failure on trial 2 — assert
  `ImagePullFailed` surfaces, trial 2 fails, trials 1 + 3
  complete.
- Inject controller restart mid-execution — assert paramodel
  resume picks up cleanly, no trial is double-started.
- Inject agent disconnect mid-trial — assert heartbeat
  loss surfaces without tearing the node; agent reconnects
  within its budget; trial completes.

## D7 — Reports

Hyperplane persists `TrialResult`, `Artifact`,
`JournalEvent`, `Event`, and metric time-series — that's
the owned surface. **Reports are not a first-class
system concept.**

Result consumers generate reports externally by querying:

- Controller API — structured result data + artifacts +
  event stream. Python / Go / etc. SDK clients consume
  this.
- Metrics backend — time-series queries for run
  performance.
- Artifact contents — captured stdout/stderr + command
  output files.

Operators building a dashboarding layer do so with a
notebook, a BI tool, or a bespoke reporter binary that
reads from these surfaces. The system's job is to make the
underlying data queryable, typed, and complete — not to
render report artifacts.

**Why this split.** "Reports" tend to sprawl — per-team
templates, per-audience framings, delivery-channel
diversity (email, dashboards, PDF, CI comment). Embedding
that in the controller would balloon the surface area.
Keeping reports external keeps the query surfaces clean
and lets report-tooling evolve independently.

## D8 — Parity proof

Every action in Phase 1–7 of the golden path (D3)
reachable from both the CLI and the web UI:

| Action | CLI | Web UI |
|---|---|---|
| Register image | `hyper image register` | `/images` → Register |
| Validate plan | `hyper plan validate` | `/plans/{id}` validation panel |
| Submit plan | `hyper plan submit` | `/plans` → Create |
| Start execution | `hyper execution start` | `/plans/{id}` → Run |
| Observe state | `hyper execution events --tail` | `/executions/{id}` live panel |
| Query results | `hyper execution results` | `/executions/{id}` results tab |
| Pull artifact | `hyper artifact get` | artifact link in UI |
| Terminate node | `hyper node terminate` | `/nodes/{id}` → Terminate |

`INV-PARITY` (SRD-0100 D8) holds: both surfaces are clients
of the controller API; every action exists because a
corresponding endpoint exists (SRD-0108 D4); parity is
structural.

## D9 — Cross-invariant check

The scenario exercises — and thus tests — every
cross-cutting invariant:

| Invariant | Exercise |
|---|---|
| `INV-CTL-SOLE-WRITER` | Every mutation rides a controller API call (writes from agents, from web, from CLI all route through `/api/v1/*`). |
| `INV-AGENT-PEER` / `INV-CTL-AGENT-CHANNEL` | Agent traffic all rides the one WebSocket. No agent-web-server edge. |
| `INV-LIFECYCLE-INDEPENDENT` | Controller restart mid-run test. |
| `INV-PARITY` | D8 table. |
| `INV-EVENT-IMMUTABLE` / `INV-EVENT-PERSIST-ALL` | Replay from `since=0` reconstructs state. |
| `INV-HYPERPLANE-NAMESPACE` | All labels + parameters in the scenario use `hyperplane_*` snake_case. |
| `INV-COMMAND-IDEMPOTENT` | Controller restart mid-deploy test asserts no double-start. |
| `INV-PARAMSPACE-DETERMINISTIC` | Two executions of the same plan against the same image digest produce identical binding validation. |
| `INV-ELEMENT-KIND-SHUTDOWN-SEMANTICS` | Plan compilation rejects a misdeclared element's shutdown_semantics. |
| `INV-WRITE-EMITS-EVENT` | Every write in Phase 1–7 produces an event; replay reconstructs. |

## D10 — Scenario status

| Stage | Description | Captured by |
|---|---|---|
| Concept | This SRD's Phase 1–7 narrative. | D3 |
| Contract | Machine-readable: OpenAPI schemas + event-type catalogue + TCK harness. | SRD-0108 D10 + SRD-0111 D3 |
| Execution | Running the conformance suite end-to-end. | CI task against a live stack. |

The TCK harness that runs this scenario is the literal
implementation of "does hyperplane work." Missing a step
here means a gap in either the SRDs or the implementation.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/docs/narratives/` — Java-era
  user-story drafts; intent ported here.
- `~/projects/hyperplane/docs/studies/study_system.md` —
  study composition specification; informs Phase 1
  authoring flow.
- Every other hyperplane SRD.
