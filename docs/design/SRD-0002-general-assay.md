<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0002 — General Assay of the Reference Projects

## 1. Purpose

This SRD is the opening survey of the two reference projects —
`links/paramodel/` and `links/hyperplane/` — mapping their concepts, modules,
semantics, and external touchpoints. It is **not** a Rust design; it is the
shared vocabulary and scope inventory we will refer back to from every later
SRD.

The outputs of this SRD are:

1. A catalog of the **domains, layers, and concepts** we intend to port.
2. A **proposed crate layout** for `hyperplane-rs` as a Cargo workspace.
3. A list of **tensions and open questions** that must be resolved in
   per-aspect SRDs before implementation can proceed on those aspects.

Nothing in this SRD is binding on implementation beyond the crate layout and
the scope fences (§5). The algebraic, protocol, and executor designs each
get their own SRDs (§8).

## 2. Scope & non-scope

**In scope for this SRD.**
- Identifying the layers of the source system: paramodel algebra, Simplica
  plan/execution, Hyperplane bridge types, Hyperplane control plane, CLI,
  web console, containerized elements.
- Naming the core entities and relationships.
- Proposing the top-level crate boundaries in `hyperplane-rs`.
- Flagging places where upstream design is unstable, contradictory, or
  upstream code and upstream docs disagree.

**Not in scope for this SRD.**
- Specifying Rust trait shapes, method signatures, error types, or async/sync
  policy. Each aspect gets its own SRD.
- Redesigning any part of the system. Where we see opportunities (e.g.
  choosing a different error model, replacing dynamic dispatch with enum
  sealing) we record them under open questions, not decisions.
- Non-functional commitments (performance, binary size, MSRV) — separate SRD.

## 3. Background: what the upstream projects are

### 3.1 Paramodel (contract-first parameter-modeling framework)

Source: `links/paramodel/`. Java 25 multi-module Maven project.

Paramodel is a framework for defining parameter spaces and compiling them
into executable study plans. It has four sibling modules under one umbrella:

| Module | Role |
|--------|------|
| `paramodel-api` | Pure interfaces/records. No implementation code. |
| `paramodel-engine` | Default implementations: compiler pipeline, executor, scheduler, binder. |
| `paramodel-mock` | Trivial in-memory implementations for tests/examples. |
| `paramodel-tck` | Technology Compatibility Kit: test suite that validates implementations against the contracts. |
| `paramodel-sims` | Simulation-only element implementations (e.g. `DummyElement`). |

Paramodel partitions its API across packages
(`links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/…`):

- `parameters` — `Parameter<T>`, `Domain<T>` (sealed: Discrete, Range,
  Composite, Custom), `Constraint<T>`, `Value<T>`, `ValidationResult` (sealed:
  Passed, Failed, Warning), `DerivedParameter<T>`, `BindingNode`,
  `ParameterBinder`, `ParameterBinding`, `BindingPolicy`,
  `SamplingStrategy`, `DynamicParameterResolver`, `ElementBindingTree`.
- `parameters/types` — concrete built-ins (`IntegerParameter`,
  `DoubleParameter`, `BooleanParameter`, `StringParameter`,
  `SelectionParameter`).
- `trials` — `Trial`, `TrialBuilder`, `TrialSet`, `TrialSetBuilder`,
  `TrialResult`, `TrialStatus`.
- `plan` — `TestPlan`, `TestPlanBuilder`, `ExecutionPlan`, `Axis<T>`,
  `AtomicStep` (sealed: DeployElement, TrialStep, AwaitElement,
  TeardownElement, BarrierSync, CheckpointState, NotifyTrialStart,
  NotifyTrialEnd), `ExecutionGraph`, `Barrier`, `TrialOrdering`,
  `OptimizationStrategy`, `StepStatus`, `ExecutionState`,
  `ImmutableExecutionState`, `LiveElementGraph`, `LiveExecutionGraph`,
  `ElementInstanceGraph`, `ExecutionPlanMetadata`, `TrialOrdering`
  (SEQUENTIAL, SHUFFLED, EDGE_FIRST, DEPENDENCY_OPTIMIZED, COST_OPTIMIZED,
  CUSTOM), and `plan.policies.ExecutionPolicies`.
- `elements` — `Element`, `RelationshipType` (SHARED, EXCLUSIVE, DEDICATED,
  LINEAR, LIFELINE), `ElementFactory`, `ElementPrototype`, `ElementProvider`,
  `ElementTypeDescriptor`, `ElementTypeDescriptorProvider`,
  `DependencyRequirement`, `TrialContext`, `TrialLifecycleParticipant`,
  `OperationalStateObservable`.
- `compilation` — `Compiler`, `CompilationContext`, `CompilationStage`,
  `OptimizationPass`.
- `execution` — `Executor`, `Runtime`, `Scheduler`, `ResourceManager`,
  `ArtifactCollector`, `ExecutionStateManager`, `execution/journal/*`.
- `attributes` — three-tier metadata system: `Labeled`, `Traits`, `Tagged`,
  records `Label`/`Trait`/`Tag`, and `AttributeSupport` with a namespace
  uniqueness rule across tiers.
- `persistence` — stores: `ArtifactStore`, `CheckpointStore`,
  `ExecutionRepository`, `JournalStore`, `MetadataStore`, `ResultStore`.
- `security` — `AccessControl`, `AuditLog`, `CredentialManager`.
- `util` — `ConfigurationManager`, `SerializationUtil`, `ValidationUtil`.

The engine (`paramodel-engine`) organizes itself differently:

- `engine.compiler` — the 8-stage compilation pipeline:
  `ValidationStage`, `NormalizationStage` (implicit, via
  `AxisExpander`), `TrialEnumerationStage`, `InstantiationStage`,
  `StepGenerationStage`, `DependencyAnalysisStage`, `OptimizationStage`,
  `CodeGenerationStage`.
- `engine.execution` — `DefaultExecutor`, `DefaultScheduler`,
  `DefaultResourceManager`, `DefaultExecutionStateManager`, and
  journal subpackage (`DefaultJournalStateReconstructor`,
  `ExecutionSnapshot`, `JournalWriter`, `InFlightStepResolver`).
- `engine.binding` — `DefaultParameterBinder`, `DefaultParameterBinding`,
  `DefaultBindingNode`, `DefaultElementBindingTree`.
- `engine.plan` — `DefaultTestPlan`, `DefaultAxis`, `DefaultElement`,
  `PlanAxis`, `AxisParameter`.
- `engine.definition` — YAML parsing: `TestPlanDefinition`,
  `TestPlanDefinitionParser`, `TestPlanComposer`.
- `engine.planners` — including `reducto` (rule-based reduction planner) and
  `simple`.
- `engine.sequence` — `DefaultTrial`, `DefaultValue`.

Key references consulted:

- `links/paramodel/README.md`
- `links/paramodel/ARCHITECTURE.md`
- `links/paramodel/docs/index.md`
- `links/paramodel/docs/explanation/design-principles.md`
- `links/paramodel/docs/explanation/architecture.md`
- `links/paramodel/docs/explanation/simplica.md`
- `links/paramodel/docs/explanation/immutability-and-reproducibility.md`
- `links/paramodel/docs/concepts/*.md` (all seven)
- `links/paramodel/docs/reference/api-packages.md`
- Java source under `links/paramodel/paramodel-api/src/main/java/...` (sampled
  extensively for signatures, especially `Parameter`, `Domain`, `Element`,
  `TestPlan`, `ExecutionPlan`, `AtomicStep`, `Executor`, `Scheduler`,
  `Runtime`).

### 3.2 Hyperplane (the control plane that uses paramodel)

Source: `links/hyperplane/`. Java 25 multi-module Maven project.

Hyperplane is a hub-and-spoke orchestration + benchmarking platform for vector
search workloads. It consumes `paramodel-api` and `paramodel-engine` to
express its studies, and adds a concrete control plane: a controller hub,
agents, a web console, a CLI, and Dockerfile-derived element types.

Its modules (as observed):

| Module | Role |
|--------|------|
| `hyperplane-cli` | `picocli`-based unified CLI (`hyperplane …`) that invokes controller APIs and runs local system services. |
| `hyperplane-controller` | Authoritative hub: HTTP/WebSocket server, SQLite `ProvisioningTracker`, paramodel bridge, SSH deployment, agent protocol, plan executor, EC2 orchestrator. |
| `hyperplane-webconsole` | Browser-facing reverse proxy + UI. Stateless. Only talks to the controller. |
| `cloud-elements-ec2` | Concrete paramodel `Element` implementations for AWS EC2 nodes and Docker containers (`Ec2NodeElement`, `DockerContainerElement`, factories). |
| `diagnostic-elements` | Diagnostic paramodel `Element` implementations (`DiagnosticElement`, type provider). |
| `containerdefs/` | Not a Maven module. Holds Dockerfiles with `@param`/`@result` annotations and Docker labels (`com.hyperplane.api`, `com.hyperplane.mode`). These Dockerfiles are *the data* that drives dockerfile-derived parameter extraction. |

Key concept clusters within hyperplane:

- **Control plane** — `ControllerMain`, `ControllerServer`, `AgentClient`,
  `AgentDockerService`, the protocol package under
  `hyperplane-controller/.../protocol/` (sealed `Message` hierarchy:
  `RegisterMessage`, `RegisterAckMessage`, `HeartbeatMessage`,
  `HeartbeatAckMessage`, `CommandResponseMessage`, `CloudInitLogMessage`,
  `SystemEventMessage`, `ErrorMessage`; commands: `DockerRunCommand`,
  `DockerStopCommand` equivalents, `ExecCommand`, `GetLogCommand`,
  `SetLogLevelCommand`, `StatusCommand`, `PingCommand`,
  `ConfigureDockerRegistryCommand`, `SetIdentityCommand`,
  `CaptureOutputCommand`, `GetIdentityCommand`, `GetLogLevelCommand`).
- **Persistence layer** — SQLite-backed implementations of paramodel stores
  (`SqliteArtifactStore`, `SqliteCheckpointStore`, `SqliteExecutionRepository`,
  `SqliteExecutionStateManager`, `SqliteJournalStore`, `SqliteMetadataStore`,
  `SqliteResultStore`, `SqliteResultQuery`), plus a `SchemaManager` and
  `ParamodelJacksonModule` for JSON codec. Plus the operational
  `ProvisioningTracker` family under `controller/tracking/*` (`Node`,
  `NodeStatus`, `NodeTracker`, `AgentTracker`, `ProvisioningRequestTracker`,
  `DeploymentJobTracker`, `SystemServiceTracker`, `CommandTracker`,
  `ResourceShareTracker`, `ModelingPlanTracker`,
  `ModelingExecutionTracker`, `ModelingResultSummaryTracker`,
  `WorkspaceTracker`, `GroupTracker`, `UserAuthTracker`,
  `UserSessionTracker`, `UserRoleTracker`, `JupyterInstanceTracker`,
  `JupyterPipTracker`, `JupyterVolumeTracker`, `JupyterLimitsTracker`,
  `PendingContainerTracker`, `ContainerOutputTracker`,
  `SystemConfigTracker`, `SystemEventTracker`, `SecurityJournalTracker`).
- **Paramodel bridge** — `controller/paramodel/*`
  (`DockerImageElement`, `DockerImageElementFactory`, `SysconfigElement`) and
  `controller/valuesource/*` (the `HyperplaneTypeProvider` that registers the
  `node`, `service`, `command` element types for paramodel's
  `ElementTypeDescriptorProvider`).
- **Execution bridge** — `controller/execution/*`: `HyperplaneRuntime`
  implements paramodel's `Runtime`, plus `HyperplaneElementInstance`,
  `HyperplaneDeploymentRequest`, `HyperplaneTrialExecutionRequest`,
  `HyperplaneSteppingHandle`, `ElementSteppingHandle`.
- **Lifecycle** — `lifecycle/NodeLifecycleManager`,
  `lifecycle/NodeLifecycleCoordinator`.
- **Deployment** — `deploy/DeploymentExecutor` (SSH-based agent install);
  `ssh/SshClient`.
- **Topology** — `topology/*` (`SystemTopology`, `TopologyView`,
  `TopologySnapshot`, `RoleTopology`, node variants: `AgentNode`,
  `ContainerNode`, `ServiceNode`, `ControllerNode`, `WebConsoleNode`).
- **Events** — `events/*` with categories LIFECYCLE, CONFIGURATION,
  OPERATION, TOPOLOGY, SYSTEM. `EventAggregator`, `EventCodec`, typed events
  like `AgentConnectedEvent`, `AgentDisconnectedEvent`, `ControllerStartedEvent`,
  `ControllerStoppedEvent`, `ServiceStartedEvent`, `ServiceStoppedEvent`,
  `ConfigChangedEvent`, `LogLevelChangedEvent`, `OperationEvent`,
  `CommandErrorEvent`. Event streaming semantics documented at
  `links/hyperplane/docs/ARCHITECTURE.md §4b` (immutable, per-consumer view,
  default "from now").
- **Dockerfile parameter extraction** — `controller/docker/*` (not fully
  read; `ImageParamSpaceService` is the relevant service). Rules defined in
  `links/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`.
- **Orchestrator** — not its own module in this repo snapshot; its behavior
  is described in `links/hyperplane/docs/ARCHITECTURE.md` and the components
  doc. EC2 operations and SSH deployment live inside the controller
  (`deploy/`, `ssh/`) with EC2-specific concrete types in
  `cloud-elements-ec2`.

Key references consulted:

- `links/hyperplane/README.md`
- `links/hyperplane/docs/ARCHITECTURE.md`
- `links/hyperplane/docs/architecture/components.md`
- `links/hyperplane/docs/architecture/planning_and_execution.md`
- `links/hyperplane/docs/NODE-CONTRACT.md`
- `links/hyperplane/docs/NODE-LIFECYCLE.md`
- `links/hyperplane/docs/parameters/paramodel-proposal-001.md`
- `links/hyperplane/docs/parameters/tactile_params.md`
- `links/hyperplane/docs/studies/study_system.md`
- `links/hyperplane/docs/serviceapi/common_api.md`
- `links/hyperplane/hyperplane-controller/CONTROLLER-API.md`
- `links/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`
- `links/hyperplane/containerdefs/README.md`
- `links/hyperplane/hyperplane-webconsole/requirements-webconsole.md`
- Java source as cited inline above.

## 4. Conceptual map

The two projects layer this way:

```
+--------------------------------------------------------------------------+
| Hyperplane Platform                                                      |
|   CLI (picocli)  ──▶  WebConsole (reverse proxy, UI)                     |
|                             │                                            |
|                             ▼                                            |
|   Controller Hub (authoritative):                                        |
|     - HTTP + WebSocket (agents, UI event stream, cloud-init logs)        |
|     - SQLite trackers (nodes, agents, requests, deployments, events, …)  |
|     - Paramodel bridge (element types: node / service / command)         |
|     - Plan executor: consumes paramodel ExecutionPlan                    |
|     - Deployment (SSH + systemd agent install)                           |
|     - EC2 orchestration                                                  |
|   Agent (on each node):                                                  |
|     - WS client, heartbeats                                              |
|     - Docker lifecycle on behalf of controller                           |
|     - Cloud-init log streaming                                           |
+--------------------------------------------------------------------------+
                                 │ uses
                                 ▼
+--------------------------------------------------------------------------+
| Simplica layer (paramodel.plan + paramodel.compilation + paramodel.exec) |
|   TestPlan  ──commit──▶  ExecutionPlan                                   |
|    axes, elements,         atomic steps, barriers, execution graph       |
|    relationships, policies trial ordering, fingerprint, metadata         |
|                             │                                            |
|                             ▼ Executor / Scheduler / Runtime             |
|                          TrialResult(s), checkpoints, journal            |
+--------------------------------------------------------------------------+
                                 │ builds on
                                 ▼
+--------------------------------------------------------------------------+
| Paramodel algebra (paramodel.parameters + paramodel.trials)              |
|   Parameter<T>  over  Domain<T>  constrained by Constraint<T>            |
|   Value<T>  with fingerprint                                             |
|   Trial  = an assignment of Values; TrialSet = a collection of Trials    |
|   DerivedParameter<T>  computed from other parameters                    |
|   ParameterBinder / ParameterBinding  bind user inputs to a schema       |
+--------------------------------------------------------------------------+
```

### 4.1 Elements and parameters are the centre of the model

The whole system exists to parameterise and run **element
deployments**. Everything else in the conceptual vocabulary
attaches to, describes, or acts upon an element. Read the rest of
this SRD (and the per-aspect SRDs that follow) with that as the
anchor.

**The primary pair.**

- **Element** — a deployable, configurable resource: an EC2 node,
  a Docker service, a command container, a diagnostic probe, a
  simulated fixture. The element is the unit the system *provisions,
  runs, observes, and tears down*. Every element carries:
  - a set of **parameters** (the dimensions along which it can be
    configured at deploy time),
  - **labels** (intrinsic facts about the element: its `type`, its
    `api`, its `mode`),
  - **tags** (extrinsic, organisational categorisation the user
    attaches for sorting / filtering / grouping),
  - **plugs** and **sockets** (connection points; compatibility
    with other elements is determined by the facets on each),
  - **dependencies** on other elements (edges with
    `RelationshipType`: Shared / Exclusive / Dedicated / Linear /
    Lifeline),
  - optional **health-check** and **lifecycle hooks**,
  - a **max_concurrency** / **max_group_concurrency** policy per
    SRD-0002 R26.
- **Parameter** — a named configurable axis *of an element*. A
  parameter has a name, a typed domain (the values it may take), a
  set of constraints, an optional default, and metadata
  (labels/tags). Parameters are never free-floating in a running
  system — they only have meaning as part of some element's
  declaration. Built-in parameter kinds: Integer, Double, Boolean,
  String, Selection. A **DerivedParameter** is one whose value is
  computed from other bound parameters.

**Everything else supports the element/parameter pair.**

- **Value** — one produced assignment for one parameter, carrying
  provenance (which element, which parameter, when, how, with what
  fingerprint).
- **Domain** — the set of valid values for a parameter.
- **Constraint** — a Boolean predicate over parameter values.
- **ValidationResult** — the outcome of checking a value against a
  parameter's domain and constraints.
- **Axis** — a parameter chosen for variation in a study; carries
  an explicit ordered list of values to sweep.
- **BindingNode** / **ElementBindingTree** — the tree through
  which parameter values cascade down an element dependency graph.
- **Trial** — one concrete deployment configuration: a complete
  assignment of values across every (element, parameter)
  coordinate in scope.
- **TrialSet** — a collection of trials to explore. Storage order is
  stable for reproducibility and reporting but carries no
  execution-order semantic; the executor runs trials concurrently
  subject to element-graph relationships and scheduler policy.
- **TestPlan** — the authored spec that combines a set of
  elements, their dependencies, the axes to sweep, and the
  policies that govern execution.
- **TrialOrdering** — scheduler policy for how pending trials in a
  trial set get picked up (sequential, shuffled, edge-first,
  dependency-optimised, cost-optimised, custom). Lives on the
  executor/scheduler layer, not on the trial set itself.
- **AtomicStep** — an indivisible execution unit in the compiled
  plan: `Deploy`, `Teardown`, `TrialStart`, `TrialEnd`, `Await`,
  `SaveOutput`, `Barrier`, `Checkpoint`. Each step is always
  *about* an element (which is why the `element` field is
  mandatory on most variants).
- **Barrier** — synchronisation point inserted by the compiler
  when relationship types require serial access.
- **ExecutionGraph** — DAG over AtomicSteps produced by
  compilation.
- **ExecutionPlan** — the immutable, fingerprinted compile output:
  steps, barriers, trial ordering, checkpoint strategy, resource
  requirements.
- **Executor / Scheduler / Runtime / ResourceManager /
  ArtifactCollector** — the execution-side machinery that *runs*
  elements according to the plan.
- **Checkpoint** — durable mid-run state that enables resume.
- **Attributes (labels / tags / plugs+sockets)** — the metadata
  model carried by elements (and by other attributable types that
  inherit label/tag conventions from SRD-0005).

If a concept is on this list and doesn't mention elements, that's
because elements are implicit — e.g. a `Value` is a value *of some
element's parameter*, a `Trial` is an assignment *across element
coordinates, where an element's coordinates are the parameters the
element declares*, and an `AtomicStep` is an action *against an
element*.

### 4.2 Hyperplane-specific adds

- **Element type taxonomy**: `node` (infrastructure), `service` (long-running
  container), `command` (run-to-completion container). Registered via
  `ElementTypeDescriptorProvider`.
- **Element implementations**: `Ec2NodeElement`, `DockerContainerElement`,
  `DockerImageElement` (paramodel wrapper around a Dockerfile-derived
  ParamSpace), `SysconfigElement` (node profile schema), `DiagnosticElement`.
- **Dockerfile `@param` / `@result` convention**: Dockerfiles are the source
  of parameter schemas for container elements. Labels `com.hyperplane.api`
  (required) and `com.hyperplane.mode` (service | command, required)
  classify images.
- **External `valueSource`**: `@param … valueSource=datasets` wires a
  parameter's valid values to a controller API endpoint
  (`GET /api/{valueSource}`) rather than a static set.
- **Study composition (higher-level wrapper around TestPlan)**: described in
  `links/hyperplane/docs/studies/study_system.md`. Introduces three scopes —
  `STUDY`, `TRIAL`, `INVOCATION` — derived (not assigned) from axis taint
  propagation in the dependency graph. Adds dependency-edge *persistence
  policies* (PERSIST, RESET_EACH, FAN_OUT) layered on top of paramodel's
  RelationshipType. Execution actions: PROVISION, DEPROVISION, START, STOP,
  AWAIT, SAVE_OUTPUT, BARRIER. Command containers produce output that is
  saved by the agent and can feed subsequent command containers via
  `${output_of:X}`.
- **Study state machine**: PENDING → VALIDATING → PROVISIONING → EXECUTING
  → COLLECTING → COMPLETED, with FAILED / CANCELLED branches and a
  configurable `on_failure` policy (stop | skip | retry(n)).
- **Node lifecycle**: distinct from paramodel; a rich state machine with 10
  happy-path states (PROVISIONING → PROVISIONED → CONFIGURING → CONFIGURED
  → DEPLOYING → DEPLOYED → REGISTERING → REGISTERED →
  AWAITING_HEARTBEAT ↔ ACTIVE_HEARTBEAT) and terminal/failure states
  (TERMINATED, PROVISIONING_FAILED, CONFIGURING_FAILED, DEPLOYING_FAILED,
  REGISTERING_FAILED, LOST_HEARTBEAT). Computed from (ec2State,
  cloudInitStatus, deploymentStatus, agentConnected, agentActive). See
  `links/hyperplane/docs/NODE-LIFECYCLE.md`.
- **Controller HTTP/WS API**: `/api/…` HTTP (health, logs, events, topology,
  agents, nodes, system config, provisioning, images, deploy), plus WS
  endpoints `/ws/agent`, `/ws/cloudinit/{nodeName}`, `/ws/events`. Full list
  in `links/hyperplane/hyperplane-controller/CONTROLLER-API.md`.
- **Agent protocol**: sealed `Message` hierarchy, JSON over WebSocket with
  `MessageCodec`. Commands are typed (DockerRun, Exec, GetLog, Ping,
  Status, SetLogLevel, ConfigureDockerRegistry, CaptureOutput,
  SetIdentity, GetIdentity, GetLogLevel). Heartbeats every 10s; 30s
  timeout → `LOST_HEARTBEAT`.
- **Node contract**: Ubuntu 24.04, Java 25, Docker, LVM'd `/mnt/data`,
  Vector (log shipping), node_exporter + vmagent, specific token resolution
  `${{db:…}}` and `${{tag:…}}`. See `links/hyperplane/docs/NODE-CONTRACT.md`.
- **Reverse-proxy gateway** (webconsole): `/api/*` → controller,
  `/jupyter/*` → Jupyter, `/datasette/*` → Datasette, `/metrics/*` →
  VictoriaMetrics (with prefix stripping). Single HTTPS port (default 8443).
- **Persistence**: two distinct SQLite databases in play — the operational
  `~/.hyperplane/hyperplane.db` (ProvisioningTracker schema) and the
  paramodel store layer (ArtifactStore, CheckpointStore, etc.) on
  SQLite. Whether these are merged or kept separate in the Rust port is an
  open question (Q3).
- **Observability**: events (typed, categorized, WS-streamable), metrics
  (VictoriaMetrics + Grafana), cloud-init log streaming from nodes to
  controller.
- **Auth**: User/pass login → session cookie for browser; bearer token for
  SDK/CLI; system API key for webconsole → controller proxy.

## 5. Proposed workspace layout (crate inventory)

This is the top-level layout `hyperplane-rs` will adopt. It does not pin
module contents; each listed crate will get its own SRD that defines its API
surface, error model, and dependencies.

```
hyperplane-rs/
├── Cargo.toml                  # workspace root
├── crates/
│   ├── paramodel-elements/     # central algebra: domain, parameter, constraint,
│   │                           #   value, validation, attributes, trial,
│   │                           #   element + dependency + relationship, runtime,
│   │                           #   type-descriptor, trial-context
│   ├── paramodel-trials/       # trial-set, sampling-strategy,
│   │                           #   trial-result, trial-status
│   ├── paramodel-plan/         # axis, test-plan, execution-plan, atomic-step,
│   │                           #   execution-graph, barrier, trial-ordering,
│   │                           #   policies, metadata, fingerprinting
│   ├── paramodel-compiler/     # 8-stage pipeline, compilation context/stage,
│   │                           #   optimization passes
│   ├── paramodel-executor/     # executor, runtime, scheduler,
│   │                           #   resource-manager, artifact-collector,
│   │                           #   journal, checkpointing
│   ├── paramodel-persistence/  # storage traits: artifact, checkpoint,
│   │                           #   execution-repo, journal, metadata, result
│   ├── paramodel-tck/          # conformance test suite, hidden behind a
│   │                           #   cargo feature so it doesn't bloat normal
│   │                           #   dependents
│   ├── paramodel-mock/         # trivial in-memory impls for tests/examples
│   │
│   ├── hyperplane-core/        # element types (node/service/command),
│   │                           #   relationship extensions (persistence
│   │                           #   policy), study model, ID types, labels
│   ├── hyperplane-dockerdefs/  # Dockerfile @param / @result parsing,
│   │                           #   label conventions
│   ├── hyperplane-persistence/ # SQLite implementations of paramodel stores +
│   │                           #   operational tracker schema
│   ├── hyperplane-protocol/    # agent ↔ controller message types + codec
│   ├── hyperplane-controller/  # HTTP + WS server, lifecycle, event
│   │                           #   aggregator, SSH deploy, EC2 orchestrator,
│   │                           #   plan executor bridge
│   ├── hyperplane-agent/       # agent binary: WS client, Docker ops,
│   │                           #   heartbeats, log shipping
│   ├── hyperplane-cli/         # `hyperplane` binary (clap-based)
│   ├── hyperplane-webconsole/  # reverse-proxy + static UI host
│   └── hyperplane-tck/         # hyperplane-level conformance suite
├── docs/
│   └── design/                 # SRDs live here
└── links/                      # reference projects, never modified
```

Notes on this layout:

- **Fewer, fatter crates are tempting**, but the paramodel contracts benefit
  from hard separation because TCK-style testing and alternative
  implementations are a design goal (cf. mock vs engine in upstream). Keep
  the algebraic crates small.
- **No `paramodel-api` / `paramodel-engine` split.** In Rust, the "contracts"
  are traits and the "engine" is a default impl crate. Splitting per domain
  (parameters vs plan vs executor) is more idiomatic than the Java split.
- **`hyperplane-tck` is separate** so platform impls can opt into conformance
  tests without pulling the full engine.
- Binaries can be `[[bin]]` targets inside their respective library crates;
  final choice deferred to the crate's own SRD. The user-facing CLI binary
  is named `hyper` (R23).

## 6. Resolved design positions

Each item below records a position reached through review. Items that are
still genuinely open move into their owning SRDs.

### 6.1 From upstream design

- **R1 — RelationshipType variants.** Adopt the full five-variant enum:
  `Shared`, `Exclusive`, `Dedicated`, `Linear`, `Lifeline`. Source of
  truth for the upstream enum is
  `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/elements/RelationshipType.java`.

- **R2 — Execution-action vocabulary.** Unify paramodel's `AtomicStep`
  hierarchy and Simplica's study actions behind a single Rust enum.
  Starting set, to be refined in the atomic-steps SRD: `Deploy`,
  `Teardown`, `TrialStart`, `TrialEnd`, `Await`, `SaveOutput`, `Barrier`,
  `Checkpoint`. Provisioning vs `docker run` vs exec is determined by
  the target element's type descriptor — `Deploy` against a node-type
  element means "provision EC2 + install agent"; `Deploy` against a
  service-type element means "pull image and `docker run`".

- **R3 — Derived parameters.** Present in the upstream source at
  `links/paramodel/paramodel-api/src/main/java/io/nosqlbench/paramodel/parameters/DerivedParameter.java`
  and consumed by `DefaultParameterBinder`. Part of the Rust design;
  covered by the parameters-and-domains SRD.

- **R4 — Test plan immutability and workflow.** A plain builder plus an
  immutable, fingerprinted record. Concretely:
  - `TestPlanBuilder` is the mutable authoring surface.
  - `TestPlan` is an immutable struct with a content-hash fingerprint.
    No `&mut self` methods — the type system enforces "cannot be changed
    after construction."
  - `TestPlanBuilder::build(self) -> TestPlan` is the one-way transition.
    `TestPlan::edit(&self) -> TestPlanBuilder` clones back into a builder
    for iteration and yields a *new* `TestPlan` with a *new* fingerprint
    on `build`. No external dependent is silently disturbed by edits.
  - Dependents (executions, results, checkpoints) reference a plan by
    its fingerprint.

- **R5 — Barrier split.** `Barrier` is the immutable record in the
  execution plan (id, type, dependencies, timeout spec). A runtime
  companion (working name `BarrierHandle`) owns the state machine and
  the `await`/`release`/`fail` operations. Names pinned in the
  atomic-steps SRD.

- **R6 — Contracts may carry concrete algebraic types.** The `paramodel-*`
  contract crates are not "pure traits only." They may also export
  concrete enums and structs (e.g. `ValidationResult`, `Label`, `Tag`,
  `Trait`, `AttributeSupport`) and self-contained accessory APIs (such
  as the tagging API), so long as those types are additive to the
  contract surface and do not encode implementation choices the
  engine should own.

- **R7 — Persistence boundaries.** The paramodel crates define what
  needs to be stored and expose it as traits (artifact, checkpoint,
  execution repo, journal, metadata, result). They do not pick or
  mention a storage engine. Hyperplane provides the concrete
  implementation on a single SQLite engine; see R11.

### 6.2 Rust idiom

- **R8 — Error model.** Library crates return `Result<T, E>` with
  `thiserror`-style error enums. No panics on recoverable failures.
  `anyhow` is allowed at binary / CLI edges only.

- **R9 — Ownership.** Owned immutable values by default; builders for
  construction; typestate used where it adds real safety. Each
  per-aspect SRD records its specific ownership model.

- **R10 — Time types.** `std::time::Duration` for elapsed time;
  `jiff::Timestamp` (preferred) or `chrono::DateTime<Utc>` for
  wall-clock. Pinned in the common-types SRD.

- **R11 — Single SQLite state store.** One SQLite engine and one
  database file back the paramodel persistence traits, the Hyperplane
  ProvisioningTracker analogue, and any other durable subsystem state.
  Each subsystem defines its persisted types in its own domain terms;
  they share the file and the connection pool, not the schema.

- **R12 — Heterogeneous parameter collections.** Use Rust enums over
  Int/Double/Bool/String/Selection variants for `Parameter`, `Value`,
  `Domain`, etc. No `dyn Trait` for the parameter algebra. Generic type
  parameters are retained inside variants where a single-type algebra
  helps; collection shapes hold the enum.

- **R13 — Sealed hierarchies as enums.** Default to `enum` with struct
  variants for every upstream sealed interface (`ValidationResult`,
  `AtomicStep`, `Domain`, barrier types, etc.).

- **R14 — TCK.** Conformance suite exposed as a crate (`paramodel-tck`)
  with a macro-generated test harness parameterised over the
  implementation under test. `proptest` for algebraic laws. Details
  deferred to the TCK SRD.

### 6.3 Scope & delivery

- **R15 — Full-parity target.** The port aims for full functional
  parity with upstream, including Docker image management, agent
  deployment, EC2 orchestration, event streaming, and auxiliary
  services. We stage the work; nothing is dropped as permanently out
  of scope at this time.

- **R16 — Target backends are generic.** Element types are generic over
  the managed resource. Anything runnable as a Docker image or
  managed as an instance (EC2 or otherwise) is a valid target,
  including the existing JVM vector backends and their harnesses.

- **R17 — Async everywhere.** `tokio`-based async end to end. HTTP
  server, WebSocket, SSH, SQLite (via `sqlx` or equivalent), agent I/O.
  No sync-only core carve-out.

- **R18 — Agent transport.** WebSocket + JSON. Simple to diagnose,
  introspectable, boundary-proxy-friendly.

- **R19 — Web UI stack.** `axum` server + HTMX on the client side.
  No SPA build pipeline.

- **R20 — SDK / Jupyter.** In scope, scheduled for a later phase.

- **R21 — Auth and multi-user.** In scope from early phases. Includes
  proxying user-authenticated connections across the webconsole →
  controller boundary (and outward to auxiliary services where
  applicable).

- **R22 — Container harnesses.** Upstream Dockerfiles may be reused
  as-is where convenient; we are free to refresh them when they are
  in the way.

- **R23 — Binary name.** The shipped CLI binary is named `hyper` for
  now. The distribution slug remains `hyperplane-rs`. A shorter final
  name may be chosen later.

- **R24 — License header.** All source files carry
  `Copyright (c) Jonathan Shook` and `SPDX-License-Identifier: Apache-2.0`.
  Additional contributors add their own lines as they contribute.

- **R25 — Toolchain.** Rust edition 2024 on the nightly toolchain.

- **R26 — Concurrency is a per-element declarative policy, not an
  authored graph property.** Upstream's reducto planner
  (`links/paramodel/paramodel-engine/src/main/java/io/nosqlbench/paramodel/engine/planners/reducto/reducto.md`)
  is explicit: "encoding concurrency limits as structural dependency
  edges … becomes explosively complex … By expressing concurrency
  limits as declarative directives rather than structural edges, the
  planner keeps graph complexity manageable and the executor retains
  the flexibility to enforce limits dynamically without artificial
  serialization." We follow this exactly.

  Concurrency is expressed on the element prototype as two scalars:

  - `max_concurrency` — global limit on the number of active instances
    of this element at any time (unset = unbounded).
  - `max_group_concurrency` — per-group limit within a coalesced group
    (unset = inherit from `max_concurrency`).

  The compiler stamps these as metadata on each `Deploy` step of the
  execution graph; the executor enforces them dynamically. Trials that
  share a group can run concurrently up to those limits — no
  `SweepMode::Concurrent` flag on axes, no concurrency hint on
  dependency edges, no concurrency-encoded `RelationshipType`
  variants.

  Authored concurrency for a "fan-out" pattern (one server, many
  parallel clients) becomes: the server is `Shared`, the client
  element declares a `max_concurrency` > 1, and the compiler emits
  one `Deploy(client)` step per trial in the group with the limit
  annotated.

- **R27 — Reducto defines the compilation pipeline, verbatim-retained
  and then scrutinised.** Reducto defines an 8-rule reduction pipeline
  (lifecycle expansion → dependency edges → group coalescing → trial
  notifications → health-check gates → concurrency annotations →
  start/end materialisation → transitive reduction), plus mixed-radix
  trial enumeration with stable trial codes, the cross-product table
  of relationship-type chains (25 combinations), and a warnings
  catalogue. It also describes the relationships *between* the
  relationship types — how SHARED, EXCLUSIVE, DEDICATED, LINEAR, and
  LIFELINE project into scheduling decisions and how they compose in
  transitive chains.

  The compilation SRD (Phase 1 item 8) **retains this rule set
  as-is** as its normative content, translated into Rust terms but
  preserving every rule, every chain-composition row, and every
  warning. That SRD is the one place these rules live in our tree.

  Reducto's rules may not be 100% internally consistent and may carry
  bugs. The compilation SRD includes a scrutiny pass: it walks the
  rule set for corner cases, contradictions between rules, and
  self-inconsistencies, and resolves each one explicitly before the
  SRD is accepted. Bugs we identify are fixed in our port with a note
  saying why we diverged; we do not mirror known bugs.

### 6.4 Three-graph model (elaboration of R1, R2, R26, R27)

Three successive graphs are distinguished — two derived from the one
authored by the user. The naming is fixed here so the later SRDs
can use it without redefinition.

**Element Graph — authored.** Nodes are `Element` *prototypes* (the
structs from SRD-0007: name, parameters, labels, tags, plugs,
sockets, configuration, exports, concurrency caps). Edges are the
`Dependency` records authored on each element, each carrying a
`RelationshipType` from R1. This is the only graph the user writes:
"A depends on B with relationship Shared; B depends on C with
relationship Dedicated; each element may run up to N instances
concurrently." Axes attach to this graph at the test-plan layer to
vary specific element parameters.

**Element Instance Graph — derived, reducto phase 1.** Nodes are
concrete *element instances* — one per unique bound parameter set
as determined by axis taint propagation. Edges are instance-to-
instance dependency edges that reflect the authored relationship
types combined with binding-level alignment: group coalescing
folds trials whose upstream parameters are unchanged into a single
instance; `Dedicated` propagation couples a target's instance
cardinality to its owner's; `Lifeline` clusters collapse their
deactivations. This layer answers "how many instances of each
element exist, how are they bound, and which instances connect to
which?" Reducto produces it via mixed-radix trial enumeration
(Stage One) plus binding-state computation and Rule 3 (group
coalescing).

**Execution Graph — derived, reducto phase 2.** Nodes are the
unified `AtomicStep` variants from R2 (`Deploy`, `Teardown`,
`TrialStart`, `TrialEnd`, `Await`, `SaveOutput`, `Barrier`,
`Checkpoint`). Edges are step-to-step ordering constraints. Reducto
derives it from the Element Instance Graph through its remaining
rules: lifecycle expansion, dependency edge materialisation, trial
notifications, health-check readiness gates, concurrency
annotations, start/end sentinel materialisation, and transitive
reduction.

Only the first graph is authored. The compilation SRD (Phase 1
item 8) specifies the derivation rules verbatim from reducto, plus
a scrutiny pass for corner cases (R27).

Upstream "persistence-policy" names (PERSIST / RESET_EACH / FAN_OUT)
do not appear as authored labels on the element graph. They are
patterns on the execution graph that the compiler chooses:

- PERSIST ≈ group coalescing across trials where upstream parameters
  are unchanged.
- RESET_EACH ≈ group coalescing is defeated, either because the
  element is a trial element (never coalesced) or because parameters
  do change between trials.
- FAN_OUT ≈ multiple instances of a downstream element activate
  concurrently against one upstream, bounded by the downstream's
  `max_concurrency`.

Each chosen pattern appears in the plan as a human-readable `reason`
on the relevant `Deploy` / `Teardown` / `Barrier` step, matching
upstream's existing `reason` field.

### 6.5 Scope model — decision

The numeric group-level mechanism from paramodel and the named scope
set from Simplica describe the same thing at different levels of
abstraction:

- Paramodel's **group level** is a non-negative integer per element,
  derived from axis taint. Group level 0 = one instance for the whole
  study; deeper levels = finer instance bucketing.
- Simplica's **scope** (`STUDY` / `TRIAL` / `INVOCATION`) is a named
  three-bucket presentation of the same underlying group-level
  mechanism, with command containers always at the deepest bucket.

**Decision:** we keep the group-level integer as the authoritative
internal value — the compiler derives and reasons over it — and we
surface the named buckets `STUDY` / `TRIAL` / `INVOCATION` on element
records for human legibility. Section reuse (the same element reused
across adjacent trials with unchanged bound-parameter set) stays an
internal scheduler optimisation, not a fourth scope name. The
parameters-and-plan SRDs fix the derivation rules.

## 7. Decisions

Beyond the resolved positions in §6:

- **D1.** The project is organised as a Cargo workspace. Reference-only
  code stays under `links/` and is never modified.
- **D2.** The crate inventory in §5 is the starting layout. Each listed
  crate gets its own SRD before its code is written.
- **D3.** `paramodel-*` crates realise the paramodel algebra and the
  Simplica layer. `hyperplane-*` crates realise the Hyperplane control
  plane. `hyperplane-*` depends on `paramodel-*`; `paramodel-*` does
  not depend on `hyperplane-*`.
- **D4.** Reference docs under `links/**/docs/` are informative, not
  normative. When SRD text disagrees with a reference doc, the SRD
  wins. When SRD text is silent, reference docs and upstream code may
  be consulted to inform future SRDs, but are not binding.
- **D5.** No open questions remain at the assay level as of this
  writing. Per-aspect SRDs surface their own open questions in their
  own sections.

## 8. Proposed follow-up SRDs

SRDs are written (and their crates implemented) in phases. Numbers are not
reserved; the next SRD written takes the next free number.

### Phase 1 — Paramodel tier (current focus)

These come first. The paramodel crates are the foundation everything
else builds on; we write and implement them before the Hyperplane-
specific work starts.

The **load-bearing SRD in this phase is #5, Elements**. Everything
before it defines shapes that an element carries (parameters, labels,
tags, plugs/sockets) or shapes the system uses to talk about running
an element (trials, trial sets). Everything after it composes elements
together (axes, test plans, compilation, execution). Read items 1–4
as "what an element is made of and what it produces," and item 5 as
"the element itself, stitched together."

1. **Common types & conventions** — edition, toolchain, error model,
   time types, ID types, fingerprint/hash policy, serde policy,
   license header. (Pins R8, R9, R10, R24, R25.)
2. **Parameters & domains** — the configurable axes of an element:
   `Parameter`, `Domain`, `Constraint`, `Value`, `ValidationResult`,
   built-in parameter types, `DerivedParameter`. (Pins R3, R12, R13.)
3. **Labels, plugs/sockets, and tags** — an element's metadata and
   connection surface: labels (intrinsic facts), plugs/sockets with
   facet-based compatibility (replaces upstream's shapeless "Traits"),
   tags (extrinsic organisation), and the namespace-uniqueness rule
   across tiers. (Pins R6; drafted as SRD-0005.)
4. **Trials & trial sets** — the shapes that describe running elements
   with specific parameterisations: `Trial` (a full
   (element, parameter)→value assignment), `TrialSet` (a collection of
   trials + the sampling strategy that produced them — no
   execution-order semantic), `TrialResult`, `TrialStatus`,
   `SamplingStrategy`.
5. **Elements & relationships** — the central SRD. Pulls items 2–4
   together into the `Element` type: its parameter list, labels, tags,
   plugs, sockets, dependencies with `RelationshipType`, health-check
   spec, trial-lifecycle participation, binding tree, concurrency
   fields. This is the anchor type the rest of the system runs on.
   (Pins R1.)
6. **Test plans & axes** — composes elements into a study: `TestPlan`
   / `TestPlanBuilder`, `Axis` (a parameter elevated to a study
   dimension, owned by the element's parameter it varies), policies,
   optimisation strategy. (Pins R4, R26.)
7. **Atomic steps & execution graph** — unified `AtomicStep` enum,
   `ExecutionGraph`, `Barrier` record, `TrialOrdering`. (Pins R2, R5.)
8. **Compilation pipeline (reducto port)** — the 8-rule reduction
   pipeline retained verbatim (in Rust terms), mixed-radix trial
   enumeration and trial codes, the full 25-row relationship-chain
   composition table, the warnings catalogue, plus a scrutiny section
   that walks the rule set for corner cases, inconsistencies, and
   contradictions and resolves each one. (Pins R27.)
9. **Executor, scheduler, runtime** — `Executor` surface, scheduling
   policies, `ResourceManager`, observer hooks, checkpoint strategy,
   resume-from. (Pins R17.)
10. **Persistence traits** — storage trait surfaces (artifact, checkpoint,
    execution repo, journal, metadata, result) exposed by paramodel
    without fixing a backend. (Pins R7.)
11. **TCK** — conformance test strategy, shared traits, property tests.
    (Pins R14.)

### Phase 2 — Hyperplane tier

Only started once Phase 1 is landed and tested end-to-end at the mock
level.

12. Hyperplane element types (node / service / command descriptors;
    type provider; concrete EC2 and Docker elements; diagnostic elements).
13. Dockerfile param extraction (`@param`, `@result`, labels
    `com.hyperplane.api` and `.mode`, `valueSource` binding). (Pins R22.)
14. SQLite state store (schema management, connection pool, backing all
    paramodel persistence traits plus the operational tracker data).
    (Pins R11.)
15. Study composition — the Simplica-on-top-of-paramodel layer; study
    YAML; composer pure function; study state machine.
16. Node lifecycle — the 10 + 6 states, transitions from EC2 / cloud-init
    / deployment / agent inputs, auto-deploy, resume behaviour.
17. Agent protocol — message hierarchy, codec, commands, heartbeats,
    reconnection, identity. (Pins R18.)
18. Controller HTTP + WS API — routes, event-streaming semantics.
19. Deployment (SSH + systemd) — agent install protocol, logs, failure
    handling.
20. EC2 orchestrator — provisioning, state sync, termination, tags,
    profiles, token resolution.
21. WebConsole — reverse proxy, auth handling, HTMX UI. (Pins R19.)
22. CLI — command tree, parity with controller API, completion
    generation. (Pins R23.)
23. Events & observability — event categories, aggregator, event
    streaming, metrics, log streaming.
24. Auth & multi-user — user/pass + session + bearer + system API key;
    boundary proxying. (Pins R21.)
25. SDK & Jupyter integration — later phase. (Pins R20.)

Anything not listed above is out of scope until an SRD brings it in.

## 9. Risks & meta-observations

- **Risk 1 — SRD granularity.** Upstream hyperplane has ~180 controller
  files and a dense tracker/event subsystem. Treating every tracker as
  its own SRD would be paralysing. Group by *subsystem*, not by file.
- **Risk 2 — Auxiliary-service heterogeneity.** Jupyter, Datasette,
  Grafana, VictoriaMetrics, and similar integrations each have their
  own flavour and integration points. They warrant per-service SRDs
  (typically short ones), not a single catch-all.
- **Observation — Simplica is a component boundary inside paramodel.**
  Simplica is not the Hyperplane layer; it is the *execution planning
  subsystem* within paramodel — the plan/compiler/executor portion that
  turns algebraic parameters and axes into concrete, scheduled step
  graphs. The Rust crate split reflects this:
  `paramodel-elements`/`-trials` carry the algebra, and
  `paramodel-plan`/`-compiler`/`-executor`/`-persistence` are the
  Simplica component within the same design space. The
  `paramodel-*` vs `hyperplane-*` split, separately, is the
  deployment-boundary split: anything that does not know about nodes,
  containers, EC2, SSH, or Docker stays in `paramodel-*`; anything
  that does goes in `hyperplane-*`.