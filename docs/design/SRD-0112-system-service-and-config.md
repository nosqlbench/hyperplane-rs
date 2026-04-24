<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0112 — System Service Management & Configuration

## Purpose

Specify how hyperplane lives on a user's machine: the on-disk
config layout, how the local service stack is started /
stopped / supervised, how credentials are generated + rotated,
how databases are located + backed up, and how multi-user
sharing is handled when two users run against the same
hyperplane install. Ops concerns live here so other SRDs
don't each reinvent them.

Hyperplane is architecturally a set of cooperating processes
— controller, web server, optional co-resident Docker
registry — run on an operator's machine. The operator is a
single user (typically) with administrative authority over
that machine; end-users interact via CLI / web UI.

## Scope

**In scope.**

- Config directory layout (XDG-canonical).
- Credential model — system API key, SSH deploy keys, AWS
  credential lookup.
- `hyper system` commands — `start`, `stop`, `status`.
- Process supervision model — what starts what, restart
  rules, stacking order.
- Database location + backup / restore.
- Multi-user sharing on one install.
- Version + schema migration policy on binary upgrade.
- Config file format + validation.

**Out of scope.**

- EC2-side / agent-side config (SRD-0104, SRD-0105).
- What CLI commands exist (SRD-0109).
- Principal + role model (SRD-0114).

## Depends on

- SRD-0100 (controller invariants, naming conventions).
- SRD-0101 (state boundaries — this SRD owns where the DB
  file lives).
- SRD-0108 (controller API — credentials authenticate
  against it).
- SRD-0114 (principals — end-user accounts managed by
  controller, not by this SRD).

---

## Process topology at a glance

![systemd-supervised process topology: three unit files — hyperplane-controller.service (Restart=always), hyperplane-web.service (Restart=always, needs system API key), and optional hyperplane-registry.service (Restart=on-failure). `hyper system start` orchestrates the start-order controller → web → registry with health-waits between each.](diagrams/SRD-0112/process-topology.png)

## Start / stop ordering

![Start sequence: migrations run, controller start, wait for /api/v1/health, web start, wait for web health, registry start if co-resident, SYSTEM_STARTED event. Stop sequence in reverse: stop registry, stop web with 30s drain, stop controller with 30s drain, SYSTEM_STOPPED event.](diagrams/SRD-0112/start-stop-ordering.png)

## D1 — Config directory layout

XDG-canonical. Two roots:

- **Config** — `$XDG_CONFIG_HOME/hyperplane/` (default
  `$HOME/.config/hyperplane/`). Human-editable; version-
  controllable by the operator.
- **State + data** — `$XDG_DATA_HOME/hyperplane/` (default
  `$HOME/.local/share/hyperplane/`). Machine-managed;
  databases, credentials, artifact blob storage.
- **Cache** — `$XDG_CACHE_HOME/hyperplane/` (default
  `$HOME/.cache/hyperplane/`). Regenerable.

Tree:

```
$XDG_CONFIG_HOME/hyperplane/
├── hyperplane.toml              # top-level system config
├── controller.toml              # controller-specific config
├── web.toml                     # web-server config (SRD-0110 D1)
├── agent.defaults.toml          # defaults injected into agent deploys
├── ec2-node-allowlist.toml      # SRD-0104 D2 allow-list
└── credentials/
    ├── system-api-key           # system API key (mode 0600)
    ├── deploy-ssh-key           # ephemeral; regenerated per-deploy
    └── registry.toml            # docker registry auth, if any

$XDG_DATA_HOME/hyperplane/
├── hyperplane.db                # single SQLite file (SRD-0101 D6)
├── hyperplane.db-wal            # WAL sidecar
├── hyperplane.db-shm            # shared-memory sidecar
├── artifacts/                   # ArtifactStore blob storage
│   └── {artifact-id-shard}/
└── backups/                     # rotated database snapshots

$XDG_CACHE_HOME/hyperplane/
├── completion/                  # CLI observation cache (SRD-0109 D5)
└── tmp/                         # ephemeral working space
```

**File modes.**

- Config files: 0644 (human-readable, editable by owner).
- Credentials: 0600.
- Database + artifact files: 0600. Owned by the hyperplane
  service user on multi-user installs (D6).

**Operator overrides.** Every path above can be overridden
in `hyperplane.toml` if the defaults don't suit. Overrides
apply to every subcomponent that reads from the tree.

## D2 — `hyperplane.toml` top-level schema

```toml
# $XDG_CONFIG_HOME/hyperplane/hyperplane.toml

[install]
data_dir    = "${XDG_DATA_HOME}/hyperplane"
cache_dir   = "${XDG_CACHE_HOME}/hyperplane"

[controller]
listen_addr = "0.0.0.0:8443"
tls_cert    = "/etc/hyperplane/tls/cert.pem"
tls_key     = "/etc/hyperplane/tls/key.pem"
config_file = "controller.toml"     # relative to config dir

[web]
listen_addr = "0.0.0.0:8090"
config_file = "web.toml"

[registry]
mode        = "co_resident"         # "co_resident" | "external" | "disabled"
listen_addr = "127.0.0.1:5000"      # only for co_resident
external_url = ""                   # only for external

[aws]
credentials_chain = ["env", "profile", "instance_role"]
default_profile   = "hyperplane"

[events]
retention = "90d"                   # D9 retention policy

[backups]
enabled  = true
schedule = "daily"
retain   = 14                       # keep 14 days
```

**Validation.** Loaded at process start; every unknown field
is a hard error (no silent-ignore — prevents typo'd configs
from appearing to work). Every required field missing is
also a hard error.

**Reload.** `SIGHUP` reloads config in the controller where
safe (allow-lists, retention, backups schedule). Listen
addresses + TLS certs require a restart. `hyper system
restart-config` is a convenience wrapping this.

## D3 — `hyper system` commands

Three subcommands (per the resolved ruling — no `doctor`).

### `hyper system start`

Starts the whole stack in order:

1. Create data directories if missing.
2. Run database migrations (D8).
3. Start the controller process.
4. Wait until the controller's `/api/v1/health` returns
   `ok`.
5. Start the web server process.
6. Wait until the web server's own health probe passes.
7. If `registry.mode=co_resident`, start the local Docker
   registry.
8. Emit a `SYSTEM_STARTED` event to the event stream and
   exit 0.

Failure at any step aborts; the command prints the step that
failed + the component's startup log tail. Partial starts
are not left running — on abort, already-started components
are stopped in reverse order.

### `hyper system stop`

Reverse order:

1. Stop the Docker registry (if co-resident).
2. Stop the web server; wait for connections to drain
   (default 30s, configurable via `web.stop_grace`).
3. Stop the controller; wait for in-flight API calls to
   complete (default 30s).
4. Emit a `SYSTEM_STOPPED` event and exit 0.

Graceful by default. `--force` skips the wait and sends
SIGKILL on timeout.

### `hyper system status`

Reports per-component status:

```
$ hyper system status
controller   running (pid 12345)  healthy     since 2026-04-24 14:00:00
web          running (pid 12346)  healthy     since 2026-04-24 14:00:01
registry     running (pid 12347)  healthy     since 2026-04-24 14:00:02
database     reachable            WAL: 2.1 MB  size: 142 MB
events       retention: 90d       oldest: 2026-01-24  size: 11 MB
```

Exit codes: 0 if everything healthy, 1 if degraded, 2 if
down.

**Idempotence.** `start` when already running is a no-op
(returns 0 with an informational message). `stop` when
already stopped is likewise a no-op.

**No per-service targeting on `start` / `stop`** by default.
Operators restarting a single component use supervisor-level
tools (`systemctl restart hyperplane-web`) directly; the
`hyper system` group is for the stack as a whole.

## D4 — Process supervision model

**Preferred: systemd** on Linux systemd distributions.

Three unit files, installed by a `hyper system install`
subcommand (or manually by the operator):

```
hyperplane-controller.service
hyperplane-web.service
hyperplane-registry.service      # optional
```

`Restart=always` on the controller and web server. The
registry unit's `Restart` is `on-failure` only (transient
crashes) — a clean exit means an operator intentionally
stopped it.

**`hyper system start` delegates to systemd** when it's
available: the command issues `systemctl start
hyperplane-*.service` in the correct order and polls for
readiness. Operators can also bypass `hyper system` and use
systemd directly — the `hyper` CLI is a convenience, not a
mandatory supervisor.

**Non-systemd fallback.** `hyper system start --foreground`
runs the components as child processes of the CLI invocation
(useful for debugging, bare-metal installs without systemd,
containerized environments). SIGINT on the parent cascades
to children. This is a development-oriented mode, not a
production recommendation.

**Ports.** Per the resolved ruling, ports are config-driven
and bind failure is an error. The controller and web server
bind on startup; an in-use port causes immediate exit with
`BindFailed` and the operator adjusts config. No
automatic port discovery, no "try the next port."

## D5 — Credential model

**System API key.** The web server's credential for
authenticating to the controller (SRD-0110 D8).

- **Mint.** Generated on first `hyper system init` run.
  32 bytes from a CSPRNG, base64url-encoded.
- **Storage.** `$XDG_CONFIG_HOME/hyperplane/credentials/system-api-key`,
  mode 0600. The controller stores its hash in the
  database; the plaintext is readable only by the web
  server's process user.
- **Rotation.** `hyper system rotate-api-key` mints a new
  value, updates both file + database atomically, prompts
  the operator to restart the web server (which reads the
  key at start). No live-rotation without restart in v1 —
  would complicate the web server's in-flight request
  handling.

**SSH deploy keys.** Ephemeral keypairs the controller
generates per EC2Node deploy (SRD-0104 D6). Not persisted
long-term; live in the credential store only while a deploy
is in-flight. Deleted post-deploy or post-teardown.

**AWS credentials.** Looked up via the standard AWS
credential chain, with order configurable in
`hyperplane.toml` (D2):

1. `env` — `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
   `AWS_SESSION_TOKEN` environment variables.
2. `profile` — `~/.aws/credentials` profile (named per
   config).
3. `instance_role` — EC2 IAM role, if hyperplane itself runs
   on EC2.

The controller process is the only component that needs AWS
credentials; CLI and web server never make AWS API calls
directly.

**Bearer tokens for end-users.** Issued by the controller
per SRD-0108 D1 + SRD-0114 D5. Not in this SRD's storage
model — end-users hold their own tokens on their own
machines (CLI per SRD-0109 D3; browser-side sessions per
SRD-0110 D5).

## D6 — Multi-user sharing

**Default install mode: multi-user shared.** One hyperplane
install, shared across multiple end-users. Process owners:

- Controller runs as a dedicated `hyperplane` system user.
- Web server runs as the same user.
- Data directory is owned by `hyperplane:hyperplane` with
  mode 0700 on the root.

End-users authenticate to the controller via bearer tokens
(SRD-0108 + SRD-0114). Their operating-system user doesn't
matter to the controller — the principal comes from the
token.

**Single-user convenience mode.** For development, a solo
operator may run everything under their own user account —
no dedicated service user, no systemd units. `hyper system
start --foreground` covers this. Data directory can be under
`$HOME/.local/share/hyperplane/` directly.

**Installing.** `hyper system install` creates the service
user, drops systemd unit files, sets up the data directory
with proper ownership, and runs `hyper system init` (D7) in
one step. Requires root / sudo.

**What doesn't change between single-user and multi-user.**
The principal model — every operation's auditable actor is
a hyperplane user, not a Unix user. This decouples the
hyperplane auth model from whichever host OS shell happens
to be running the client.

## D7 — `hyper system init` bootstrap

![hyper system init flow: 1. create data directory; 2. run migrations (paramodel-store first, then hyperplane-store, with consistency check); 3. mint system API key at credentials/system-api-key (0600); 4. prompt for initial admin user (username + password); 5. mint bootstrap admin token (24h expiry) and print to stdout; 6. emit SYSTEM_INITIALIZED event and exit 0. Re-running on an initialised install fails AlreadyInitialized; destructive re-init requires deleting the data directory first.](diagrams/SRD-0112/bootstrap-init.png)



First-run bootstrap. Creates initial state when the install
has none:

1. Creates the data directory if missing.
2. Initializes the SQLite database (runs all migrations to
   current version).
3. Mints the system API key (D5).
4. Creates a bootstrap `admin` user per SRD-0114 D13 — prompts
   for username + password.
5. Mints a bootstrap admin token (24h expiry per SRD-0114
   D13) and prints it on stdout with instructions for
   `hyper login`.
6. Emits `SYSTEM_INITIALIZED` to the event stream (which at
   this point exists, having been created by migrations).
7. Exits 0.

Rerunning `init` on an already-initialized install is
rejected with `AlreadyInitialized` — destructive re-init is
an operator's explicit decision (delete the data directory
first).

## D8 — Versioning + schema migration

**Semantic versioning** on the hyperplane binary
(controller + web server + CLI ship together as one
version).

**On binary upgrade:**

1. Operator stops the running stack (`hyper system stop`).
2. Operator replaces the binaries.
3. Operator runs `hyper system start`.
4. Start-up runs migrations (SRD-0101 D5):
   - `paramodel-store-sqlite` migrations first.
   - `hyperplane-store` migrations next.
   - Consistency check for bridge tables.
5. Stack starts; new version running.

**Compatibility window.** Version N must open a database
last-written by N-1 (migrations forward). N writing a
database N+1 opens cleanly is not guaranteed — rollback
requires a database restore (D10).

**Migration safety.** Each migration is scripted + checked;
catastrophic failures (partial writes, schema violations)
roll back the whole migration transaction and exit start-up
with a clear error.

**No live-migration.** Binary replacement is a downtime
event. Long-running executions are paused at the trial
boundary (SRD-0011); resumed after upgrade via paramodel's
resume story.

## D9 — Event retention

Per the `events.retention` config key (D2). Two modes:

- **Time-window** — `90d` keeps events newer than 90 days.
  Background eviction runs hourly.
- **Row-cap** — `100_000_000` keeps the most recent 100M
  events. Eviction on every insert that pushes the count
  above the cap.

Defaults: time-window `90d`.

**INV-EVENT-IMMUTABLE** is preserved — eviction is time-
scoped deletion, not modification of retained events.

**Pre-eviction snapshots.** Operators can configure a
pre-eviction hook to export events being evicted to cold
storage (S3, tarball, whatever). Default: no hook;
evicted events are gone.

## D10 — Database backup + restore

**Live backup.** A background task runs SQLite's online-
backup API (`sqlite3_backup_*`) on the schedule configured
in `backups.schedule`. Backups land in
`$XDG_DATA_HOME/hyperplane/backups/{timestamp}.db`.

**Retention.** `backups.retain` (default 14) bounds the
number of backups kept. Older backups are deleted as new
ones arrive.

**Filesystem snapshot alternative.** Operators on
ZFS / Btrfs / LVM can take filesystem snapshots with the
stack stopped — cleaner than the online backup, but
requires a stop window.

**Restore.** `hyper system restore <backup-path>` stops the
stack, replaces `hyperplane.db` with the backup, runs
migrations forward if the backup is older than the current
binary, starts the stack.

**Tooling.** `hyper system backup now` takes an immediate
backup. `hyper system backup list` shows available
backups.

## D11 — Configuration file precedence

Order of resolution for any setting:

1. CLI `--flag` on the invoking command.
2. Environment variable `HYPERPLANE_*` (e.g.
   `HYPERPLANE_CONTROLLER_LISTEN_ADDR`).
3. Config file entry.
4. Compiled-in default.

Every layer's sources are listed on `hyper system status
--config` for debugging ("where does this value come
from?").

## D12 — Cross-references

- Event retention (D9) + `INV-EVENT-PERSIST-ALL` (SRD-0111
  D11) compose: events persist unconditionally within the
  retention window; the window is operator policy; eviction
  outside the window preserves `INV-EVENT-IMMUTABLE`.
- AWS credential chain (D5) feeds EC2Node provisioning
  (SRD-0104 D3).
- System API key + bootstrap admin token (D5, D7) feed
  SRD-0108 D1's auth tiers and SRD-0114 D13's bootstrap
  flow.

## D13 — New invariants

| Code | Invariant |
|---|---|
| `INV-SYSTEM-SINGLE-STACK-PER-INSTALL` | One hyperplane install runs one controller + one web server + optionally one registry. No sharded / clustered topology in v1. |
| `INV-CONFIG-STRICT` | Unknown config fields are rejected at start-up; required fields missing are start-up errors. |
| `INV-PORT-BIND-FATAL` | A configured port unavailable at bind is a fatal start-up error; no port fallback. |

Extends SRD-0100.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/docs/ENVIRONMENT.md` — Java-era
  environment-variable story; ported into the config file
  layout here.
- `~/projects/hyperplane/env.sh` — Java-era bootstrap
  script; its content is replaced by `hyper system init`.
- XDG Base Directory spec.
- systemd unit documentation.
