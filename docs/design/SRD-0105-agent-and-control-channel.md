<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0105 — Rust Agent & WebSocket Control Channel

## Purpose

Specify the Rust agent binary and the single long-lived WebSocket
that connects it to the controller. Per SRD-0100 D5 (`INV-AGENT-PEER`,
`INV-CTL-AGENT-CHANNEL`) agents talk only to the controller, and the
controller reaches agents only through two designated channels,
used in sequence per agent lifetime:

1. **SSH deploy (one-shot).** Controller opens a single SSH session
   to the node, `scp`s the agent binary, hands over an `auth-token`
   through the already-encrypted channel, and starts the agent —
   wiring it into systemd on systemd distributions so it survives
   reboots and is supervised locally, or starting it as a
   foreground process where systemd is absent. SSH is used only
   during this bootstrap; once the agent is running, the SSH
   session closes and is never reopened.

2. **WebSocket (long-lived).** Agent uses the `auth-token` to
   register with the controller over a single persistent WebSocket.
   Every subsequent message in either direction — commands from
   controller to agent, heartbeats / events / logs / metrics from
   agent to controller — rides that one WebSocket. Wire format is
   JSON.

This SRD pins down the binary, the deploy flow, the token lifecycle,
the wire format, the message catalogue, the lifecycle state machine,
and the recovery rules.

## Scope

**In scope.**

- Agent binary layout — single statically-linked executable, install
  path, systemd unit or standalone foreground.
- SSH deploy flow — how the controller places the binary on a node
  via SSH + SCP, hands over the `auth-token`, verifies integrity,
  starts it. Once-per-agent-lifetime.
- `auth-token` lifecycle — minted by controller at deploy time,
  delivered over the encrypted SSH channel, used for WebSocket
  registration, rotated on controller-initiated challenge-response.
- WebSocket wire format — single long-lived connection per agent,
  JSON envelope, correlation ids, version negotiation.
- Message catalogue for both directions.
- Agent lifecycle state machine — starting → registering → ready →
  draining → dead.
- Liveness: heartbeat cadence, timeout, lost-heartbeat recovery.
- Reconnection + re-association rules.
- Docker daemon event integration — `/events` push stream via
  `bollard` with a polling fallback.
- Graceful shutdown semantics (node-shutdown via the agent, not
  agent-process teardown).
- New invariants specific to the agent channel.

**Out of scope.**

- What commands the agent executes (SRD-0106 for services,
  SRD-0107 for commands).
- EC2 provisioning (SRD-0104).
- The controller-side endpoint that terminates the agent WebSocket
  — SRD-0108 owns the endpoint catalogue; this SRD owns the
  protocol riding on it.
- Multi-user role/permission checking at the controller — SRD-0114.

## Depends on

- SRD-0100 (controller-agent invariants, auth-token rule, lifecycle
  independence).
- SRD-0104 (EC2 node element — agent install is part of the node
  becoming ready).
- SRD-0108 (controller API — owns the WebSocket endpoint this
  protocol rides on).

---

## Channel lifecycle at a glance

```
  Phase 1: SSH deploy (ONE-SHOT)                Phase 2: WebSocket (LONG-LIVED)
  ──────────────────────────────                ────────────────────────────────

  Controller          Node                      Controller          Agent
      │                 │                           │                 │
      │─ open SSH ──────▶                           │                 │
      │                 │                           │                 │
      │─ scp binary ───▶│                           │                 │
      │─ write config ─▶│                           │                 │
      │─ systemd start ▶│                           │                 │
      │                 │                           │                 │
      │── close SSH ────▶                           │                 │
      │                 │                           │                 │
      │             (agent starts)                  │                 │
      │             (reads config)                  │                 │
      │             (opens WSS) ────────────────────▶                 │
      │                                             │◀── Register ────│
      │                                             │─── Register ack─▶
      │                                             │                 │
      │                                             │◀── Heartbeat ───│  every 10s
      │                                             │                 │
      │                                             │── CommandReq ──▶│  (declarative)
      │                                             │◀── CommandResp──│
      │                                             │◀── EventPush ───│  (docker events)
      │                                             │◀── LogChunk ────│
      │                                             │                 │
      │                                             │── TokenRefresh ▶│  periodic
      │                                             │◀── ack ─────────│
      │                                             │                 │
      │                                             │── Shutdown ────▶│  (node teardown)
      │                                                       (agent shuts down node)
```

SSH is used exactly once (`INV-AGENT-SSH-ONCE`). Everything
after deploy rides the single WebSocket (`INV-CTL-AGENT-CHANNEL`).
Commands are declarative + idempotent (`INV-COMMAND-IDEMPOTENT`).

## Message catalogue at a glance

```
  Agent → Controller                      Controller → Agent
  ─────────────────────                   ───────────────────
  Register                                CommandRequest
  Heartbeat                               TokenRefresh
  EventPush                               Shutdown
  CommandResponse                         Drain
  LogChunk                                Ping
  MetricSample
  TokenRefreshAck
```

All messages share one envelope: `{ v, id, kind, correlates?,
ts, body }`. See D4.

## D1 — Agent binary layout

The agent is a single statically-linked Rust executable,
`hyperplane-agent`. Target triples match the supported node OSes
(initially `x86_64-unknown-linux-musl` and
`aarch64-unknown-linux-musl` — musl so the binary runs without
pinning a glibc version).

- **Install path.** `/usr/local/bin/hyperplane-agent` on the node.
- **Runtime user.** A dedicated `hyperplane` system user. Narrow
  sudo rule permits only `shutdown -h now` / `systemctl poweroff`
  (see D11). Docker socket access is granted by group membership
  (`docker` group), not sudo.
- **Process supervision.** A systemd unit, `hyperplane-agent.service`,
  installed alongside the binary. The service is `Restart=always`
  with a backoff; the agent's own reconnect loop (D9) handles
  transient WebSocket failures internally, so restarts only happen
  when the agent exits non-zero.
- **Standalone mode.** The same binary runs in the foreground
  (`hyperplane-agent --foreground`) for debugging / bare-metal
  use-cases where systemd is not present. No functional difference;
  only supervision differs.
- **Configuration.** Single config file at
  `/etc/hyperplane/agent.toml`, created during SSH deploy. Contains:
  - `controller_url` (wss://...)
  - `auth_token` (mode 600, agent user only)
  - `node_id` (assigned by controller at deploy)
  - Docker socket path (defaults to `/var/run/docker.sock`)
  - Optional log-level override.

No other on-disk state. The agent is stateless across restarts:
everything it knows about what should be running comes from the
controller on (re)registration, and everything it knows about what
*is* running it reads from the Docker daemon.

## D2 — SSH deploy flow (one-shot)

The controller bootstraps the agent onto a node exactly once per
node lifetime. The SSH channel is closed when bootstrap completes
and is never reopened.

Flow:

1. **Connect.** Controller opens an SSH session to the node using
   a provisioning key (SRD-0104 owns key management).
2. **Mint token.** Controller mints a fresh `auth-token` (opaque
   random bytes, not a JWT), records it in the `agents` table
   bound to a new `agent_id` for this node, and marks the binding
   `pending-registration`.
3. **Upload.** Controller `scp`s the agent binary to a staging
   path (`/tmp/hyperplane-agent.<nonce>`), verifies a SHA-256
   digest over the file contents matches the controller-held
   digest, then `mv`s it to `/usr/local/bin/hyperplane-agent`
   with mode 0755.
4. **Write config.** Controller writes `/etc/hyperplane/agent.toml`
   with `controller_url`, `auth_token`, `node_id` via the same
   SSH session. File mode 0600, owned by the `hyperplane` user.
5. **Install unit.** Controller drops the systemd unit file, runs
   `systemctl daemon-reload && systemctl enable --now
   hyperplane-agent`.
6. **Close SSH.** Controller closes the SSH session. The SSH key
   used for deploy is not re-used for anything else.
7. **Wait for registration.** Controller watches for the WebSocket
   registration event (D5) bearing the matching `auth-token`. On
   receipt, the `agents` row flips from `pending-registration` to
   `registered`.

If the WebSocket registration does not arrive within a deploy
timeout (default 120 s, configurable per SRD-0112), the node
transitions to `REGISTERING_FAILED` (per SRD-0104 state machine)
and operator intervention is required. The `auth-token` stays
valid; a retry is "restart the agent," not "redeploy."

**INV-AGENT-SSH-ONCE.** SSH to a running agent's node is never used
for control traffic. Post-deploy SSH access is an operational
concern (debugging, incident response), not part of the control
channel. Any code path in the controller that opens an SSH session
to a registered agent's node for control purposes is a violation.

## D3 — `auth-token` lifecycle

A single credential covers both WebSocket registration and every
reconnect. There is no separate session key, no refresh-token
pair.

**Mint.** Controller generates 32 bytes from a CSPRNG, encodes
base64url, stores a hash (argon2id) in the `agents` table, and
delivers the plaintext over the SSH channel (D2 step 2). The
controller never persists the plaintext.

**Register.** Agent presents the plaintext token in the WebSocket
handshake (see D4 — header on the HTTP upgrade request). Controller
verifies the hash match, binds the WebSocket to the `agent_id`.

**Reconnect.** Same token. The agent identifies itself by presenting
the token; the controller re-associates it with the existing row.
No separate re-auth dance.

**Rotate (controller-initiated).** Periodically (default 24 h) or
on operator command, the controller sends a `TokenRefresh` message
(D6) containing a challenge. The agent hashes `challenge || current
token` and returns the hash plus a request for the new token. The
controller verifies, generates a new random token, sends it on the
WebSocket (which is already authenticated), and updates the hash
in the `agents` table. The agent overwrites
`/etc/hyperplane/agent.toml` atomically. Both sides switch to the
new token on the next handshake; the current WebSocket is
unaffected.

**Revoke.** Admin marks the `agents` row revoked. The next
WebSocket message from that agent is rejected and the connection
closed. The agent then exits non-zero (supervisor does not restart
a revoked agent — systemd sees the `RevokedToken` close code and
the unit is configured to treat that as terminal).

**INV-AUTH-TOKEN-PLAINTEXT-EPHEMERAL.** The plaintext `auth-token`
exists only in transit (SSH deploy, WebSocket handshake, rotation
message) and in the agent's `agent.toml`. The controller stores
only the hash.

```
  auth-token lifecycle:

    (mint) ──▶ ┌──────────────────┐
               │  pending-register│  in agents table
               └────────┬─────────┘
                        │ agent presents token on
                        │ WebSocket upgrade
                        ▼
               ┌──────────────────┐
               │   registered     │◀──────┐
               └────────┬─────────┘       │ (agent reconnects
                        │                 │  with same token)
                        │ periodic (24h)  │
                        ▼                 │
               ┌──────────────────┐       │
               │   rotating       │       │
               │ (challenge/resp) │───────┘
               └────────┬─────────┘
                        │ admin revokes
                        ▼
               ┌──────────────────┐
               │    revoked       │  close code RevokedAuthToken
               └──────────────────┘  supervisor does not restart
```

## D4 — WebSocket wire format

A single persistent WebSocket per agent. JSON frames.

**Endpoint.** `wss://controller/agent/ws` (exact path owned by
SRD-0108). Authentication is an `Authorization: Bearer <auth-token>`
header on the HTTP upgrade request; non-matching tokens are
rejected with `401` pre-upgrade. TLS is mandatory (`wss://`); the
plain `ws://` scheme is rejected.

**Envelope.** Every message in both directions shares one shape:

```json
{
  "v": 1,
  "id": "01HZXABC...",
  "kind": "Heartbeat",
  "correlates": "01HZXABD...",
  "ts": "2026-04-24T14:05:33.123Z",
  "body": { ... }
}
```

```
  Envelope anatomy:

  ┌─ v ────────────── protocol version (int) ──────────────────┐
  │  ┌─ id ────────── ULID, sender-unique ─────────────────────┐│
  │  │  ┌─ kind ───── message type (D5 or D6) ─────────────────┤│
  │  │  │  ┌─ correlates ─ optional; echoes a prior id ────────┤│
  │  │  │  │  (set on CommandResponse, TokenRefreshAck, etc.)  ││
  │  │  │  │  ┌─ ts ────── RFC-3339 UTC at send ───────────────┤│
  │  │  │  │  │  ┌─ body ── kind-specific payload ─────────────┤│
  │  │  │  │  │  │                                              ││
  └──┴──┴──┴──┴──┴──────────────────────────────────────────────┘│
                                                                 │
                    one JSON object per WebSocket text frame     │
                    no multiplexing, no application-layer frag   │
                                                                 │
  ───────────────────────────────────────────────────────────────
```

- `v` — protocol version. Integer. Current: 1. Controller and
  agent both pin the accepted version range; a mismatch is a
  hard error (close code `UnsupportedVersion`).
- `id` — ULID, unique per message per sender.
- `kind` — message type; names from D5 and D6.
- `correlates` — optional; the `id` of a prior message this one
  responds to. Used on every `CommandResponse` and every
  `TokenRefresh` reply.
- `ts` — RFC-3339 UTC timestamp at send.
- `body` — message-type-specific payload. Schema per kind.

**Framing.** One JSON object per WebSocket text frame. No
multiplexing, no fragmentation at the application layer. If a
message exceeds the WebSocket max-frame size (default 1 MiB),
split the underlying payload (e.g. a large log chunk) into
multiple application messages rather than fragmenting a single
envelope.

**Version negotiation.** The agent advertises `v` on its first
message (Register). The controller accepts or rejects. No inline
downgrade: protocol evolution across breaking changes uses a new
agent deploy.

## D5 — Messages: agent → controller

| Kind | Purpose | Correlates? |
|---|---|---|
| `Register` | First message after WebSocket open; carries node metadata and protocol version | no |
| `Heartbeat` | Periodic liveness signal (every 10 s) | no |
| `EventPush` | Container / runtime / metric event observed locally | no |
| `CommandResponse` | Ack or completion of a prior `CommandRequest` | yes |
| `LogChunk` | Tailed container log output (one chunk per frame) | no |
| `MetricSample` | Point-in-time metric reading | no |
| `TokenRefreshAck` | Response to a controller-initiated rotation | yes |

**Register** body:

```json
{
  "agent_version": "0.1.0",
  "node_id": "node-abc",
  "os": "linux",
  "arch": "x86_64",
  "docker_version": "24.0.7",
  "docker_available": true,
  "startup_containers": [ { "id": "...", "state": "running", ... } ]
}
```

`startup_containers` is the agent's observation of what's already
running on the Docker daemon at agent start — supports the
reconcile pattern from SRD-0100 D13 (`INV-LIFECYCLE-INDEPENDENT`).
The controller uses this to detect crashes, restarts, or
container-runtime churn while the agent was down.

**Heartbeat** body is minimal: `{ "uptime_s": 12345 }`. The
controller uses heartbeat arrival itself, not the body, for
liveness.

**EventPush** body carries a Docker-daemon event (see D10) or a
hyperplane-level observation (e.g. `DockerUnavailable`):

```json
{
  "source": "docker" | "agent",
  "event_type": "container.die",
  "actor_id": "...",
  "time_nano": 1737728400123456789,
  "attrs": { ... }
}
```

**CommandResponse** body:

```json
{
  "status": "ok" | "error",
  "result": { ... },
  "error": { "code": "...", "message": "..." }
}
```

`status=ok` with an empty `result` is a valid ack-only response
(appropriate for commands whose outcome is fully observable via
`EventPush`).

## D6 — Messages: controller → agent

| Kind | Purpose | Correlates? |
|---|---|---|
| `CommandRequest` | Goal-state directive (see D12) | no |
| `TokenRefresh` | Challenge + request for token rotation | no |
| `Shutdown` | Node-shutdown directive (see D11) | no |
| `Drain` | Stop accepting new CommandRequests, finish in-flight | no |
| `Ping` | Out-of-band liveness probe (rare; normally heartbeat suffices) | no |

**CommandRequest** body:

```json
{
  "command_id": "cmd-01HZX...",
  "kind": "EnsureContainerRunning",
  "spec": { ... },
  "deadline": "2026-04-24T14:10:00Z"
}
```

The `kind` is an enum of goal-state operations. Initial catalogue:

- `EnsureContainerRunning { spec: ContainerSpec }` — declarative;
  agent reconciles to the spec.
- `EnsureContainerAbsent { name }` — remove container and free
  resources.
- `EnsureImagePresent { ref, digest? }` — pull if missing or if
  digest doesn't match.
- `StreamLogs { container, since, follow }` — start pushing
  `LogChunk` events for a container until `StopStreamLogs`.
- `StopStreamLogs { container }`.
- `InspectContainer { container }` — one-shot observation, replies
  with a `CommandResponse` carrying the inspect payload.
- `RunCommand { spec: CommandSpec }` — SRD-0107 run-to-completion
  container with captured stdout/stderr/exit-code.

The `deadline` is a soft upper bound on how long the agent is
permitted to work on this command. Past the deadline, the agent
aborts the in-progress work and returns `status=error, code=deadline`.

SRD-0106 and SRD-0107 extend this catalogue with their specific
semantics (service vs command element).

## D7 — Agent lifecycle state machine

Five states, scoped to the agent process itself (distinct from
the node lifecycle in SRD-0104):

```
  starting ──► registering ──► ready ──┬──► draining ──► dead
                   │                   │
                   └──► dead           └──► (back to ready on reconnect)
```

| State | Meaning | Entered by |
|---|---|---|
| `starting` | Binary launched, reading config, opening Docker socket | systemd start |
| `registering` | WebSocket open, `Register` sent, awaiting controller ack | successful TCP+TLS connect |
| `ready` | Controller acknowledged; accepting `CommandRequest`s | controller ack |
| `draining` | Received `Drain` or `Shutdown`; finishing in-flight work, rejecting new | controller command |
| `dead` | Process exiting | any terminal path |

**Ready invariants.**

- The Docker event stream (D10) is open or the agent has declared
  `DockerUnavailable` and the node is flagged degraded.
- Heartbeats are being emitted on schedule.
- The WebSocket is open.

If any of these becomes false, the agent transitions back toward
`registering` (for WebSocket loss) or surfaces an event and stays
in `ready` with a partial-service flag (for Docker-socket loss).

The node-lifecycle state machine (SRD-0104) observes the agent
through registration + heartbeat and maps to its own terms
(REGISTERING, REGISTERED, ACTIVE_HEARTBEAT, LOST_HEARTBEAT).

## D8 — Liveness: heartbeat + timeout

- **Agent cadence.** `Heartbeat` every 10 s.
- **Controller expectation.** A heartbeat is expected every 10 s ±
  a 5 s slack. The controller considers the agent `awaiting` after
  12 s with no message of any kind.
- **Timeout.** 30 s without any inbound traffic from the agent
  (heartbeat, event, response, anything) marks the agent
  `lost-heartbeat`. The controller closes the WebSocket with a
  `HeartbeatTimeout` close code and waits for a reconnect.
- **Node-level projection.** A lost-heartbeat does not by itself
  tear the node down; it marks the node non-ready in the topology
  view. The node element (SRD-0104) decides whether the timeout
  warrants re-provisioning based on its own retry rules.

Thresholds ported directly from the Java node-lifecycle doc
(heartbeat every 10 s, timeout at 30 s) — no reason to change
what works. Tuneable via controller config (SRD-0112).

## D9 — Reconnection + re-association

On WebSocket loss, agent reconnects with exponential backoff:
1 s → 60 s ceiling, doubling each attempt, full-jitter. Retries
continue for a total 15-minute window. If unsuccessful after that,
the agent exits non-zero; systemd starts a fresh agent instance
that goes through registration from scratch.

**Re-association.** On reconnect the agent presents the same
`auth-token`, sends `Register` again with a fresh `startup_containers`
observation. The controller recognises the `agent_id`, updates its
record of "what is currently running on this node," reconciles
against expected state, and issues any `CommandRequest`s needed
to close the gap (e.g. a container that should be running but
isn't).

**Duplicate connection handling.** If a prior WebSocket is still
open when a new `Register` arrives (e.g. the controller didn't
see the old close), the controller closes the old connection with
`Superseded` before accepting the new one. There is at most one
live WebSocket per `agent_id` at any time.

**Docker event resume.** On reconnect, the agent resumes its
`/events` subscription at the last-observed `time_nano` minus a
small overlap (1 s), so no events are lost. Duplicates are
tolerated; event consumers downstream are idempotent.

## D10 — Docker daemon event integration

Subscribe to the Docker Engine `/events` endpoint via the `bollard`
crate:

```
Docker::events(Option<EventsOptions>) -> impl Stream<Item = Result<EventMessage, Error>>
```

The endpoint is a long-lived streaming HTTP connection; each event
is a JSON object over chunked transfer.

- **Event shape we care about.** `EventMessage` carries `type`
  (container / image / volume / network / daemon / plugin /
  service / node / secret / config), `action` (for containers:
  `create`, `start`, `die`, `stop`, `kill`, `oom`, `pause`,
  `unpause`, `restart`, `health_status`, `destroy`, `exec_die`,
  and more), `actor` (id + label attributes), `scope`, `time`,
  `time_nano`.
- **Filtering.** `EventsOptions.filters` is a
  `HashMap<String, Vec<String>>`; we filter `type=container` plus
  optional `container=<id>` / `label=<k>=<v>` to keep the stream
  tight to what the agent actually manages.
- **Forwarding.** Each event is wrapped into an `EventPush`
  message (D5) and sent to the controller. The agent does not
  interpret the event beyond forwarding; the controller reconciles
  against plan state.
- **Windowing.** `since` / `until` parameters accept RFC-3339
  timestamps; on reconnect after a drop, resume from the last-seen
  `time_nano` (D9).
- **Failure modes.** Docker daemon restart closes the stream;
  agent reconnects with exponential backoff. Permanently missing
  socket → agent flags the node unhealthy and surfaces a
  `DockerUnavailable` event on the hyperplane WebSocket.
- **Fallback.** If the `/events` socket can't be reached (non-Docker
  runtime, socket permission failure), fall back to a polling
  loop over `docker inspect` against known container ids with a
  1–2 s cadence. Flag the degraded mode via an event so operators
  know the push path is down.

Rationale for push over poll: the Java implementation's polling
loop was the largest source of controller-observation staleness.
Push eliminates the polling-interval lower bound on reaction
latency, and `bollard` exposes the native `/events` stream
directly — no bespoke HTTP chunking code on our side.

## D11 — `Shutdown` is a node-shutdown command

`Shutdown` instructs the agent to initiate shutdown *of the node*
through whatever OS-level mechanism is available (typically
`shutdown -h now` / `systemctl poweroff` or equivalent). The
agent dying is a consequence of the node going away, not the goal
of the command.

Per SRD-0100 D13 (`INV-LIFECYCLE-INDEPENDENT`) the agent's own
process lifecycle is bound to the node's lifecycle — the agent is
alive and connected for the duration of the node it's assigned
to, no longer and no shorter.

Flow:

1. Controller sends `Shutdown` (typically as part of executing a
   `TeardownElement` step for a Node element — SRD-0104).
2. Agent acknowledges on the WebSocket, transitions to `draining`,
   stops accepting new `CommandRequest`s.
3. Agent invokes the OS shutdown.
4. OS tears down containers (Docker daemon receives SIGTERM), the
   agent process, the network, and eventually the machine.
5. Controller observes the node state via the cloud provider's
   instance-state API (SRD-0104) and completes the teardown
   accounting.

The agent therefore needs privileges to shut the node down; the
narrow sudo rule (D1) grants exactly `shutdown -h now` /
`systemctl poweroff` and nothing else.

**INV-SHUTDOWN-NODE-LEVEL.** `Shutdown` ends the node, not just
the agent. Any path that treats `Shutdown` as an agent-process
terminator (systemctl stop hyperplane-agent) without taking the
node down is a violation: it leaves containers orphaned under a
live Docker daemon with no hyperplane observability.

## D12 — Commands are declarative, idempotent by construction

The command catalogue (D6) expresses goal states, not imperatives.
Retry of the same command against the same goal state is a no-op
by construction — no dedup keys needed. Examples:

- **Not** `StartContainer { image, args, … }` (imperative).
- **But** `EnsureContainerRunning { spec: FullContainerSpec }`
  (declarative goal state).

The agent reconciles: if the declared container is already
running matching `spec`, no action. If missing, create + start.
If mismatched, stop + recreate.

This pattern generalises up the stack — SRD-0108 API writes
inherit it, so no idempotency-key headers are needed on the
controller API either. The tradeoff is that command authors must
think in goal-state terms; the payoff is robust retry semantics
at every layer.

**INV-COMMAND-IDEMPOTENT.** Every agent command must be safe to
execute zero, one, or many times against the same goal-state
payload. New `CommandRequest` kinds added to D6 must either
satisfy this or be explicitly flagged as non-reconcilable in
their SRD with a justification.

## D13 — New invariants

Stable identifiers introduced by this SRD. All are cited in tests.

| Code | Invariant |
|---|---|
| `INV-AGENT-SSH-ONCE` | SSH to a running agent's node is not used for control traffic. |
| `INV-AUTH-TOKEN-PLAINTEXT-EPHEMERAL` | The plaintext `auth-token` exists only in transit and in the agent's config file; controller stores only the hash. |
| `INV-SHUTDOWN-NODE-LEVEL` | `Shutdown` ends the node, not just the agent. |
| `INV-COMMAND-IDEMPOTENT` | Every agent command is safe under retry against the same goal-state payload. |

These extend — not replace — the SRD-0100 catalogue. Any new
agent-side message kind must cite its effect on these invariants
in the amending SRD.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/docs/NODE-LIFECYCLE.md` §§ 5–6 (agent
  registration, heartbeat, disconnect) — porting heartbeat
  cadence (10 s) and timeout (30 s) unchanged.
- `~/projects/hyperplane/docs/NODE-CONTRACT.md` — node-side
  expectations the agent relies on.
- `~/projects/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/agent/` — Java agent-server
  reference implementation.
- `bollard` crate `Docker::events` — the push-driven event stream
  this SRD depends on.
