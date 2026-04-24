<!--
 Copyright (c) Jonathan Shook
 SPDX-License-Identifier: Apache-2.0
-->

# SRD-0103 — Parameter Extraction From Container Images

## Purpose

Define how hyperplane turns a Dockerfile (or OCI image manifest)
into a paramodel `ParamSpace` — the extraction that lets users
point the system at an image and get back a typed, validated
configuration schema they can author a TestPlan against.

The extraction is a pure function of the image: given a specific
image digest, the `ParamSpace` it produces is deterministic.
That determinism is what makes the per-digest cache safe (D6)
and what lets the validation endpoint (D5) work offline against
a registered image.

## Scope

**In scope.**

- `@param` Dockerfile annotation grammar.
- ENV-default fallback rules (parameter with no explicit domain
  → inferred from ENV value kind).
- Extraction algorithm — image source → Dockerfile text →
  parsed annotations → typed `ParamSpace`.
- Validation algorithm — resolved-value computation (applying
  defaults to a partial binding) + domain + constraint checks.
- Controller API endpoints for extraction + validation
  (`POST /api/v1/images/{spec}/paramspace`,
  `POST /api/v1/images/{spec}/validate-params`).
- Caching — extracted `ParamSpace` is keyed by image digest;
  tag → digest resolution happens at lookup time.
- How extracted `@param`s relate to Docker labels (D2).

**Out of scope.**

- Image build pipeline (SRD-0107 covers command containers;
  SRD-0106 covers service containers).
- The registry policy itself (SRD-0106).
- OCI manifest parsing internals — treat as a library dep.
- Authentication against private registries (operator concern,
  covered by config in SRD-0112).

## Depends on

- SRD-0101 (state boundaries — cache lives in controller).
- SRD-0102 (element type registry — Service/Command kinds
  consume extracted `ParamSpace`).
- Paramodel SRD-0004 (Parameters & Domains).

---

## Extraction pipeline at a glance

![Image param-extraction and validation pipelines: the left pipeline resolves an image reference to a digest (via registry HEAD if a tag), hits the images table cache keyed by digest, and on miss pulls the manifest, parses @param annotations, infers ENV defaults, builds a ParamSpace, and caches; the right pipeline takes bindings and an image ref, applies defaults, and runs domain/pattern/valueSource checks to return a resolved binding.](diagrams/SRD-0103/extraction-pipeline.png)

**Left path**: `POST /api/v1/images/{spec}/paramspace` — pure
function of the digest; cache safe because deterministic.

**Right path**: `POST /api/v1/images/{spec}/validate-params` —
resolves defaults, checks against the cached `ParamSpace`.

## D1 — Grammar site: Dockerfile comments, not labels

Parameter annotations ride in Dockerfile comments, not in
Docker labels. A comment-annotation grammar sits alongside the
top-level Dockerfile concerns (ENV, ARG, CMD, EXPOSE) that
parameters accessorize, without colonising the Docker-owned
label namespace.

**Why not labels.**

- Parameter-level concerns (one declaration per parameter, with
  domain metadata) would drastically pollute the image's label
  namespace. A non-trivial container easily has 10+ parameters;
  10+ labels is a lot of metadata on an image intended to
  describe "what kind of container this is."
- Labels are runtime metadata about the image. Parameters are
  authoring-time metadata about how the image is used. Different
  concerns, different surfaces.

**What stays in labels.** Two fixed runtime-metadata concerns
only (from SRD-0102 / SRD-0106 / SRD-0107):

- `hyperplane_mode=service|command` — which element kind
  this image binds to.
- OCI standard labels (`org.opencontainers.image.*`) — vendor
  metadata.

Everything parameter-related — name, description, type, domain,
default source — lives in Dockerfile comments.

## D2 — `@param` grammar

Starting grammar, ported from the Java implementation
(`~/projects/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`):

```
# @param NAME - Description [attr=value, attr=value, ...]
ENV NAME=default_value
```

**Rules.**

- The `@param` name must match the `ENV` variable name exactly
  (including case). Mismatch is a parse error.
- The description is required — human-readable; used by CLI help
  and UI forms.
- Attributes in `[...]` are optional; the default domain comes
  from the `ENV` value when no attributes are given (D4).

**Attribute set.**

| Attribute | Values | Meaning |
|---|---|---|
| `type` | `string` \| `integer` \| `double` \| `boolean` \| `select` \| `path` | Typed domain. Default `string`. |
| `options` | comma-separated list | For `type=select`: enumerated values. |
| `min` | number | For `integer`/`double`: lower bound (inclusive). |
| `max` | number | For `integer`/`double`: upper bound (inclusive). |
| `pattern` | regex | For `type=string`: valid regex for value; maps to paramodel's `StringParameter::regex`. |
| `required` | `true` \| `false` | Must be provided at runtime. Default `false`. |
| `fixed` | `true` \| `false` | Informational; not editable by the user. Default `false`. |
| `valueSource` | identifier | External API providing valid values; controller proxies to `/api/v1/{valueSource}`. |

**Rust-port additions over the Java grammar** (each with a
concrete paramodel capability behind it):

- `pattern=<regex>` on `type=string`. The Java grammar had no
  regex attribute; the Rust port maps directly to paramodel's
  `StringParameter::regex`.
- Splitting `type=integer` from `type=double`. Java had a single
  numeric type; paramodel distinguishes `IntegerParameter` from
  `DoubleParameter` in its algebra, and the grammar reflects
  that distinction.

**Deferred.** Derived-parameter expressions — paramodel has the
abstraction, but inlining expression syntax in Dockerfile
comments is a tar pit. Revisit when a concrete adopter asks for
it.

**Example.**

```dockerfile
# @param LOG_LEVEL - Logging verbosity [type=select, options=DEBUG,INFO,WARN,ERROR]
ENV LOG_LEVEL=INFO

# @param PORT - HTTP listen port [type=integer, min=1, max=65535]
ENV PORT=8080

# @param DATASET - Dataset name [valueSource=datasets, required=true]
ENV DATASET=""
```

Extracted `ParamSpace` (schematically):

```
  ParamSpace {
    LOG_LEVEL  → SelectParameter  { options: [DEBUG,INFO,WARN,ERROR],
                                    default: "INFO" }
    PORT       → IntegerParameter { min: 1, max: 65535,
                                    default: 8080 }
    DATASET    → StringParameter  { value_source: "datasets",
                                    required: true,
                                    default: "" }
  }
```

## D3 — Precedence of sources

An image's `ParamSpace` is derived from two sources in priority
order. Conflicts resolve in favour of higher-priority.

1. **Explicit `@param` annotations** — the authoring-time
   declaration. Wins over inferred.
2. **ENV-default inference** — for ENV variables with no
   matching `@param`, hyperplane synthesises a minimal
   parameter descriptor (D4). Treated as `type=string`,
   optional, default from the ENV value.

**Why infer at all.** Images imported from other ecosystems
often use ENV for configuration without hyperplane-aware
annotations. Inference gives them a baseline "you can override
this at runtime" surface without requiring the author to rewrite
the Dockerfile. Authors who want richer domains add `@param`
annotations; the inferred baseline disappears for any ENV that
has an explicit annotation.

**No inference from ARG.** Docker `ARG` is build-time; `ENV` is
run-time. Hyperplane runs the container, not builds it — `ARG`
values are baked into the image and not runtime-configurable.
We infer only from `ENV`.

## D4 — ENV-default inference rules

For each `ENV NAME=value` statement with no matching `@param`:

| Value pattern | Inferred type | Notes |
|---|---|---|
| Matches `/^-?\d+$/` | `integer` | No bounds inferred. |
| Matches `/^-?\d*\.\d+$/` | `double` | No bounds inferred. |
| Exactly `"true"` or `"false"` | `boolean` | |
| Anything else | `string` | No pattern inferred. |

**Inferred-parameter properties.** `required=false`, `fixed=false`,
default = the ENV value as-parsed. Description is synthesised as
`"inferred from ENV"` — authors who want better descriptions
should add `@param`.

**Multi-line ENV handling.** `ENV FOO=bar BAZ=qux` (one ENV,
two vars) is treated as two separate inferences. `ENV FOO \
bar` (line continuation) is normalised before inference.

## D5 — Extraction algorithm

**Input.** An image spec — either a tag (`my-harness:latest`) or
a digest reference (`my-harness@sha256:abc...`).

**Steps.**

1. **Resolve.** If the input is a tag, HEAD the registry for its
   digest. Cache the `(registry, repository, tag) → digest`
   result in `image_tag_cache` (SRD-0101) with a short TTL
   (default 5 minutes).
2. **Cache hit.** If the digest is present in `images` (SRD-0101),
   return its cached `ParamSpace`. Done.
3. **Cache miss.** Pull the image's manifest + config via the
   OCI registry API (no full image pull required — the
   Dockerfile text is reconstructable from the config's
   `history` for images built by a Dockerfile-aware builder;
   otherwise extract from a labeled Dockerfile stored as an
   image annotation).
4. **Parse.** Lex Dockerfile comments; identify `@param`
   annotations; validate grammar (D2); check name/ENV match.
5. **Infer.** For each ENV without a matching `@param`,
   synthesise an inferred descriptor (D4).
6. **Build `ParamSpace`.** Construct paramodel's `ParamSpace`
   from the merged descriptors. Parameter names, types,
   constraints, defaults.
7. **Store.** Insert into `images` keyed by digest. Return the
   `ParamSpace`.

**Determinism.** Steps 4–6 are a pure function of the
Dockerfile text. Step 7's cache is safe because of that.

**Errors.**

- Grammar parse failure → `400 InvalidParamAnnotation` with
  the offending line cited.
- `@param` without matching `ENV` → `400 ParamMissingEnv`.
- `ENV` named in `@param` but of different type than declared
  (e.g. `@param FOO [type=integer]` but `ENV FOO=abc`) →
  `400 ParamTypeMismatch`.
- Missing Dockerfile source (not a Dockerfile-built image, no
  embedded source) → `422 NoExtractableSource`; the image must
  embed its source under a known annotation
  (`hyperplane.source.dockerfile`) or be rebuilt.

## D6 — Validation algorithm

Once a `ParamSpace` is cached, a user-supplied partial binding
can be validated without re-extracting.

**Endpoint.** `POST /api/v1/images/{spec}/validate-params`
(see D7).

**Inputs.** An image spec + a partial binding:

```json
{
  "bindings": { "PORT": 9090, "LOG_LEVEL": "DEBUG" }
}
```

**Steps.**

1. Resolve the image (D5 steps 1–2) — must hit cache or
   extract fresh.
2. Apply defaults: for each parameter with no binding, use its
   `ParamSpace` default. If a `required=true` parameter lacks
   both a binding and a default, return
   `422 RequiredParamUnbound` citing the name.
3. Domain check: each resolved value against its parameter's
   type + constraints.
4. `valueSource` check: for parameters with `valueSource`, call
   the configured endpoint and verify the value is present in
   the returned option set. (Lightweight; the endpoint is
   controller-internal.)

**Output.** A resolved binding (original + defaults filled in)
or a typed error list. The resolved binding is what the
executing agent receives — hyperplane never passes a partial
binding to a container.

## D7 — Controller API endpoints

Two endpoints, both under `/api/v1/images/` (SRD-0108).

### `POST /api/v1/images/{spec}/paramspace`

Trigger extraction (or return cached `ParamSpace`) for an image.

**Path.** `{spec}` is URL-encoded, either a tag ref or a digest
ref.

**Response (200).**

```json
{
  "digest": "sha256:abc...",
  "paramspace": { /* paramodel ParamSpace JSON */ },
  "source": {
    "dockerfile_sha": "...",
    "annotations_extracted": 5,
    "env_inferred": 2
  }
}
```

`source.annotations_extracted` and `source.env_inferred` help
authors verify what was picked up.

### `POST /api/v1/images/{spec}/validate-params`

Validate a binding against the image's `ParamSpace`.

**Request.**

```json
{ "bindings": { "PARAM": value, ... } }
```

**Response (200).**

```json
{
  "digest": "sha256:abc...",
  "resolved_bindings": { /* complete binding after defaults */ },
  "warnings": []
}
```

**Response (422) on validation failure.**

```json
{
  "error": {
    "code": "ParamValidationFailed",
    "details": {
      "failures": [
        { "param": "PORT", "reason": "out_of_range", "bound": 70000, "max": 65535 }
      ]
    }
  }
}
```

Both endpoints require the `image:read` scope (SRD-0114).

## D8 — Caching strategy

Two caches, both in the controller (per SRD-0101):

| Cache | Key | Value | TTL | Invalidation |
|---|---|---|---|---|
| `images` (SRD-0101) | Image digest | Extracted `ParamSpace` | None (digest is immutable) | Only on explicit invalidate + eviction policy |
| `image_tag_cache` (SRD-0101) | `(registry, repo, tag)` | Digest | Short (default 5 min) | TTL expiry |

**Lookup flow.**

1. User names an image (`my-harness:latest` or `my-harness@sha256:abc...`).
2. If tag, resolve to digest via registry HEAD (cheap
   round-trip) — hit `image_tag_cache` first.
3. Hit `images` keyed by digest. If present, serve.
4. If not, pull image metadata, extract `@param` annotations,
   cache by digest, serve.

**Tag stability.** Since tags can move, `image_tag_cache` TTL
trades freshness against HEAD-request cost. Operators can
tune the TTL (SRD-0112); long-lived executions can pin to a
digest up front and skip the tag cache entirely.

**Eviction.** `images` is uncapped by default (per-image
`ParamSpace` payloads are small — kilobytes). A soft cap +
LRU eviction is available via config; unset by default.

**Revisit.** If latency of the HEAD call becomes a real pain
point for a real adopter, fall back to tag-keyed caching with
digest-check invalidation rather than TTL-only. Not now.

## D9 — New invariants

| Code | Invariant |
|---|---|
| `INV-PARAMSPACE-DETERMINISTIC` | `ParamSpace` extraction is a pure function of the image digest; two extractions from the same digest produce identical `ParamSpace`. |
| `INV-PARAMSPACE-CACHE-KEY` | The `ParamSpace` cache is keyed by image digest, never by tag. |

Tests: a TCK case extracts the same image twice and asserts
identical output; a second case extracts via two tags pointing
at the same digest and asserts cache hit on the second.

## Open questions

None remaining.

## Reference material

- `~/projects/hyperplane/docs/parameters/tactile_params.md`
  section "Existing APIs (Ground Truth)".
- `~/projects/hyperplane/containerdefs/DOCKERFILE-CONVENTIONS.md`
  — Java grammar source.
- Paramodel SRD-0004 — `Parameters & Domains` abstractions.
