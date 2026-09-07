# Design: model catalog endpoint discovery

## Problem

Hya currently has two catalog authorities:

1. `hya_app::config::load` flattens configured `providers.<id>.models` into
   `ResolvedConfig.models` for `hya-backend models`.
2. `ProviderRouter::catalog` projects registered provider routes for the server,
   SDK, and TUI.

The two paths can disagree. Providers with empty model lists are skipped. Missing
credentials also suppress routes. Server catalog handlers then fabricate a model
row from `AgentSpec.model` when the router catalog is empty and report every row
as active, enabled, and connected. OAuth login can persist fetched or guessed
models. The TypeScript TUI caches the server projection as if it were a verified
catalog.

This makes configuration, startup discovery, routing, selection, and presentation
separate authorities. A stale default, Session model, category candidate,
Workflow route, or failed OAuth fetch can appear to be a usable model without a
provider-owned catalog result.

## Decisions

The design implements the reviewed product decisions in `prd.md`:

- **D1 — explicit configuration wins.** A non-empty normalized
  `providers.<id>.models` list is authoritative and makes no startup model-list
  request.
- **D2 — no automatic foreign configuration reads.** Runtime startup reads Hya
  config and Hya auth only. The explicit `hya --import compat` migration remains.
- **D3 — real local fallback.** `hya/offline` is the only local model row. It is
  present only when no live configured or discovered row exists.
- **D4 — no refresh flag.** Remove `hya-backend models --refresh`; a normal
  invocation performs a fresh startup composition.
- **D5 — credentials are optional.** Explicit routes and startup discovery work
  without auth headers when Hya has no credential. An unauthenticated 401/403 is
  `auth_required`; a credentialed 401/403 is `auth_rejected`.

## Invariants

1. One immutable startup snapshot owns catalog membership and provider status.
2. Router claims and visible model rows are built from the same normalized IDs.
3. Explicit non-empty lists never perform discovery.
4. Empty or absent lists perform one bounded provider-kind discovery sequence per
   process startup, with optional authentication.
5. Discovery results are process-local. No startup path writes model IDs to
   config, auth files, another product, or a cache.
6. A provider-local discovery failure never removes another provider's valid
   rows and never creates a fallback row for that provider.
7. Only the all-zero-live case creates `hya/offline`.
8. `hya/offline` is served by the built-in echo provider and claims no arbitrary
   model aliases.
9. Config defaults, environment values, CLI overrides, Session events,
   categories, Workflow routes, variants, Recent, and Favorites never add rows.
10. Unknown Session model references remain wire-compatible and fail through the
    existing typed provider route when used.
11. Catalog/status payloads contain no credentials, auth headers, account IDs,
    response bodies, or endpoint health claims.
12. No catalog consumer performs network discovery, normalization, or model
    synthesis.

## Non-goals

- Background polling or hot catalog replacement.
- Provider health, entitlement, quota, or inference probes.
- Validation of explicit user-authored model IDs against the endpoint.
- New image, video, embedding, or multimodal execution support.
- Reading Claude, Codex CLI, Grok CLI, Compat/OpenCode, or plugin configuration
  during normal startup.
- Changing Workflow, category, provider-failover, Session event, or projection
  wire contracts.
- Publishing a release or creating a tag. Version/changelog alignment is part of
  the change; release publication remains a separate user action.

## Target module shape

### Catalog discovery module

Add a deep catalog module under `hya-provider` (recommended
`crates/hya-provider/src/catalog.rs`). Provider protocol, endpoint, request
header, pagination, and response-shape knowledge belong beside the HTTP provider.
Hya credential lookup and policy about when to discover remain in `hya-app`.

The external interface is one provider-local operation:

```rust
pub async fn discover_models(
    request: CatalogDiscoveryRequest,
) -> ProviderDiscoveryOutcome;
```

`CatalogDiscoveryRequest` contains the Hya provider ID, `ProviderKind`, parsed
Hya `base_url`, and an internal optional-auth value. It does not accept a
fallback model, active model, category, Session, cache, or foreign config path.

The implementation hides:

- provider-kind endpoint construction;
- protocol and non-secret session headers;
- optional auth header construction;
- redirect rejection;
- time/body/page/model-count limits;
- response parsing and provider-specific normalization;
- pagination cursor validation;
- safe error classification.

Tests use an internal transport seam or a local HTTP server. Production callers
do not select parsers or construct provider-specific URLs themselves.

### Immutable shared snapshot

Place shared catalog types below `hya-core` and `hya-server`, preferably in
`hya-provider`. This avoids a server-owned DTO becoming the runtime contract and
avoids a dependency from core/server back into `hya-app`.

Recommended shape:

```rust
pub struct ProviderCatalogSnapshot {
    models: Arc<[ProviderModel]>,
    providers: Arc<[ProviderCatalogState]>,
    default_model: ModelRef,
    notice: Option<CatalogNotice>,
}

pub struct ProviderModel {
    pub provider_id: String,
    pub model_id: String,
    pub capabilities: Capabilities,
    pub reasoning_variants: Vec<String>,
    pub reasoning_default: Option<ReasoningEffort>,
    pub source: ModelCatalogSource,
}

pub enum ModelCatalogSource {
    Configured,
    Discovered,
    Offline,
}

pub struct ProviderCatalogState {
    pub provider_id: String,
    pub kind: ProviderKind,
    pub source: ProviderCatalogSource,
    pub auth: ProviderAuthState,
    pub result: ProviderCatalogResult,
}
```

Provider state uses orthogonal values instead of one combined enum:

- source: `configured`, `discovered`, `none`, `offline`;
- auth: `credentialed`, `unauthenticated`, `auth_required`, `auth_rejected`,
  `not_applicable`;
- result: `models`, `empty`, `unavailable`, `invalid`, `unsupported`, `offline`.

These values describe startup composition only. They do not mean healthy,
entitled, or currently reachable.

The snapshot exposes slices, not cloned vectors. Construction sorts model rows by
`provider_id`, then `model_id`; provider states sort by provider ID. Mutation is
private to the builder. There is no refresh method.

### Router and engine integration

`hya-app` builds each `HttpProvider` only after its final model IDs are known.
The resulting router catalog becomes the snapshot's model membership. This keeps
`HttpProvider` claims and visible rows identical.

`RuntimeConfig` carries `Arc<ProviderCatalogSnapshot>` instead of a second flat
`Vec<ModelEntry>`. Remove `ResolvedConfig.models`, `RuntimeConfig.models`, and
`has_providers` after every caller migrates.

`SessionEngine` receives the snapshot through the central app builder. To avoid
changing every test constructor, `SessionEngine::new` can derive a configured
snapshot from `ProviderRouter::catalog`, while the production builder overrides
it with the full app-composed snapshot. `SessionEngine::provider_catalog` returns
a borrowed model slice. Add a snapshot/provider-state accessor for server
projection. Before changing this exported symbol, implementation must run LSP
references and migrate every caller.

No reducer or event schema stores catalog state. Restarting a process builds a new
snapshot; replayed Session model events remain independent historical facts.

## Startup data flow

```text
Hya config.yaml
  -> parse provider declarations, including empty model lists
  -> normalize explicit model entries
  -> resolve optional Hya credential/session material
  -> per provider:
       non-empty list -> Configured, zero network calls
       empty list     -> bounded provider-kind discovery
  -> normalize each outcome and retain provider status
  -> construct authenticated or unauthenticated HttpProvider routes
  -> aggregate router catalog
  -> if live rows empty, construct canonical hya/offline route and row
  -> choose row-backed configured default
  -> freeze ProviderCatalogSnapshot
  -> build RuntimeConfig and SessionEngine
  -> CLI / HTTP / SDK / TUI project the same snapshot
```

Eligible empty-list providers run concurrently with a fixed semaphore. The
snapshot is published only after all provider outcomes settle or hit the batch
deadline. Consumers never observe a partial catalog.

## Configuration and explicit-list normalization

`resolve_providers` must stop skipping empty lists and stop credential-gating
provider declarations. It produces provider plans with optional credentials.

For a non-empty list:

1. trim each ID;
2. reject blank IDs;
3. reject control-invalid IDs;
4. preserve spelling and case;
5. remove exact duplicates while preserving first occurrence;
6. preserve each concrete model's configured reasoning variants/default;
7. reject the reserved exact reference `hya/offline` as a configuration error;
8. make no catalog request;
9. construct a route even when credential material is absent.

A list that becomes empty after normalization is an empty list and therefore
enters startup discovery.

Explicit models remain an assertion by the Hya config author. They can be stale
or unauthorized; that is a request-time provider failure, not a reason for Hya
to invent or silently remove a row.

## Optional authentication

`HttpProvider::new` currently requires a `String` key and every `AuthStyle`
contains a secret. Change the constructor to represent credential absence
explicitly, for example `Option<String>`, and store optional `SecretString`
material inside the provider. Do not represent no auth as an empty token.

Header behavior:

- OpenAI-compatible / Responses: omit `Authorization` when absent.
- Anthropic: omit `x-api-key` when absent; retain `anthropic-version`.
- Google: omit `x-goog-api-key` when absent.
- Codex: omit bearer and account ID when absent; retain required non-secret
  protocol headers and use Hya's known catalog endpoint.
- Grok Build: omit bearer/auth headers when absent; retain required non-secret
  client identity headers and use Hya's existing catalog adapter.

OAuth bearer resolvers and forced refresh hooks attach only when Hya has the
matching OAuth credential. Route identity serialization must distinguish
credentialless configuration without embedding secret bytes.

Discovery uses the same auth semantics. A no-credential request must contain no
bearer, API-key, account-ID, or auth-session header. It still sends protocol
headers such as Anthropic version and non-secret client identity.

## Provider adapter matrix

| Hya `ProviderKind` | Catalog request | Parsing and filtering |
|---|---|---|
| `OpenAiCompatible` | Configured API root plus `/models` | Require OpenAI object shape and non-empty `data[].id`. Preserve path prefix. If authoritative text capability metadata exists, use it; otherwise the configured kind is the protocol assertion. Do not use name-prefix heuristics. |
| `OpenAiResponse` | Same API root `/models`; never append to `/responses` | Same `data[].id` contract and optional auth behavior. |
| `Anthropic` | API root `/models`, `anthropic-version`, optional `x-api-key` | Parse `data[].id`; follow `has_more`/cursor chain within bounds; discard the whole result on malformed or incomplete pagination. |
| `Google` | Configured API root `/models`, optional Google key | Parse `models[].name`; require `models/<id>`; retain only entries advertising `generateContent`; strip the documented `models/` prefix; follow bounded page tokens. |
| `OpenAiCodex` | Existing Hya Codex catalog endpoint/adapter, with or without Hya OAuth material | Reuse the known Codex response parser. A credentialless request is allowed so 401/403 can become `auth_required`; do not read Codex CLI config or guess a public OpenAI endpoint. |
| `GrokBuild` | Existing Hya Grok Build catalog endpoint/adapter, with or without Hya OAuth material | Reuse Grok response normalization and non-secret client headers. Do not infer public xAI configuration or read Grok CLI auth. |

For all adapters, 404/405 or an incompatible custom gateway schema is a local
provider outcome, never a fallback to a different provider kind.

## Resource and security limits

Use dedicated startup discovery limits, not streamed-completion retry settings:

- redirects disabled;
- supported URL schemes only; reject userinfo;
- 3-second connect timeout;
- 8-second per-request/page deadline;
- 10-second process discovery-batch deadline;
- 1 MiB response-body limit per page;
- maximum 8 pages per provider;
- maximum 2,000 normalized models per provider;
- maximum 4 providers in flight;
- no retry/backoff loop;
- no partial publication when a cursor loops, a page is malformed, or a bound is
  exceeded.

Constants remain internal and test-overridable. Logs may contain provider ID,
safe endpoint origin/path, status code, and bounded error class. Logs must not
contain query secrets, headers, tokens, account IDs, response bodies, or model
payload dumps.

## Discovery outcomes

The discovery interface returns typed outcomes:

- `Discovered(models, credentialed|unauthenticated)`;
- `Empty(credentialed|unauthenticated)`;
- `AuthRequired` for no-credential 401/403;
- `AuthRejected` for credentialed 401/403;
- `Unsupported` when Hya has no safe adapter contract;
- `Failed(CatalogFailure)` for invalid URL, redirect, transport, timeout,
  non-auth HTTP status, oversized body, decode/schema, or pagination failure.

A successful response whose normalized set is empty becomes `Empty`. No failure
contains a fallback model. Provider states keep only the safe classification;
detailed internal errors remain logs/errors and are not serialized.

Provider failures are non-fatal to mixed startup. A global Hya config parse or
composition error retains the existing fail-safe runtime behavior, but the
fallback snapshot is canonical `hya/offline`, not a copied default string.

## Default and override selection

Select the process catalog default after the final row set is known:

1. configured `default_model` when it matches a row;
2. a uniquely matching bare configured default;
3. deterministic first live row;
4. `hya/offline` when no live row exists.

`HYA_MODEL`, `--model`, and Session model requests are explicit request-time
overrides. Preserve their current wire behavior: an unknown override does not
become a row and fails through the existing typed unknown-model route if used.
The TUI validates its selected model against the snapshot and therefore ignores
an unknown historical or active override.

Per-provider defaults in compatibility DTOs derive from the same model slice.
The TUI bootstrap must carry the row-backed process default so first selection
matches backend startup rather than an unrelated alphabetical/cache fallback.

## Offline model

The all-zero-live snapshot adds exactly:

```text
provider_id: hya
model_id: offline
model_ref: hya/offline
source: offline
provider state: offline
reasoning: none
tools: false
```

`DevProvider` changes from claiming every model to claiming only `hya/offline`.
It publishes the corresponding catalog row. There is no bare `offline`, `dev/*`,
or arbitrary-model alias.

A request to `hya/offline` returns the user's text and a clear statement that no
live provider is available and the user must configure one. Empty input returns
the same configuration notice. Machine-readable output keeps the notice in the
normal assistant event; interactive startup/TUI may additionally render
non-secret offline metadata without corrupting stdout/JSONL.

If any live configured or discovered row exists, the router and snapshot contain
no offline provider or row.

## Catalog surface cutover

### Backend CLI

`hya-backend models` awaits normal startup composition and reads snapshot rows.
Remove `fallback_model`, synthetic `hya/{fallback}` construction, `_refresh`, and
the CLI `refresh` field. Provider filtering uses declared model membership.
`--refresh` becomes an unknown argument and is absent from help/docs.

Verbose output can add source/provider status fields, but must remain non-secret.
Model IDs printed by normal and verbose modes are identical.

### Server APIs

`crates/hya-server/src/compat/catalog.rs::catalog_models` must stop using
`st.agent.model`. All of these derive from the engine snapshot:

- `/config/providers`;
- `/provider`;
- `/provider/auth`;
- `/api/provider` and `/api/provider/:id`;
- `/api/model`;
- `/tui/bootstrap` provider payloads.

Provider lists are built from declared provider states, not by reverse-grouping
model rows. Therefore `auth_required`, `auth_rejected`, empty, invalid, and
unavailable providers can be shown with empty model maps. `provider_get` returns a
declared zero-row provider rather than a synthetic not-found.

Add source and provider-resolution metadata as additive fields. Model `status`
uses `configured`, `discovered`, or `offline`; `enabled` means a route is
selectable, not healthy. Legacy `connected` contains only providers whose model
endpoint successfully completed discovery in this startup. It excludes explicit
unprobed providers, failures, and the offline provider.

### Rust SDK

Keep `Client::models` tuple shape and `PendingClient::models` forwarding. It reads
only server rows and keeps configured/discovered/offline entries unless
explicitly deprecated. Add tests for variants and no synthetic active model.
Expose provider metadata separately only if an existing SDK provider response can
carry it without changing the tuple.

### TypeScript TUI

Use existing `@opencode-ai/sdk/v2` sync and bootstrap contexts. Do not add a
network client or catalog cache.

- Bundle bootstrap and multi-call fallback decode the same model rows and
  provider states.
- `isModelValid`, `currentModel`, model dialog, prompt, and usage lookup accept
  only snapshot rows.
- Recent/Favorite/variant disk entries remain stored but invisible when their
  row is absent.
- Unknown historical Session models do not repopulate the selector.
- The model dialog shows `hya/offline` only when supplied by the backend.
- Provider/connect presentation distinguishes `auth_required`, `auth_rejected`,
  unavailable, configured, discovered, and offline without calling them healthy.
- Selecting `hya/offline` allows prompt submission and shows the echo/configure
  notice.

Hya-specific decoding/status presentation belongs in `src/hya/`; retained picker
and context behavior stays in the existing `src/upstream/` modules.

## Routing and event compatibility

No event or projection field changes are required.

- `ModelSwitched` continues to persist any accepted wire reference.
- `ProviderRouter::resolve` remains the authority for routability.
- Unknown models remain absent from catalog and return `ProviderError::UnknownModel`
  unless an existing ordered category/Workflow fallback succeeds.
- Category candidates remain ordered routing policy.
- Workflow assignments remain request-local preferred/fallback chains with
  reasoning effort; they do not alter global catalog state.
- Established-stream no-replay and pre-stream failover behavior remain unchanged.
- Reasoning variants remain metadata on one model row; `#variant` never becomes a
  row.

## Foreign configuration and OAuth cutover

Split automatic first-run behavior from explicit migration:

- remove calls from `first_run_config_bootstrap` and TypeScript launcher startup
  that enumerate Compat/OpenCode config candidates;
- retain Hya-only starter config creation;
- retain explicit `hya --import compat` and its selected source argument;
- retain maintenance-only Compat tooling outside runtime;
- add a test that a present foreign config is not opened during normal startup.

OAuth login keeps credential acquisition and provider metadata. It may fetch a
catalog to present login choices, but it must not write a fetched or guessed model
list into an otherwise empty provider block. It must not overwrite a non-empty
user-authored list. Empty/error responses must not persist a guessed model or
new default. An empty list remains empty so normal startup discovers it again.
Existing historical non-empty lists remain explicit under D1; users clear them to
opt into discovery.

## Compatibility and clean removal

Preserved:

- valid explicit provider/model config;
- optional inline, environment, file, and Hya auth credentials;
- explicit Compat migration;
- OAuth credential refresh;
- model reference parsing and Session replay;
- category and Workflow ordering/failover;
- SDK tuple shape and Compat-compatible route envelopes.

Removed without aliases or shims:

- automatic first-run foreign config import;
- credential gating of provider declarations;
- empty-model provider skipping;
- `ResolvedConfig.models` / `RuntimeConfig.models` as a second catalog;
- server fallback from `AgentSpec.model`;
- arbitrary DevProvider model claims;
- bare `offline` and `dev/*` catalog identities;
- OAuth guessed/fetched model persistence into empty lists;
- `models --refresh` parser/help/implementation;
- unconditional `active`/`connected` health claims.

## Alternatives rejected

### Discovery inside `Provider` / `SessionEngine`

Rejected. Route construction needs final IDs, the config catalog can diverge, and
engine/reducer code would gain network and refresh policy. This is a shallow
interface with poor locality.

### Background refresh and cache

Rejected. Consumers can see different catalogs, selection can race model removal,
and stale cache rows become a second authority.

### Intersect explicit config with every startup response

Rejected by D1. It adds startup latency and makes deterministic user configuration
dependent on endpoint availability.

### Empty catalog with no local model

Rejected by D3. The user chose a real local echo row with an explicit configure
notice.

### Guessing a model from defaults or active Session state

Rejected. It recreates the defect and makes route metadata look like provider
evidence.

### Reading another agent's config/cache

Rejected. It violates the Hya-owned source boundary. Explicit import is a
separate user-invoked migration.

## Rollout and rollback

The change has no database migration and writes no discovery cache. Rollout is an
atomic code cutover:

1. add snapshot/outcome types and optional-auth provider support;
2. add bounded adapters;
3. switch app composition to async snapshot construction;
4. migrate router/engine/CLI/server/SDK/TUI consumers;
5. remove synthetic/foreign/refresh paths;
6. update docs, version, and changelog;
7. run focused and full gates, build actual binaries, and smoke the real surface.

Rollback is a code revert. There is no persisted catalog to clean. Composition
and all catalog consumers must roll back together; never leave CLI on config rows
while server uses router rows. Additive presentation fields can be reverted only
if the server still refuses synthetic active/default rows. Rollback must not
restore automatic foreign-config reads, empty auth headers, or guessed OAuth
models.

## Acceptance traceability

| PRD acceptance | Design proof |
|---|---|
| Explicit rows, zero requests | Explicit normalization branch bypasses discovery; request counter test seam. |
| Empty list refreshes each startup | Async composition has no cache/write and invokes one bounded adapter sequence per process. |
| Optional auth and auth status | Optional `AuthStyle`; header omission; typed 401/403 split retained in provider state. |
| Hya-only startup | Config/auth resolution accepts no foreign path; automatic candidate enumeration removed. |
| Failure creates no row | Typed outcomes have no fallback value; snapshot builder adds only the global local row after all live rows are empty. |
| Consistent surfaces | Runtime/engine hold one immutable snapshot; every projection consumes it. |
| No synthetic health | Source/auth/result metadata replaces unconditional active/connected claims. |
| Offline behavior | Canonical `hya/offline` route, row, capabilities, echo, and notice. |
| No offline beside live | Snapshot builder applies offline only after aggregate live membership is empty. |
| Refresh removed | CLI parser, handler, tests, help, and docs delete the field. |
| Routing remains stable | Router claims come from final rows; category/Workflow/events remain unchanged. |
