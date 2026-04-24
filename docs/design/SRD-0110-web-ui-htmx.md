<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0110 — Web Server (BFF + htmx UI)

## Purpose

Specify the web server — a standalone process that is the
browser's only network peer (per SRD-0100 D3, D4). It is a
Backend For Frontend: htmx-based server-rendered UI on the
outside, authenticated controller-API client on the inside.
Its job is to translate between the controller's API shape
and the shape most ergonomic for a responsive htmx UI.

No SPA. No browser-side routing framework. The web server
holds no persistent domain state — per request it fetches from
the controller, composes, renders htmx, returns. What it *does*
hold is a transient event-stream fanout buffer (per SRD-0100
D6 refinement — observations are not state), so multiple
browsers watching the same subject share a single upstream
subscription.

This SRD pins the process model, the BFF composition patterns,
the page inventory, the htmx swap strategies, the session
model, the system-API-key handling, the event-consumer wiring,
the asset pipeline, and the accessibility baseline.

## Scope

**In scope.**

- Process model — a separate `hyperplane-web` binary, its own
  port, its own config. Talks to the controller via the
  controller API (SRD-0108) using the system API key.
- Page inventory — topology, nodes, agents, plans, executions,
  results, studies, images, settings.
- **BFF patterns** — composition of multiple controller calls
  into one browser-facing endpoint, reshaping for htmx swaps,
  pre-rendering fragments.
- htmx update patterns — full-page render vs partial `hx-swap`,
  SSE or polling for live updates, auth context propagation.
- Session model — cookie-backed sessions, login flow, logout.
- Event-consumer architecture — web server subscribes to the
  controller's event stream server-side and exposes a
  browser-suitable SSE projection.
- Asset pipeline — where CSS / JS / templates live, build
  story.
- Accessibility baseline.

**Out of scope.**

- Controller API (SRD-0108).
- Agent / element details (SRD-0104/0106/0107).
- CLI autocompletion (SRD-0109).
- Specific page visual designs (owned by a design doc, not
  this SRD — this SRD owns the inventory + interaction
  patterns).

## Depends on

- SRD-0100 (browser isolation invariant, event stream
  invariants).
- SRD-0108 (controller API — UI is a client).
- SRD-0114 (principals, roles, impersonation header).

---

## BFF composition at a glance

![BFF composition sequence: browser sends GET /nodes/{id}; web server fans out in parallel (under one system API key + X-On-Behalf-Of) to GET /api/v1/nodes/{id}, /agents/{id}, /nodes/{id}/containers, /events?subject=node:... ; controller returns four payloads; web server composes via maud templates into a single HTML fragment; browser gets one rendered page in one round-trip.](diagrams/SRD-0110/bff-composition.png)

## Event fanout at a glance

![Event fanout: a single WebSocket upstream from controller feeds the web server's fanout bus (with a rolling in-memory buffer of ~200 events per scope). Multiple browsers each receive per-browser SSE streams filtered by principal. INV-WEB-EVENT-FANOUT-UPSTREAM-ONE: at most one upstream subscription per scope-family.](diagrams/SRD-0110/event-fanout.png)

## D1 — Process model

`hyperplane-web` is a single Rust binary, separate from the
controller. Deployed on the same host in most installations
but does not need to be — it talks to the controller over
HTTPS only (plus the agent-independent event stream).

**Configuration.**

| Setting | Purpose |
|---|---|
| `controller_url` | Base URL (e.g. `https://controller.internal:8443`) |
| `system_api_key` | Credential for authenticating to the controller |
| `listen_addr` | Browser-facing address, default `0.0.0.0:8090` |
| `session_secret` | Key for signing session cookies |
| `tls_cert` / `tls_key` | Optional; production deployments front with a reverse proxy |
| `asset_dir` | Override for bundled asset path (dev only) |

Configuration file at `/etc/hyperplane/web.toml` (per SRD-0112
config directory layout).

**Dependencies.** `axum` for HTTP + WebSocket, `maud` for
templates (D11), `reqwest` for controller client, `tower-http`
for middleware (sessions, logging). No database driver — the
web server is `INV-NON-CTL-NO-PERSISTENCE`.

**Process supervision.** Systemd unit
`hyperplane-web.service`, `Restart=always` with backoff.
Independent of the controller's supervision — either can
restart without the other.

**INV-WEB-DEP-GRAPH.** The `hyperplane-web` crate does not
depend on `paramodel-store-sqlite` or any other database
crate. Compile-time enforcement of `INV-NON-CTL-NO-PERSISTENCE`
per SRD-0100 D12.

## D2 — BFF composition pattern

The web server composes multiple controller calls into a
single browser-facing endpoint. The reshape is the BFF's
reason to exist: a single htmx page render should correspond
to a single round-trip between browser and web server, even if
the server fans out to multiple controller calls behind the
scenes.

**Example: the node-detail page.**

Browser request: `GET /nodes/{id}`.

Web-server fan-out (parallel, one round-trip per controller
call, all under one bearer of the system API key plus
impersonation):

1. `GET /api/v1/nodes/{id}` — core node data.
2. `GET /api/v1/agents/{id}` — agent registration + heartbeat
   state.
3. `GET /api/v1/nodes/{id}/containers?limit=20` — recent
   containers on this node.
4. `GET /api/v1/events?subject=node:{id}&limit=10` — recent
   node events.

Response to browser: a single rendered HTML fragment composed
from all four payloads. On subsequent updates (container list
changes, heartbeat arrives), htmx swaps individual panels via
SSE (D6) — the full composition happens only on page load.

**Reshape freedom.** The web server is not a transparent
proxy. It is free to:

- Flatten nested payloads that would require client-side joins.
- Pre-format timestamps in the user's timezone (session).
- Derive aggregate fields (e.g. "3 of 5 services healthy"
  from per-service reads).
- Omit fields the UI doesn't need.

The constraint is only that it never caches cross-request
domain state (`INV-WEB-NO-CACHE-STATE`). Composition buffers
are request-scoped; live event buffers are observations, not
state (D7).

**INV-WEB-COMPOSITION-SCOPE.** Every BFF endpoint is composed
from controller data fetched for that request. Carrying domain
state from a prior request into a fresh page render is a
violation. The exception is the event-stream fanout buffer (D7),
which is observation state.

## D3 — Page inventory

One inventory, each page a browser-facing endpoint. Each
page's panels correspond to BFF composition + htmx swap
targets.

| Page | Path | Purpose | Live-update panels |
|---|---|---|---|
| Dashboard | `/` | Topology overview, active executions, recent events | Topology heatmap; event stream |
| Topology | `/topology` | Graph of nodes + agents + services | Node states; agent heartbeats |
| Nodes list | `/nodes` | Tabular node inventory | Per-row status |
| Node detail | `/nodes/{id}` | Single-node panel: metadata, agent, containers, recent events | Containers; events |
| Agents list | `/agents` | Connected agents with node binding | Connection state |
| Plans | `/plans` | Plan catalogue | — |
| Plan detail | `/plans/{id}` | Plan editor + execution history | Execution status |
| Executions | `/executions` | Active + recent executions | Step progress; trial status |
| Execution detail | `/executions/{id}` | Per-step progress, per-trial outcomes, live log tail | Steps; trials; logs |
| Results | `/results` | Cross-execution result viewer | — |
| Studies | `/studies` | Study catalogue | — |
| Study detail | `/studies/{id}` | Study-level rollup | — |
| Images | `/images` | Registered image catalogue, param-space viewer | — |
| Settings | `/settings` | Current user's preferences + tokens | — |
| Admin | `/admin/*` | User/role/workspace management | Users table; audit events |

**Routing convention.** Paths mirror controller resources;
browser URLs are bookmarkable. Full-page renders on path
navigation (htmx `hx-boost` handles pjax-style partials).
Panels within a page use `hx-get` to nested BFF endpoints:
`/nodes/{id}/panel/containers` returns only the containers
fragment, swapped into the target element.

## D4 — htmx swap strategies

![Three swap cadences: full-page swap (link navigation, hx-boost, server returns full page); panel swap on-demand (user action via hx-get, server returns HTML fragment); live swap push-driven (hx-ext='sse' with sse-connect + sse-swap, server pushes fragments over SSE, with hx-trigger='every Ns' as polling fallback and hx-ext='ws' deferred). Convention: every live-swap target has a stable id matching the server's SSE event name.](diagrams/SRD-0110/swap-cadences.png)




Three swap cadences, one per kind of data:

**Full-page swaps.** Navigation across pages. Default htmx
boost for internal links; the server returns a fresh full
page.

**Panel swaps (on demand).** A user action triggers a panel
refresh: clicking "refresh", changing a filter, expanding a
detail row. htmx `hx-get` to a nested BFF endpoint returning
the panel's HTML fragment.

**Live swaps (push-driven).** Panels that reflect changing
backend state subscribe to SSE (D6). The fragment target has
`hx-ext="sse"` + `sse-connect="/events/<scope>"` +
`sse-swap="<event-name>"`. Each incoming SSE event is an HTML
fragment that htmx swaps into place.

**Polling fallback.** Panels where SSE is overkill (slow-
changing, infrequent) use `hx-trigger="every 30s"` instead.
Trivial; no transport upgrade.

**Convention: targets are named.** Every live-swap target has a
stable `id` that matches the event name the server emits. A
panel receiving node-status events declares
`id="node-{id}-status"` and the server's SSE event name is
`node-status`. Unambiguous cross-reference in both directions.

## D5 — Session + browser auth

Browsers authenticate to the web server via signed session
cookies. The web server authenticates to the controller via
the system API key + impersonation header (D8). The browser
never sees the system API key.

**Login flow.**

1. Browser hits a protected URL → `302` to `/login`.
2. `/login` shows username/password form. On submit:
   a. Web server `POST`s credentials to
      `/api/v1/auth/login` on the controller (with the
      system API key authenticating the proxy request).
   b. Controller validates the user's password, returns a
      `user_id` + basic user metadata + default roles.
   c. Web server mints a session cookie containing the
      `user_id`, active roles, and a CSRF token. Signed
      with `session_secret`. `HttpOnly`, `Secure`,
      `SameSite=Lax`.
3. Browser gets the cookie + `302` to the originally
   requested URL.

**Cookie contents (signed, not encrypted).**

```
{
  "user_id": "usr-01HZX...",
  "active_roles": ["user"],
  "csrf": "...",
  "iat": 1737728400,
  "exp": 1737814800
}
```

24-hour absolute expiry; cookie is rotated on each response
(sliding idle window). Users toggle active roles via a
settings panel; SRD-0114 D4 owns the rules (unguarded toggling,
additive + non-hierarchical).

**Logout.** Clear the cookie. No server-side session store —
the cookie is the session. Revocation before expiry requires
rotating `session_secret` (heavy) or a short cookie expiry +
frequent renewal (light; the chosen model).

**CSRF.** All state-changing requests (POST/PUT/DELETE) require
the CSRF token from the cookie in an `X-Csrf` header or form
field. htmx attaches it automatically via configured `htmx:
configRequest` handler.

**Session-store location is the web server's cookie, not the
controller.** The controller is stateless with respect to
browser sessions (per SRD-0114 open question 5 — resolved
here). When the web server proxies a browser request it
constructs the controller credential fresh each time (system
API key + impersonation header).

## D6 — SSE transport for live updates

Per the design ruling, SSE is the real-time push transport.
htmx has first-class SSE support via `hx-ext="sse"`.

**Endpoints.** SSE endpoints live under `/events/*` on the web
server and correspond to browser-view scopes, not controller
event types:

| Endpoint | Scope |
|---|---|
| `/events/dashboard` | Aggregate for the dashboard page |
| `/events/topology` | Topology graph updates |
| `/events/nodes/{id}` | Node-specific updates |
| `/events/executions/{id}` | Execution-specific updates |
| `/events/audit` | Admin-only audit log tail |

Each endpoint is a long-lived SSE stream; the web server
subscribes to the controller's event stream (D7) and emits
HTML fragments per event for htmx to swap.

**Event framing.** One SSE event per htmx swap target:

```
event: node-status
data: <span id="node-node-abc-status" class="healthy">active</span>

```

The `event:` field matches the htmx `sse-swap` value; the
`data:` field is the HTML fragment.

**Connection lifecycle.** Browser opens the SSE connection
when the panel mounts; closes when the panel is removed or
the page navigates away. The web server fans out from a
single upstream subscription to all browsers viewing the
same scope (D7).

**WebSocket deferred.** htmx supports `hx-ext="ws"` for the
rare bidirectional case. Not used in v1 — SSE plus per-action
`hx-post` covers every interaction pattern we have. SSE and
long-polling both ride vanilla HTTP, so proxies +
load-balancers don't need WS-upgrade handling.

## D7 — Event-consumer architecture

The web server is the event consumer. It subscribes to the
controller's event stream server-side (one WebSocket to the
controller per web-server process) and exposes a browser-suitable
SSE projection (D6).

**Upstream.** At startup, the web server opens a WebSocket to
`/api/v1/events/subscribe` (SRD-0108 D6) with
`since=<now>`. Every controller event arrives on this one
connection.

**Fanout.** The web server maintains a publish/subscribe bus
internally: each browser-facing SSE connection is a subscriber
keyed on its scope (D6 endpoints). When a controller event
arrives, the web server's fanout:

1. Classifies the event against the subscribed scopes (e.g.
   a `NODE_STATUS_CHANGED` for `node-abc` matches the
   `dashboard`, `topology`, and `nodes/node-abc` scopes).
2. Renders the event into an HTML fragment per scope (the
   fragment may be different per scope — the dashboard sees
   a row update; the node-detail page sees an inline status
   change).
3. Pushes the fragment as an SSE event to every subscriber
   of that scope.

**Permissions.** Each event is visibility-checked against the
subscriber's effective principal before being pushed, per
SRD-0114 D10. The web server remembers each SSE subscriber's
`user_id` + active roles (from the session cookie) and
filters accordingly. Events a user cannot see are dropped
silently.

**Buffer policy.** The web server holds a rolling in-memory
buffer of the last N events per scope (default N=200) so
late-joining subscribers can paint a warm initial state
without waiting for the next event. This is a fanout
efficiency mechanism, not a replay source of truth — per
SRD-0110's previous ruling, if a subscriber needs replay
from a specific sequence / timestamp, the request forwards
to the controller's event stream.

**INV-WEB-EVENT-FANOUT-UPSTREAM-ONE.** The web server
maintains at most one upstream subscription to the controller
per scope-family. Per-browser proxying is forbidden — that'd
multiply subscriptions by the browser count, defeating the
fanout design.

## D8 — System API key + impersonation

Every controller call from the web server carries:

```
Authorization: Bearer <system-api-key>
X-On-Behalf-Of: <user-id-from-session>
```

The system API key authenticates the proxy; the impersonation
header names the effective principal (per SRD-0108 D3 + SRD-0114
D11). The controller's permission checks fire against the
impersonated user — not against the system account.

**Boot-time auth probe.** On startup the web server does a
smoke-test `GET /api/v1/health` with the system API key; a
401 exits the process with a clear error. Misconfigured keys
fail fast rather than serving broken pages.

**Scope.** The system API key has every scope except
`admin:tokens:mint-admin` (creating new admin tokens). The
web server cannot escalate an end-user's privileges by virtue
of being the BFF — only a user with the right roles + token
scopes can take a given admin action, and the web server
just plumbs the impersonation through.

## D9 — Admin surfaces

Per SRD-0114, admin actions (user CRUD, role assignment,
workspace management, audit log view) are gated on active
`admin` role + appropriate token scope. The web server
exposes `/admin/*` pages only to users whose active roles
include `admin`; non-admin users don't see admin nav items
and direct `/admin/*` requests return 403.

**Audit-log view.** `/admin/audit` shows a live tail of the
audit journal (SRD-0114 D12). Powered by the `/events/audit`
SSE endpoint (D6).

**Impersonation UX.** Admins can open a "view as" dialogue
to see the site through a specific end-user's eyes. This
flips the impersonation header for subsequent requests and
logs the action in the audit journal. No "act as" — only
"view as"; state changes still flow against the admin's own
principal.

## D10 — Asset pipeline

Single default stylesheet built around CSS custom properties
(design tokens at `:root`, referenced throughout). Naming
follows a stable convention (BEM) so third-party overrides
don't need to study internal structure to target elements.
Override surface: a second stylesheet redefining tokens,
loaded after the default.

**Layout.**

```
crates/hyperplane-web/assets/
├── css/
│   ├── tokens.css       # :root custom properties
│   ├── base.css         # element + BEM block styles
│   └── components.css   # reusable panel styles
├── js/
│   └── htmx.min.js      # vendored htmx runtime + SSE ext
└── fonts/               # optional
```

**Build.** Assets are embedded at compile time via
`rust-embed`; the binary has no filesystem dependency in
production. Dev mode reads from disk for live-reload.

**No bundler.** No webpack, no esbuild, no sass. Plain CSS
and one vendored htmx JS file. Any preprocessing that
becomes necessary (e.g. PostCSS for autoprefixer) runs as a
`cargo` build script, not as a separate node-based pipeline.

**Third-party theming.** Operators drop a `tokens.override.css`
in an asset-override directory; the web server serves it
after the default tokens. Zero code change required to
re-theme a deployment.

## D11 — Template engine

**maud** as the default recommendation. Compile-time HTML
safety; fragments are functions (a partial return for
`hx-swap` is `fn foo(x: &X) -> Markup`); no runtime template
parsing. Works naturally with htmx's fragment-oriented
rendering.

Fragments compose the same way functions compose — a page
template is just a function that calls panel functions
that call widget functions. Same dispatch the runtime uses
for full-page renders works for partial swaps.

**Why not file-based templates.** The Rust-port bar for
iteration speed is type-checked-at-compile-time. A file-based
Jinja-like engine loses that guarantee and buys us… not much
in a codebase where the templates are already colocated with
their handlers. If a future page grows complex enough to
warrant a separate template file, we can introduce `askama`
(file-based, Jinja-like, still compile-time) without
rewriting the maud pages — coexist.

## D12 — Accessibility baseline

- Semantic HTML first: use the right element, not a `<div>`
  with ARIA props.
- Keyboard navigation: every action reachable via keyboard;
  focus order matches visual order.
- Live-region ARIA on SSE-driven panels so screen readers
  announce state changes.
- Contrast ratios meeting WCAG AA in the default token set;
  themable via `tokens.override.css`.
- Form fields have associated labels; error messages are
  programmatically linked to fields.

Validated via a CI pass running `axe-core` (or equivalent) on
each page's rendered HTML. Baseline fails CI.

## D13 — New invariants

| Code | Invariant |
|---|---|
| `INV-WEB-DEP-GRAPH` | `hyperplane-web` crate does not depend on any database crate. |
| `INV-WEB-COMPOSITION-SCOPE` | Every BFF endpoint composes from per-request controller data; no cross-request domain state. |
| `INV-WEB-EVENT-FANOUT-UPSTREAM-ONE` | At most one upstream subscription to the controller per scope-family. |

These extend the SRD-0100 catalogue.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/hyperplane-webconsole/requirements-webconsole.md`
  — Java web-console requirements ported selectively (no SPA,
  no server-side state).
- `~/projects/hyperplane/docs/ARCHITECTURE.md` § 4, 4a, 4b.
- htmx docs, particularly `sse` and `hx-boost` extensions.
- `maud` crate docs.
