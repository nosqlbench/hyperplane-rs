<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0109 — CLI & Dynamic Autocompletion

## Purpose

Specify the `hyper` CLI as a first-class client of the
controller API (SRD-0108), with dynamic autocompletion routed
through the single binary itself — no per-shell completion
scripts, no out-of-tree generators. The shell calls back into
`hyper __complete ...` and the binary answers based on live
state (current plans, running executions, known element
names, registered images).

Dynamic completion is a first-class v1 requirement, not an
afterthought. Static completions (generated-at-install) are
insufficient for a system whose interesting completion
sources — plan IDs, execution IDs, node IDs, image specs,
agent IDs — are live and frequently-changing.

The reading of SRD-0100 D8 (`INV-PARITY`): every action in
the CLI is also in the web UI and vice versa. The CLI is a
client of the controller API; so is the web server. Parity
falls out of both being thin clients of the same contract.

## Scope

**In scope.**

- Binary layout, install path, XDG-canonical config/cache
  directories.
- Command tree — top-level groups mirroring the API
  endpoint taxonomy (SRD-0108 D4).
- Auth flow — `hyper login`, token storage per XDG paths,
  re-auth on expiry.
- Dynamic completion protocol — `veks-completion` as the
  completion framework, the shell-to-binary wire contract
  `__complete` implements.
- Completion data sources — which CLI completions hit the
  controller live vs. reuse a local observation.
- Local observation caches — structure, refresh rules,
  invariant compliance.
- Offline / degraded-mode behaviour — what completes when the
  controller is unreachable.
- Shell integration shims — bash, zsh, fish, pwsh — all
  delegating to `hyper __complete`.

**Out of scope.**

- API endpoint definitions (SRD-0108).
- Which user-facing actions exist (SRD-0108 owns the catalogue;
  CLI mirrors).
- Server-side impersonation / RBAC (SRD-0114).

## Depends on

- SRD-0100 (parity invariant, naming conventions, state-cache
  rules).
- SRD-0108 (canonical API; CLI is a client).
- SRD-0114 (principals — the CLI authenticates as an
  end-user).
- `veks-completion` crate (we own; the dynamic-completion
  engine).

---

## Completion architecture at a glance

```
  User types: hyper execution show <TAB>
       │
       ▼
  ┌──────────────┐
  │    shell     │   bash / zsh / fish / pwsh
  └──────┬───────┘
         │ shell completion hook (per-shell shim, installed once)
         ▼
  ┌──────────────┐
  │    hyper     │   same binary, internal subcommand
  │  __complete  │
  └──────┬───────┘
         │ veks-completion: parse the partial command line
         │                 → identify the completion point
         │                 → which data source?
         ▼
  ┌────────────────────────────────────────────────┐
  │   Completion source dispatch                   │
  │                                                │
  │   static ──▶ compiled-in list                  │
  │   file ────▶ filesystem                        │
  │   live ────▶ GET controller API                │
  │              │                                 │
  │              ├── success ──▶ refresh cache     │
  │              │               return live       │
  │              │                                 │
  │              └── fail ─────▶ fall back to cache│
  │                              (stale if older)  │
  └──────┬─────────────────────────────────────────┘
         │
         ▼
  completions back to shell → rendered to user
```

Observation cache at
`$XDG_CACHE_HOME/hyperplane/{server}/completion.json`. Exempt
from `INV-STATE-CACHE` because completion data is
observation, not state (per SRD-0100 D6 refinement).

## Configuration file layout at a glance

```
  $XDG_CONFIG_HOME/hyperplane/
  ├── credentials                  # bearer token (mode 0600)
  └── hyper.toml                   # server URL + profile defs

  $XDG_CACHE_HOME/hyperplane/
  ├── {server-host}/
  │   └── completion.json          # observation cache
  └── tmp/
```

## D1 — Binary layout + install

The CLI is a single Rust binary, `hyper`, built from the
`hyperplane-cli` crate. Statically-linked musl builds for
Linux; native builds for macOS and Windows.

**Install path.** Standard location on the invoking user's
`$PATH` (`/usr/local/bin/hyper`, `~/.cargo/bin/hyper`,
`$HOME/.local/bin/hyper`, etc.). The CLI itself doesn't
mandate a specific location — the shell-integration shim
(D7) auto-detects where it was invoked from.

**Dependencies.** `reqwest` for HTTP, `tokio` for async,
`veks-completion` for completion, `clap` for argument
parsing (limited to parsing; completion does not ride clap
per D4), `serde` + `serde_json` for wire format,
`rust-embed` or equivalent for the shell shims.

**No DB driver.** Per `INV-NON-CTL-NO-PERSISTENCE`
(SRD-0100), the CLI holds no persistent domain state — only
credentials (D3) and observation caches (D5). Compile-time
enforcement via the Cargo graph: `hyperplane-cli` does not
depend on `paramodel-store-sqlite`.

## D2 — Command tree

Top-level command groups mirror the controller API endpoint
taxonomy (SRD-0108 D4). Each group's subcommands map 1:1 to
endpoints or operations within that group.

```
hyper
├── login                    # Auth: mint a bearer token
├── logout
├── whoami
│
├── study
│   ├── list
│   ├── show <id>
│   ├── create [--from <path>]
│   ├── delete <id>
│   └── ...
│
├── plan
│   ├── list [--study <id>]
│   ├── show <id>
│   ├── validate <path>
│   ├── submit <path>
│   └── ...
│
├── execution
│   ├── list [--plan <id>]
│   ├── show <id>
│   ├── start <plan-id>
│   ├── cancel <id>
│   ├── pause <id>
│   ├── resume <id>
│   ├── logs <id>            # live log tail
│   └── events <id>          # live event tail
│
├── node
│   ├── list
│   ├── show <id>
│   ├── provision [--from <profile>]
│   ├── terminate <id>
│   ├── logs <id>
│   └── ...
│
├── agent
│   ├── list
│   ├── show <id>
│   ├── broadcast <message>  # admin
│   └── ...
│
├── image
│   ├── list
│   ├── paramspace <spec>
│   ├── validate-params <spec> [--file <json>]
│   └── register <spec>
│
├── events
│   ├── tail [--filter ...]  # live
│   └── query [--since ...]  # historical
│
├── system
│   ├── start                # operates the local service stack (SRD-0112)
│   ├── stop
│   └── status
│
└── __complete ...           # internal; shell shims call this
```

**Parity restated (`INV-PARITY`, SRD-0100 D8).** Every action
in this tree corresponds to an endpoint (or composed
endpoint-set) in SRD-0108. Adding an action here requires
adding the matching API endpoint first — the CLI is a
client, not a feature surface of its own.

**System group exception.** `hyper system start|stop|status`
target the local service stack (SRD-0112) — starting the
controller process, the web server, etc. on the user's
machine. These are local-supervisor operations, not
controller-API calls; they're explicitly exempt from
`INV-PARITY` because they have no meaning in the web UI (which
runs inside the stack being supervised).

## D3 — Auth flow

```
  hyper login:

  user                CLI                       controller
   │                   │                             │
   │─ hyper login ───▶ │                             │
   │                   │── prompt username+password  │
   │◀── prompt ────────│                             │
   │── credentials ──▶ │                             │
   │                   │── POST /api/v1/auth/login ─▶│
   │                   │                             │── validate
   │                   │                             │── mint bearer token
   │                   │◀── token + expiry + scopes ─│
   │                   │                             │
   │                   │── write credentials file    │
   │                   │   (mode 0600 in XDG config) │
   │                   │                             │
   │◀── "logged in" ───│                             │

  subsequent calls:
  CLI reads credentials file → attaches Authorization: Bearer <token>
  401 TokenExpired → if TTY, prompt re-login inline; if not, exit 2
```



**`hyper login`.** Interactive first-time auth.

1. User runs `hyper login [--server <url>]`. Server URL
   optional; defaults to a previously-configured value or
   prompts.
2. CLI prompts for username + password.
3. CLI POSTs `/api/v1/auth/login` (SRD-0108) with credentials.
4. Controller validates + returns a bearer token + token
   metadata (expiry, scopes).
5. CLI writes the token to
   `$XDG_CONFIG_HOME/hyperplane/credentials` (default
   `$HOME/.config/hyperplane/credentials`), file mode 0600,
   in TOML:
   ```toml
   [server]
   url = "https://controller.example.com"

   [token]
   value = "..."
   expires_at = "2026-04-25T14:00:00Z"
   scopes = ["study:read", "plan:read", "plan:write", ...]
   ```
6. Subsequent CLI invocations read this file and attach
   `Authorization: Bearer <token>` to every controller call.

**Token expiry.** When a request returns `401 TokenExpired`,
the CLI:

- Prompts for re-auth interactively if stdin is a TTY
  (`hyper login` flow inline).
- Exits non-zero with a clear message if non-interactive
  (scripts must re-login via `hyper login --password-stdin`
  or equivalent).

**Logout.** `hyper logout` deletes the credentials file and
calls the controller's token-revoke endpoint (best-effort —
if the controller is unreachable, the local file is still
removed).

**Multiple servers / multiple identities.** Credentials file
can hold one default plus named profiles:

```toml
[default]
server = "prod"

[servers.prod]
url = "https://prod.example.com"
token = "..."
expires_at = "..."

[servers.dev]
url = "https://dev.example.com"
token = "..."
expires_at = "..."
```

`hyper --server dev <cmd>` selects the profile; absence
uses `default`.

**File locations.**

- Config + credentials: `$XDG_CONFIG_HOME/hyperplane/`
  (default `$HOME/.config/hyperplane/`).
- Regenerable caches: `$XDG_CACHE_HOME/hyperplane/`
  (default `$HOME/.cache/hyperplane/`).
- Both respect `$XDG_CONFIG_HOME` / `$XDG_CACHE_HOME`
  overrides per XDG spec.

No keyring integration. Standard 0600 file-mode is the
protection story. Rationale: keyring integrations balloon
platform-specific complexity (libsecret, macOS Keychain,
Windows Credential Manager) for marginal security gain over
a mode-protected file on a user's own machine.

**`INV-CREDENTIALS-NOT-STATE`** (SRD-0100 D7). The token file
is a credential, not application state — the state-cache
rules of D6 don't apply to it.

## D4 — Dynamic completion: `veks-completion`

Resolved ruling: completion rides `veks-completion`, not
clap. We own `veks-completion`; we can update it to track
hyperplane's needs.

**Why not clap's completion.** clap's completion story is
static-generated: at install time, clap emits a shell script
with every possible subcommand and flag hard-coded. That
shell script has no live-state awareness — it can complete
`hyper execution show <TAB>` to the word `execution` but not
to the list of actual execution IDs on the controller. The
dynamic completion requirement (live IDs, live element
names, live image refs) is exactly the thing clap's story
doesn't cover.

**What `veks-completion` gives.** The framework for:

- A `Complete` trait each command implements, describing
  what completions its positional args and flags accept
  (static strings, file paths, or "call into this function
  at completion time").
- A wire format between shell shims and the binary's
  `__complete` subcommand.
- Shell-integration shims for bash, zsh, fish, pwsh — all
  delegating back to `hyper __complete`.

**Parsing vs completion split.** `clap` may be used for
argument *parsing* (the normal runtime flow; clap is good
at this). When the command is being *completed* rather than
*executed*, control routes through `veks-completion`'s
`__complete` subcommand instead. The two frameworks are
disjoint — the same command definition annotates both via
macros in `hyperplane-cli`, and a build-time check ensures
every command's parse definition and completion definition
agree on the argument shape.

## D5 — Completion data sources + observation cache

Each completion point declares its source:

| Completion target | Source | Live? |
|---|---|---|
| Subcommand names | Static (compiled in) | — |
| Flag names | Static | — |
| Flag enum values | Static | — |
| File paths | Filesystem | — |
| Plan IDs | Controller `GET /api/v1/plans` | live |
| Execution IDs | Controller `GET /api/v1/executions` | live |
| Node IDs | Controller `GET /api/v1/nodes` | live |
| Agent IDs | Controller `GET /api/v1/agents` | live |
| Image specs | Controller `GET /api/v1/images` | live |
| Study names | Controller `GET /api/v1/studies` | live |
| Event types | Controller `GET /api/v1/system/event-types` | live, rare |
| Element kinds | Controller `GET /api/v1/system/element-kinds` | live, rare |

**No latency budget.** Completion calls the controller live.
If it takes 300 ms, the user sees the completion after 300
ms. This is the resolved ruling: "if it is worth being
dynamic, it is worth waiting for."

**Observation cache.** Completion data held across CLI
invocations is an *observation* under SRD-0100 D6 — it can
be rebuilt from the controller on demand. The CLI may hold
such caches without violating `INV-STATE-CACHE`. Any *write*
operation still routes to the controller; completion data is
read-side observation only.

**Cache structure.** A per-server SQLite-free key-value file
at `$XDG_CACHE_HOME/hyperplane/{server}/completion.json`:

```json
{
  "plans":       { "fetched_at": "...", "items": [...] },
  "executions":  { "fetched_at": "...", "items": [...] },
  ...
}
```

**Refresh policy.** Pragmatic: on each completion call, the
CLI tries the live controller. If the live call returns,
refresh the cache and answer with live data. If the live call
fails (timeout, network, 5xx), fall back to the cached
value and mark the completion result with a
"(stale)" suffix in shell-visible form. If neither is
available, return no completions (completion failures are
soft; the user types the value instead).

**Freshness hint.** When live data succeeds, the cache is
replaced; the previous entry's `fetched_at` informs the
operator about staleness when the fallback path triggers.

**No TTL.** No "after N seconds the cache is stale" rule —
the live call either succeeds (cache is refreshed + used) or
fails (cached fallback is used regardless of age). Matches
"no latency budget" resolution: the system does not try to
predict staleness, it simply tries live and falls back on
actual failure.

## D6 — Offline / degraded-mode behaviour

When the controller is unreachable and the CLI is invoked:

- **Static completions** (subcommand names, flag names,
  enum values, file paths) — always work.
- **Live completions** — fall back to the cache (D5). If the
  cache is empty or older than the session of interest, the
  completion returns empty and the user types the value.
- **Actual commands** that need the controller return a
  clear error: `controller unreachable: <url>`, exit code
  2 (distinct from 1 for "operation failed for a valid
  reason").
- **Auth flows** (`login`, `logout`) need the controller;
  they fail with the same unreachable error.

**No queue-for-later semantics.** Commands aren't buffered
locally to retry when the controller comes back — per
`INV-NON-CTL-NO-PERSISTENCE` that'd be persisted domain
state at the wrong tier. The user retries.

## D7 — Shell integration shims

One install command drops a per-shell shim that delegates
to `hyper __complete`. The shim is identical in shape across
shells, differing only in the shell's completion-registration
syntax.

**Install.** `hyper completion install [--shell <shell>]`.
Detects the current shell if unspecified. Writes:

| Shell | File |
|---|---|
| bash | `~/.bash_completion.d/hyper` (or sources from `~/.bashrc`) |
| zsh | `~/.zsh/completions/_hyper` |
| fish | `~/.config/fish/completions/hyper.fish` |
| pwsh | `$PROFILE`-sourced script fragment |

Each shim:

1. Registers a completion function with the shell.
2. On `<TAB>`, the function invokes `hyper __complete "$args"`
   (exact protocol owned by `veks-completion`).
3. Reads the binary's stdout and feeds it back to the shell
   as completion candidates.

**Uninstall.** `hyper completion uninstall [--shell <shell>]`.
Removes the file, leaves the shell's RC untouched (user
sourced the file once; they can remove the source line
themselves).

**Reinstall on CLI upgrade.** Not required. The shim calls
the `hyper` binary at completion-time; binary changes pick
up automatically. The only time a shim upgrade is needed is
if `veks-completion`'s wire protocol itself changes — a
breaking-compatibility event that requires coordinated
release notes and an upgrade reminder.

## D8 — Internal `__complete` subcommand

`hyper __complete <shell-protocol-args>` is the shell-
invoked entry point. Its contract (owned by `veks-completion`):

- **Input** on argv: the current command line being completed,
  plus cursor position.
- **Output** on stdout: one completion candidate per line,
  UTF-8 encoded. Candidates may carry display hints (e.g.
  rich display text, suffixes for disambiguation) via a
  simple tab-separated format.
- **Exit code.** `0` for completions returned (even if
  empty). Non-zero for completion framework errors
  (unrecognized shell protocol, etc.).

The subcommand is hidden from the normal help output; it's
internal plumbing, not a user surface.

## D9 — Output formatting

User-facing output follows three conventions:

- **Default output** — human-readable tables for `list` /
  `show` commands; plain text for `logs` / `events` tails;
  exit codes communicate success/failure.
- **`--json`** — machine-readable JSON. Every command supports
  this; scripts use it for automation.
- **`--quiet`** — minimal output; useful for composable
  shell pipelines.

Progress bars for long operations (`plan submit`, `execution
start`, `image register`) go to stderr; scripts redirect to
keep stdout clean.

## D10 — Error reporting

The CLI's error model is aligned with SRD-0108 D8:

- Controller errors surface with their `code`, `message`,
  and `details`. Exit code derived from HTTP status
  (`4xx` → 1, `5xx` → 2, transport → 2).
- Local errors (bad args, missing creds, unreachable
  controller) use dedicated local codes: `CredentialsMissing`,
  `ServerUnreachable`, `InvalidArgs`.
- `--debug` adds a `request_id` footer on error (SRD-0108
  D8); support tickets include it.

## D11 — Testing

Command-tree parity with the API catalogue is a
TCK-asserted property: for each endpoint in SRD-0108's
OpenAPI (D10), there must be a CLI subcommand that exercises
it. Missing subcommands fail CI. This closes the
`INV-PARITY` loop mechanically.

Completion tests shell-exec the shims against a controller
fixture; the shim must return the expected completion set.

## D12 — New invariants

| Code | Invariant |
|---|---|
| `INV-CLI-OBSERVATION-ONLY` | CLI-side caches are observation data (SRD-0100 D6); no domain-state writes happen locally. |
| `INV-CLI-COMPLETION-LIVE` | Completion calls the live controller first; cache is fallback only. No TTL-based stale-hiding. |

Extends SRD-0100.

## Open questions

None remaining.

## Reference material

- `veks-completion` crate — internally-owned completion
  engine; this SRD relies on it.
- `~/projects/hyperplane/hyperplane-cli/` — Java-era CLI
  reference (command tree + conventions ported; Java-specific
  static completion story dropped).
- XDG Base Directory spec —
  `https://specifications.freedesktop.org/basedir-spec/`.
