<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0114 — Principals, Roles, Permissions & Sharing

## Purpose

Specify hyperplane's multi-user authorization model — principals,
roles, permissions, API-token scopes, resource ownership,
workspaces, sharing, the security audit journal, and the impersonation
contract between the web server and the controller.

The Java hyperplane project ships a mature multi-user design
(`~/projects/hyperplane/docs/design/multi-user-design-sketch.md`).
This SRD ports that design substantially unchanged into the Rust
port, adapting only where Rust-port concerns diverge (agent and
system-account principal kinds, which didn't exist in the Java
project). Everything defined here is enforced inside the
controller (SRD-0100 `INV-CTL-SOLE-WRITER`); the web server,
CLI, and SDK are clients of the controller's authorization
checks, not implementors of them.

## Scope

**In scope.**

- Principal kinds: end-user, agent, system account.
- Users, passwords, the auto-created `system` user.
- Groups + group membership.
- Roles: `viewer`, `user`, `admin` — multi-role assignment, session
  toggleability, non-hierarchical / additive semantics.
- API tokens + scope catalogue.
- The two-gate access model — active role ∧ token scope.
- Resource ownership — every ownable resource carries an owner.
- Workspaces — logical grouping for organisation and batch sharing.
- Sharing model — `resource_shares` table, `read` / `execute`
  permissions, wildcard grants.
- Visibility resolution — ordered rules that determine what a
  principal can see and do with a resource.
- Impersonation contract — the web server acts on behalf of the
  authenticated end-user; the controller enforces against the
  end-user principal, not the system account.
- Security audit journal — dual-destination (DB + application
  log), event-type catalogue.
- Bootstrap / first-run.

**Out of scope.**

- Authentication mechanics (password hashing, session cookie
  minting, token generation) — SRD-0108 + SRD-0112.
- Jupyter-per-user or any specific user-facing feature — those
  sit in downstream SRDs that *cite* this one.
- Quota + rate-limit enforcement — deferred (SRD-0108 ruling).

## Depends on

- SRD-0100 (invariants, especially `INV-CTL-SOLE-WRITER`).
- SRD-0101 (state boundaries — all tables in this SRD are
  controller-owned).
- SRD-0108 (controller API — endpoints declare required roles +
  scopes).
- SRD-0105 (agent auth-token lifecycle).
- SRD-0112 (credential lifecycle + config directory).

---

## Authorization model at a glance

```
  principal        ──┐
  (end-user/       ──┤
  agent/system)    ──┘
        │
        ▼
  ┌───────────────┐       ┌──────────────┐
  │ active roles  │  AND  │ token scopes │     both must permit
  │ (viewer/user/ │       │ (fine-grain  │        for access
  │  admin)       │       │  capability) │
  └───────────────┘       └──────────────┘
        │                        │
        └────────┬───────────────┘
                 ▼
        ┌──────────────┐
        │  per-request │        then visibility check:
        │  authz gate  │        ownership / workspace /
        └──────┬───────┘        resource_shares / public
               │
               ▼
         access decision
```

## Impersonation at a glance

```
  browser            web server                controller
     │                   │                          │
     │── login ─────────▶│                          │
     │                   │── POST /api/v1/auth/login ─▶
     │                   │◀── user_id + scopes ─────│
     │◀── session cookie │                          │
     │                   │                          │
     │── GET /nodes ────▶│                          │
     │                   │── GET /api/v1/nodes ────▶│
     │                   │   Authorization:         │
     │                   │     Bearer <system-key>  │
     │                   │   X-On-Behalf-Of: <user> │
     │                   │                          │
     │                   │       controller runs    │
     │                   │       handler with       │
     │                   │       effective_principal│
     │                   │       = <user>           │
     │                   │                          │
     │                   │◀── response (user-scoped)│
     │◀── HTML (composed)│                          │
```

## Resource visibility resolution

```
  For each resource request, check in order:

    1. Principal is owner?      ──▶ allowed
    2. Resource is public?      ──▶ allowed (read only, unless shared)
    3. Explicit resource_share  ──▶ allowed (with share's permission)
       - to this principal?
       - to one of this principal's groups?
       - to this principal's active workspace?
    4. Admin role active?       ──▶ allowed
    5. No match                 ──▶ denied (silent or 404, by context)
```

## D1 — Principal kinds

Three kinds of principal authenticate against the controller:

| Kind | Acquired via | Credential | Typical use |
|---|---|---|---|
| **End-user** | `hyper login` (CLI/SDK) or web-server login | Bearer token / session cookie | A human operating the system. |
| **Agent** | SSH deploy handshake (SRD-0105) | `auth-token` | A long-running agent on a node. |
| **System account** | Provisioned at install (SRD-0112) | System API key | The web server; internal automation. |

Every authenticated request carries a principal identity in the
controller's `RequestContext`. The permission checks fire against
that principal — not against the transport credential.

**End-users** own resources, hold roles, belong to groups, and
drive the workflow this system exists for.

**Agents** act on behalf of the plan, not on behalf of a user.
They have a fixed role (`agent`, not one of the three user roles)
and a narrow, enumerated set of permissions — basically "accept
commands from controller, push events / heartbeats / logs upstream."
They are not part of the sharing / workspace model.

**System accounts** are used by the web server to authenticate to
the controller when it has no end-user to impersonate (e.g., on
startup to fetch config). The web server *also* impersonates
end-users for browser-driven requests (D11). A request may carry
*both* a system API key (for transport authentication) and an
impersonated end-user principal (for permission checks).

---

## D2 — Users

```
users
  username              TEXT PRIMARY KEY
  password_hash         TEXT NOT NULL
  display_name          TEXT
  default_share_level   TEXT NOT NULL DEFAULT 'private'  -- 'private' | 'read' | 'execute'
  enabled               BOOLEAN NOT NULL DEFAULT TRUE
  created_at            TEXT NOT NULL
  updated_at            TEXT
```

`default_share_level` is a user preference that controls what
happens when a user creates a new resource:

- `private` — only the owner can see it (default).
- `read` — all authenticated users can see it via a
  `granted_to='*'` share written automatically.
- `execute` — all authenticated users can see and execute it.

Changeable at any time; does not retroactively alter existing
resources.

### The `system` user

The `system` user is a regular user with both `admin` and `user`
roles assigned. Auto-created on first `hyper system init`.
Resources owned by `system` follow the same sharing rules as any
other user's — no special-case logic.

---

## D3 — Groups

```
groups
  group_id        TEXT PRIMARY KEY
  display_name    TEXT NOT NULL
  description     TEXT
  created_by      TEXT NOT NULL REFERENCES users(username)
  created_at      TEXT NOT NULL

group_members
  group_id        TEXT NOT NULL REFERENCES groups(group_id)
  username        TEXT NOT NULL REFERENCES users(username)
  group_role      TEXT NOT NULL DEFAULT 'member'  -- 'owner' | 'member'
  added_at        TEXT NOT NULL
  PRIMARY KEY (group_id, username)
```

Groups are an optional convenience for batch sharing. A user may
belong to zero or more groups; a group has one or more owners.
Group ownership grants the right to add / remove members.

---

## D4 — Roles: independent, additive, toggleable

Three user-facing roles:

| Role | Capabilities |
|---|---|
| **`viewer`** | Read-only access to everything they can see. Cannot create, modify, execute, or share. |
| **`user`** | Create + own resources. Execute plans / studies on shared infrastructure. Share resources they own. |
| **`admin`** | Manage users / groups / roles. Manage shared infrastructure (nodes, system config). Full access to any resource. |

**Non-hierarchical, additive.** Holding multiple roles grants the
union of their capability sets. `viewer` does *not* include
"cannot create" as a rule — it simply lacks create-capability.
`admin` does *not* strictly include `user`'s capabilities by
hierarchy — it has its own capability set that happens to cover
most of `user`'s by design. Holding `[viewer, admin]` is the union
of both.

### Assignment

```
user_roles
  username        TEXT NOT NULL REFERENCES users(username)
  role            TEXT NOT NULL  -- 'viewer' | 'user' | 'admin'
  assigned_by     TEXT NOT NULL REFERENCES users(username)
  assigned_at     TEXT NOT NULL
  PRIMARY KEY (username, role)
```

Every user must hold at least one role. Default on user creation
is `user`. Role assignment is an `admin`-role action (gated
per the two-gate model in D10).

### Session toggling

A session carries *active roles* — the subset of assigned roles
currently in effect. All assigned roles are active by default on
login; the user may toggle individual roles on/off within the
session.

**Toggling is unguarded by permissions.** If you hold the role,
you can toggle it. The only enforced invariant is
`active_roles ⊆ assigned_roles`.

Toggling serves three purposes:

1. **Safely test as a lower-privilege user.** An admin can
   deactivate the admin role to preview what a `viewer` or `user`
   sees.
2. **Reduce blast radius.** Work only with the roles needed for
   the current task.
3. **Validate sharing.** Check that a shared resource presents
   correctly from a viewer's perspective without a separate
   account.

### The `agent` role

Agents hold exactly one fixed role — `agent` — with a narrow
permission set (see D6). Agents cannot hold user roles and user
roles cannot be assigned to agent principals.

---

## D5 — API tokens + scopes

```
api_tokens
  token_hash      TEXT PRIMARY KEY  -- SHA-256 of raw token
  prefix          TEXT NOT NULL     -- 'hp' (user tokens) / 'ha' (agent tokens) / 'hs' (system key)
  principal_kind  TEXT NOT NULL     -- 'user' | 'agent' | 'system'
  principal_id    TEXT NOT NULL     -- username / agent_id / system-account name
  display_name    TEXT              -- user-assigned label
  scopes          TEXT NOT NULL     -- JSON array of scope strings
  created_at      TEXT NOT NULL
  expires_at      TEXT              -- NULL = no expiry
  last_used_at    TEXT
  revoked_at      TEXT              -- NULL = active
```

Raw tokens are shown once at creation; the store holds only the
hash.

### Scope catalogue

Scopes are `resource:action`:

| Scope | Permits |
|---|---|
| `*` | Full access (bounded by role). |
| `plans:read` | Read own + shared plans. |
| `plans:write` | Create / modify / delete own plans. |
| `plans:execute` | Execute plans. |
| `executions:read` | Read own + shared executions. |
| `executions:execute` | Start / stop executions. |
| `templates:read` | Read own + shared templates. |
| `templates:write` | Create / modify / delete own templates. |
| `configs:read` | Read own + shared configs. |
| `configs:write` | Create / modify / delete own configs. |
| `infrastructure:read` | Read node / service status. |
| `infrastructure:manage` | Provision / terminate nodes (requires `admin` role). |
| `workspaces:read` | Read own + shared workspaces. |
| `workspaces:write` | Create / modify / delete own workspaces. |
| `shares:manage` | Create / revoke shares on owned resources. |
| `users:manage` | Manage users / groups / roles (requires `admin` role). |
| `agent:connect` | Agent-only — register + maintain a WebSocket (SRD-0105). |
| `agent:events` | Agent-only — push events / heartbeats / logs. |

The scope catalogue is extensible by design — new scopes land as
endpoints land (SRD-0108), with the constraint that every
endpoint declares its required scope(s).

```
  Scope catalogue organised by resource:

  user-facing resources:
    plans           ── read | write | execute
    executions      ── read | execute
    templates       ── read | write
    configs         ── read | write
    workspaces      ── read | write
    infrastructure  ── read | manage (admin role)
    shares          ── manage  (for resources you own)

  admin-only:
    users           ── manage  (admin role)

  agent-only:
    agent           ── connect | events  (bound to agent auth-token)

  special:
    *               ── wildcard; bounded by active role
```

Every token's `scopes` field enumerates a subset of the catalogue.
Endpoints declare required scopes; the two-gate check (D6) tests
"token has ≥ required scope" AND "active role permits category."


---

## D6 — The two-gate access model

Every operation requires **two gates** to pass:

1. **Active role gate.** The principal's currently-active roles
   must include one that confers the operation's required
   capability.
2. **Scope gate.** The API token used must include the
   operation's required scope(s).

Both must agree. Examples:

- An `admin`-role user with a token scoped `plans:read` only
  cannot manage infrastructure — the scope gate denies, even
  though the role gate allows.
- A `viewer`-only user with a `*`-scoped token cannot create
  resources — the role gate denies.
- The `admin` role grants full-access capability, but the token
  scope still caps what any specific request can do.

Fine-grained per-resource sharing (D8 / D9) further restricts
*which* resources the operation applies to, but cannot grant
capabilities beyond what the active roles + token scopes allow.

### RequestContext

Every handler receives a resolved context:

```rust
pub struct RequestContext {
    pub principal:       Principal,          // end-user / agent / system account
    pub active_roles:    BTreeSet<Role>,     // currently toggled-on
    pub assigned_roles:  BTreeSet<Role>,     // all held
    pub group_ids:       BTreeSet<GroupId>,
    pub scopes:          BTreeSet<Scope>,    // from the API token
    pub impersonated_by: Option<SystemAccountId>,  // web server acting on user's behalf
}
```

---

## D7 — Ownership

Every ownable resource carries:

```
  owner        TEXT NOT NULL REFERENCES users(username)
  workspace_id TEXT REFERENCES workspaces(workspace_id)
```

Ownership is *not transferrable*. If you want a resource to become
someone else's, they copy it — the copy is theirs.

```
  Ownership vs. access:

    owner ──────── can read + write + share + delete
                   (always; absolute)

    shared users ─ can read or execute
                   (per share permission; revocable)

    workspace ──── shared members inherit access via workspace
                   (batch-sharing convenience)

    public ─────── world-readable
                   (owner's choice; still revocable)

    admin ──────── can access anything (for support/operations);
                   every access audited (SRD-0114 D12)
```


---

## D8 — Workspaces

```
workspaces
  workspace_id    TEXT PRIMARY KEY
  name            TEXT NOT NULL
  description     TEXT
  owner           TEXT NOT NULL REFERENCES users(username)
  created_at      TEXT NOT NULL
  updated_at      TEXT

  UNIQUE(owner, name)
```

Every user has a **default workspace** auto-created on first
login. Users create additional workspaces to organise related
resources. Every ownable resource row carries a nullable
`workspace_id`; `NULL` means "owner's default workspace."

Sharing a workspace shares every resource in it at the granted
permission level (via `resource_type='workspace'` in
`resource_shares`). The workspace owner retains full edit
permission regardless of how it's shared; shared users see a
read-only or read+execute view.

---

## D9 — Sharing

One table handles every share (resource-level + workspace-level):

```
resource_shares
  share_id        TEXT PRIMARY KEY
  resource_type   TEXT NOT NULL   -- 'workspace' | 'plan' | 'template' | 'config' | ...
  resource_id     TEXT NOT NULL
  granted_to      TEXT NOT NULL   -- username / 'group:<group_id>' / '*'
  permission      TEXT NOT NULL   -- 'read' | 'execute'
  granted_by      TEXT NOT NULL REFERENCES users(username)
  granted_at      TEXT NOT NULL
  expires_at      TEXT            -- NULL = no expiry

  UNIQUE(resource_type, resource_id, granted_to)
```

### Permission levels

| Permission | Allows |
|---|---|
| `read` | View the resource. Copy it into your own workspace. |
| `execute` | `read` + launch a plan run / deployment using this resource. |

**There is no `write` share.** If you want to modify something,
copy it — now it's yours. Keeps ownership clean, avoids co-editing
complexity.

### Wildcard grants

`granted_to='*'` grants the permission to every authenticated
user. This is the mechanism `default_share_level` uses when the
owner sets their default to `read` or `execute`.

---

## D10 — Visibility resolution

For any resource access, the resolver checks in order. First hit
wins:

1. **Owner?** → full access (read / execute / modify / delete /
   share).
2. **`admin` in active roles?** → full access for any resource.
3. **Workspace share?** → if the resource is in a shared workspace,
   use that permission level.
4. **`granted_to='*'` share?** → the granted permission level.
5. **Direct share to user?** → the granted permission level.
6. **Share to a group the user belongs to?** → the granted
   permission level. When multiple grants apply (direct share +
   group + wildcard + workspace), the **highest permission wins**.
7. **Otherwise** → denied.

Visibility determines *which* resources you can see; the active
role still gates *what you can do*. A `viewer` seeing a resource
via an `execute` share can still only read it — the role gate
denies execute.

---

## D11 — Impersonation (web server → end-user)

The web server authenticates browsers to itself via session
cookies, and authenticates itself to the controller via the
system API key (D1). When a browser-driven request requires
proxying to the controller, the web server *impersonates* the
authenticated end-user:

1. The browser sends a request to the web server with its session
   cookie.
2. The web server resolves the cookie to an end-user principal.
3. The web server sends a controller API request carrying:
   - Authorization: its own system API key (authenticates the
     call).
   - `X-On-Behalf-Of: <username>` header (or equivalent
     envelope field) naming the end-user principal.
4. The controller validates the system API key, validates that
   the `on-behalf-of` header names a real enabled user, and
   fills `RequestContext.principal = EndUser { username }`,
   `RequestContext.impersonated_by = Some(web-server system
   account)`.
5. Permission checks fire against the end-user principal. The
   audit journal records both the acting principal and the
   impersonator.

Only system accounts may impersonate. Admin users can *not* use
their role to impersonate other users — impersonation is a
system-tier capability.

---

## D12 — Security audit journal

Every authentication + authorization + permission-change event is
recorded in a dedicated security journal. Dual-destination: both
a database table (queryable) and the application log
(grep-friendly).

```
security_journal
  entry_id        TEXT PRIMARY KEY
  timestamp       TEXT NOT NULL         -- ISO-8601 with ms
  event_type      TEXT NOT NULL
  severity        TEXT NOT NULL         -- 'info' | 'warn' | 'error'
  principal_kind  TEXT                  -- 'user' | 'agent' | 'system' (NULL for pre-auth)
  principal_id    TEXT                  -- username / agent_id / system account
  impersonated_by TEXT                  -- system account if impersonation was used
  source_ip       TEXT
  session_id      TEXT
  token_prefix    TEXT
  detail          TEXT NOT NULL         -- JSON event-specific payload
```

### Event-type catalogue

| Event type | Severity | When |
|---|---|---|
| `auth.login.success` | info | Successful login (password or token). |
| `auth.login.failure` | warn | Failed login (bad password, disabled user, unknown user). |
| `auth.logout` | info | User logged out. |
| `auth.token.created` | info | New API token minted. Records scopes; never records raw token. |
| `auth.token.revoked` | info | API token revoked. |
| `auth.token.rejected` | warn | Request with invalid / expired token. |
| `auth.session.expired` | info | Session timed out. |
| `auth.impersonation` | info | System account impersonated end-user for a request. |
| `role.assigned` | info | Admin assigned a role to a user. |
| `role.removed` | info | Admin removed a role. |
| `role.toggled` | info | User toggled active roles in session. |
| `share.created` | info | Resource or workspace shared. |
| `share.revoked` | info | Share revoked. |
| `access.denied` | warn | Authorization check failed. |
| `access.denied.scope` | warn | Token scope insufficient. |
| `user.created` | info | New user. |
| `user.disabled` | warn | User disabled. |
| `user.enabled` | info | User re-enabled. |
| `agent.auth_token.refreshed` | info | Agent auth-token rotated (SRD-0105 challenge-response). |

The journal is **read-only via API**. Retention + archival is an
admin-ops task configured in SRD-0112.

---

## D13 — Bootstrap (first run)

`hyper system init` performs first-run setup:

1. Creates the `system` user with roles `[admin, user]`.
2. Creates a default `system` workspace.
3. Mints an initial bootstrap admin bearer token, printed once to
   stdout for the operator to capture.
4. Records all of the above in the security journal as
   `system.bootstrap` events.

After bootstrap, the operator logs in with the captured token and
creates human admin users. The bootstrap token is revoked when
the first human admin promotes themselves to `admin` and revokes
it, or after a default 24-hour expiry — whichever comes first.

---

## Open questions

- **Session-store location.** The web server mints per-user
  session cookies. The cookie → principal map can live:
  - **(a) In the web server.** Cross-request map on the web
    server. Permitted (sessions are credentials, exempt under
    `INV-CREDENTIALS-NOT-STATE`).
  - **(b) In the controller.** Web server forwards the cookie
    to the controller on every request; controller resolves.
    Tighter, aligns with single-source-of-truth; one extra
    round-trip per request.
  - **(c) Self-contained cookies (JWT-style).** No server-side
    store at all. Rotation + revocation become harder.

  All three are invariant-clean. Pick before drafting the
  session-authentication section of SRD-0108.

- **Resource-kind catalogue for `resource_shares.resource_type`.**
  The Java set is `plan`, `template`, `config`, `workspace`, and
  a few others. The Rust port needs a finalised list tied to the
  element-type registry (SRD-0102).

- **Token TTL + rotation policy for user bearer tokens.**
  Agent auth-tokens rotate via challenge-response (SRD-0105).
  User tokens in the Java design are long-lived with optional
  explicit expiry. Do we tighten that (e.g. 90-day default TTL,
  forced rotation) or follow the Java posture of long-lived-with-
  manual-revoke?

- **Instance-level permissions.** The Java design is
  endpoint-level (e.g. `GET /api/nodes` requires `infra:read`
  for the whole collection). Future work: instance-level
  (`GET /api/nodes/{id}` can be denied on a per-node basis via
  resource-level sharing). v1 is endpoint-level.

- **Multi-tenancy.** The Java design assumes a single tenant
  (one admin group, one permission scope). A later SRD handles
  tenant-scoped plans / nodes / results if we need it.

- **Bootstrap token expiry** — 24 hours is a placeholder. Tune
  based on operator workflow feedback.

## Outline (maps to the D-sections above)

Structural sections are written. Remaining work for a full-draft
pass:

1. Pseudocode for the visibility resolver (D10) as a single
   function that takes `(principal, resource)` and returns
   `PermissionLevel | Denied`.
2. Endpoint catalogue for the user / group / role / share /
   workspace surfaces — belongs in SRD-0108 (this SRD names the
   model; 0108 names the URLs).
3. Conformance hooks — `paramodel-tck`-style tests that assert
   each `INV-*` code from SRD-0100 is enforced here (principal
   propagation, two-gate semantics, visibility ordering, audit
   completeness).
4. Database migration story — first-pass schema + migration
   numbering (SRD-0112 owns the migration mechanism).

## Reference material

- `~/projects/hyperplane/docs/design/multi-user-design-sketch.md`
  — the Java project's source-of-truth design. Port sections
  1 through 12 as the starting point.
- `~/projects/hyperplane/hyperplane-controller/src/main/java/com/hyperplane/controller/tracking/UserAuthTrackerMultiRoleTest.java`
  — the test suite that pins the multi-role semantics. Port the
  test shape when we implement.
- SRD-0100 D6 — `INV-CREDENTIALS-NOT-STATE` justifies session
  cookies being exempt from the no-state rule.
- SRD-0100 D11 — invariant codes this SRD's conformance hooks
  should cite.
