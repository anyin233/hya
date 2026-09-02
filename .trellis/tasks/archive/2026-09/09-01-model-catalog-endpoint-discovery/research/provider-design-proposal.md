# Technical design proposal: config-authoritative model catalog

## Resolved contract

The catalog is a startup snapshot owned by Hya. It is composed before the backend builds its provider router and is then shared by the CLI, HTTP/API, SDK, and TUI surfaces.

The rules are:

- A provider with a non-empty `providers.<id>.models` list trusts those configured IDs. It does not make a model-list request. The provider still builds a live inference route even when it has no Hya credential; inference then sends no auth and lets the upstream decide whether anonymous use is allowed.
- A provider with an empty model list always issues startup catalog discovery. If a Hya credential resolves, discovery uses it; if no credential exists, discovery sends an unauthenticated request. No other model source is consulted.
- Automatic reads of Claude, Codex, Compat/OpenCode, Grok, or another agent product's config are removed. The explicit `hya --import compat` migration command remains and writes only Hya's own config.
- A failed, empty, malformed, unsupported, unauthorized, or timed-out discovery never creates a model row, default, alias row, or active-model placeholder. The declared provider's non-secret catalog/auth outcome is still retained in the startup snapshot.
- Exactly one reserved fallback row, `hya/offline`, is created only when the final live catalog has no rows. It routes to the built-in echo provider and carries an explicit configure-provider notice. It is not added when any live configured or discovered row exists.
- `hya-backend models --refresh` is removed. Startup discovery is the only automatic fetch; an explicit refresh command is not another persistence path.
- Startup discovery is ephemeral. It does not write `config.yaml`, auth files, another product's config, or a model cache.
- Provider/bootstrap/TUI surfaces expose each provider declared in Hya config with non-secret catalog/auth status. These statuses describe composition and authentication outcomes, not endpoint health or liveness; do not expose `healthy`, `connected`, or similar health claims based on a model-list request.

The design keeps discovery at the config/provider composition boundary. `SessionEngine`, reducers, server projections, and TUI code consume the snapshot and do not fetch, filter, or reconstruct model rows.

## Composition seam

Current composition is `crates/hya-app/src/config.rs::load` followed by `crates/hya-app/src/runtime.rs::resolve_runtime`. `load` is synchronous, skips empty model lists, resolves Hya credentials, and constructs `HttpProvider` before `RuntimeConfig` is assembled. Replace that path with one async boundary rather than adding a second lazy catalog path:

```text
read Hya config
  -> parse provider specs (including empty/non-empty model list)
  -> optionally resolve Hya credential/session, without logging secret material
  -> for each declared provider:
       non-empty list -> Configured outcome, no network, auth optional
       empty list + credential -> authenticated adapter discovery
       empty list + no credential -> unauthenticated adapter discovery
  -> normalize and validate every provider outcome
  -> retain one non-secret status for every declared provider
  -> stage one immutable CatalogSnapshot
  -> build HttpProvider routes from the same normalized IDs (auth optional)
  -> resolve default/active model against that snapshot
  -> build RuntimeConfig/AppState
```

Recommended API shape (names are design-level; implementation may choose the repository's preferred module):

```rust
pub async fn load_with_catalog<C: CatalogHttpClient>(
    client: &C,
) -> Result<ResolvedConfig, ConfigLoadError>;

pub async fn resolve_runtime(
    model_override: Option<String>,
) -> RuntimeConfig;
```

The production implementation can own one `reqwest::Client` and pass it to the discovery service. Tests inject a small `CatalogHttpClient` or a local HTTP server. The important property is that `load_with_catalog` returns only after the snapshot is complete, and `HttpProvider::new` receives the exact IDs used to make the snapshot. `HttpProvider` construction needs a no-auth mode for credentialless explicit routes and credentialless discovery; an absent credential must not be represented by an empty bearer token.

Do not retain a synchronous `load` as a competing default composition path: migrate backend startup, the model CLI, and any SDK/TUI bootstrap that resolves runtime to the one async path. The result is staged per provider and committed once. A provider-local failure does not abort other providers, but no surface sees a partially updated catalog. `RuntimeConfig` should hold an `Arc<CatalogSnapshot>` (or an equivalent immutable value); `AppState`, API projections, and TUI bootstrap should clone the `Arc` and never mutate it.

## Immutable catalog DTO

Keep the internal catalog contract richer than the existing flat `Vec<ModelEntry>`, while preserving the existing reasoning fields needed by routing and retaining status for every declared provider:

```rust
pub struct CatalogSnapshot {
    pub models: Arc<[CatalogModel]>,
    pub providers: Arc<[CatalogProviderStatus]>,
    pub default_model: Option<ModelRef>,
    pub notice: Option<CatalogNotice>,
}

pub struct CatalogModel {
    pub provider_id: String,
    pub model_id: String,
    pub model_ref: ModelRef,                 // provider/id
    pub source: CatalogSource,               // Configured, Discovered, Offline
    pub offline: bool,
    pub reasoning_variants: Arc<[String]>,
    pub reasoning_default: Option<ReasoningEffort>,
    pub capabilities: Capabilities,
}

pub struct CatalogProviderStatus {
    pub provider_id: String,
    pub kind: ProviderKind,
    pub source: ProviderCatalogSource,       // Configured, Discovered, None
    pub auth: ProviderAuthStatus,            // Credentialed, Unauthenticated, Required, Rejected
    pub result: ProviderCatalogResult,       // Models, Empty, Unsupported, Failed
}

enum CatalogSource { Configured, Discovered, Offline }
enum ProviderCatalogSource { Configured, Discovered, None }
enum ProviderAuthStatus { Credentialed, Unauthenticated, Required, Rejected }
enum ProviderCatalogResult { Models, Empty, Unsupported, Failed }

enum CatalogNotice {
    ConfigureProvider { message: String },
}
```

`CatalogProviderStatus` is non-secret. `Credentialed` means only that Hya found usable credential material; `Unauthenticated` means the request/route carried no credential; `Required` means an unauthenticated request was rejected with 401/403; and `Rejected` means a credentialed request was rejected with 401/403. `Models`, `Empty`, `Unsupported`, and `Failed` describe catalog composition, not health. A status may be projected as lower-case wire values such as `configured`, `discovered`, `unauthenticated`, `auth_required`, or `auth_rejected`, but the wire contract must not call any provider healthy/connected merely because discovery succeeded.

The concrete location should be the composition-owned catalog module (prefer `hya-app::config` unless dependency layering requires a small shared type in `hya-provider`). Wire DTOs can be projections of this type, but must preserve `providerID`, `modelID`, `model_ref`, source/availability metadata, every declared provider's status, and the offline notice. Do not serialize auth/session material or adapter failure details. If a provider has no model rows, its status remains in `CatalogSnapshot.providers` and in provider/bootstrap/TUI projections.

`CatalogSnapshot` has no public mutation or refresh operation. Its rows are sorted deterministically by the existing provider/model ordering after per-provider exact-ID deduplication. `ProviderRouter::catalog`, `RuntimeConfig.models` compatibility projections, `hya-backend models`, `/api/model`, provider listing, and `/tui/bootstrap` all derive from this snapshot. There must be one membership set, not a separate “router models” and “visible models” set. Provider status is a second, declared-provider set and must not be synthesized from rows: a failed/empty provider is still visible in provider/bootstrap status, but has no model row.

Reserve `hya/offline` as a runtime-owned reference. It is never read from an external config and is not a normal discovered row. To preserve the “only when no live rows” invariant, a config entry attempting to claim the reserved fallback should be rejected as a reserved ID (while retaining other valid explicit rows) or treated as invalid during normalization; it must not acquire offline metadata.

### Offline row

When the staged live row set is empty, add exactly this row:

```text
provider_id: hya
model_id:    offline
model_ref:   hya/offline
source:      Offline
offline:     true
availability: offline
reasoning:   none
```

Its metadata must say that the built-in provider only echoes input and cannot reason or use tools, and must include the configure-provider notice currently represented by `runtime::OfflineNotice`. Interactive startup may render that notice on stderr or the TUI; machine-readable surfaces carry it as metadata and must not corrupt JSON/JSONL stdout. The offline route must claim `hya/offline` exactly, without exposing a second bare `offline` row or a compatibility alias. If a live row later becomes available, that process's snapshot contains no offline row; there is no background replacement in the same process.

A provider declaration that failed discovery is still listed in `CatalogSnapshot.providers` with `auth_required`, `auth_rejected`, `failed`, or `unsupported` as applicable. The offline row is the only model row added for the all-zero-live case; it does not hide or replace provider statuses.

## Typed discovery outcomes

The adapter boundary must distinguish intentional config authority, optional authentication, and all discovery failures. A useful typed shape is:

```rust
enum ProviderCatalogOutcome {
    Configured {
        models: Vec<ConfiguredModel>,
        auth: AuthPresence,
    },
    Discovered {
        models: Vec<DiscoveredModel>,
        auth: AuthPresence,
    },
    Empty {
        auth: AuthPresence,
    },
    AuthRequired,
    AuthRejected,
    Unsupported { kind: ProviderKind },
    Failed { error: CatalogFailure },
}

enum AuthPresence { Credentialed, Unauthenticated }

enum CatalogFailure {
    InvalidUrl,
    Transport,
    Timeout,
    Redirect,
    HttpStatus { status: u16 },
    BodyTooLarge,
    Decode,
    Schema,
    PaginationLimit,
    AuthRefresh,
}
```

`Configured` is produced directly from a non-empty Hya model list and must not invoke the HTTP client. It carries `Credentialed` or `Unauthenticated` so the route and provider status can reflect auth presence without exposing the credential. `Empty` is a successful response with no usable IDs and also carries auth presence. `AuthRequired` is specifically a 401/403 received when no credential was sent; `AuthRejected` is specifically a 401/403 received when a credential was sent. These two typed outcomes must be preserved in `CatalogProviderStatus` as `auth_required` and `auth_rejected`, not collapsed into generic failure or a health status. `Unsupported` is the intentional result for a provider kind without a safe list contract. `Failed` records only a redacted error class and status; it must not contain a bearer token, API key, account ID, query key, response body, or a URL with userinfo/query secrets.

A successful HTTP response with a valid but empty list is represented as `Empty` and produces no live rows. Invalid or incomplete pagination discards the complete provider result rather than publishing a partial catalog. Normalization runs after the outcome and can turn an all-invalid response into an empty/failure result; it never manufactures a model.

## Adapter interface and provider matrix

Use a registry of provider-kind adapters at the composition boundary. The interface owns endpoint construction, optional auth headers, response parsing, pagination, and capability filtering, but not runtime route construction:

```rust
#[async_trait]
trait ModelCatalogAdapter: Send + Sync {
    fn supports(&self, kind: ProviderKind) -> bool;

    async fn discover(
        &self,
        request: CatalogRequest<'_>,
    ) -> ProviderCatalogOutcome;
}

struct CatalogRequest<'a> {
    provider_id: &'a str,
    kind: ProviderKind,
    base_url: &'a Url,
    auth: &'a CatalogAuth, // Credentialed or Unauthenticated; secret stays internal
}
```

`CatalogAuth` is an internal, non-serializable enum. It is constructed from optional Hya credential/OAuth resolution and has no `Debug` implementation that prints its secret. OAuth refresh should use the existing Hya `ensure_access_token`/force-refresh machinery when an OAuth credential is present; the adapter receives a short-lived resolved session, not a foreign config path. When no credential is present, `CatalogAuth::Unauthenticated` makes a request with no auth header and does not attempt a fake empty token.

| `ProviderKind` | Adapter | Endpoint/shape | Rules |
| --- | --- | --- | --- |
| `OpenAiCompatible` | `OpenAiModelsAdapter` | Configured API root plus `/models`; OpenAI shape `{data: [{id: ...}]}`. | Parse non-empty `data[].id`. This is a contract assertion for custom gateways; 404 or a different shape is a provider-local failure. Send a bearer only when Hya has one; no credential still issues the request. |
| `OpenAiResponse` | `OpenAiModelsAdapter` | Same OpenAI model resource `/models`; Responses completion uses a different request path but does not change the model-list shape. | Use the configured API root, not `/responses` with a suffix. Parse only `data[].id`; credentialless requests are deliberately attempted. |
| `Anthropic` | `AnthropicModelsAdapter` | Configured Anthropic API root plus `/models`; `data[].id`, `has_more`, and cursors. | Send `x-api-key` only when available, plus `anthropic-version`. Request the largest documented page size, follow cursors within a hard page cap, and reject malformed/partial chains. A credentialless 401/403 is `auth_required`; a credentialed 401/403 is `auth_rejected`. |
| `Google` | `GoogleModelsAdapter` | Configured Google API root plus `/models`; `{models: [{name, supportedGenerationMethods}], nextPageToken}`. | Send Google API-key auth only when available. Keep only entries advertising `generateContent`; require `name` to use the `models/<id>` resource form and strip that prefix for the ID expected by `GoogleProtocol`. Follow bounded page tokens. |
| `OpenAiCodex` | `OAuthCatalogAdapter` for Codex | Existing `crates/hya-app/src/oauth/models_catalog.rs::fetch_oauth_models` provider-specific OAuth catalog path and `CatalogModel` normalization. | Do not guess the public OpenAI `/models` URL for the ChatGPT Codex backend. With matching Hya Codex OAuth use the existing helper; without it, issue no guessed private request and return `Unsupported` (the provider status remains declared). Explicit non-empty lists still build a credentialless Codex inference route. |
| `GrokBuild` | `OAuthCatalogAdapter` for Grok Build | Existing OAuth catalog helper/provider-specific path and normalization. Grok Build is the CLI chat-proxy protocol, not the public xAI Inference contract. | Do not infer xAI `/v1/models` or a private Grok path from the configured URL. Use the helper only for supported Hya Grok OAuth credentials; without it, return `Unsupported`. Explicit non-empty lists still build a credentialless Grok inference route. |

The adapter registry is selected from the configured `ProviderKind`, never guessed from a URL, provider ID, or another agent's provider metadata. If a custom OpenAI-compatible provider points at a service that does not implement `/models`, the result is failure/status only and the configured provider contributes no discovered row unless its Hya model list is non-empty. Provider status is retained either way.

## Request construction, auth, and privacy

For generic HTTP adapters, derive the list URL from the configured Hya `base_url` with URL parsing and one safe `/models` path append. Do not concatenate untrusted strings in a way that changes host, and reject URLs with unsupported schemes, userinfo, or invalid syntax. Preserve a provider's configured path prefix (for example `/v1`) while avoiding duplicate separators.

Use a dedicated short-lived discovery client, not the streamed-completion retry/idle policy in `hya-provider/src/http.rs`:

- no redirects (`reqwest::redirect::Policy::none`); an HTTP redirect is a `Redirect` failure and must not forward credentials;
- connect timeout: 3 seconds;
- overall response timeout: 8 seconds per request/page;
- response body limit: 1 MiB, enforced before JSON parsing;
- maximum pages: 8 per provider, with a maximum aggregate model count of 2,000;
- one request per page, no completion-style three-attempt backoff;
- all eligible providers run concurrently behind a bounded semaphore (for example, four in-flight providers), so six configured providers do not add six serial timeout windows;
- a process-level startup budget of 10 seconds is a guardrail around the discovery batch. Providers that exceed their budget become typed failures while other outcomes remain usable.

The exact numeric limits are implementation constants, but the invariants are required: bounded connect/overall/body/page/concurrency costs, no unbounded pagination, and no startup retry loop. A successful page that would exceed the aggregate cap is a `PaginationLimit` failure for that provider, not a partial result.

Auth must reuse the same semantics as completion, while allowing an absent credential:

- OpenAI-compatible and Responses: sensitive `Authorization: Bearer <token>` only when Hya has a token; otherwise omit the header.
- Anthropic: sensitive `x-api-key` only when Hya has a key, plus `anthropic-version`.
- Google: the existing Google API-key header style only when Hya has a key.
- Codex: bearer plus the optional ChatGPT account header, through the existing OAuth helper/session path when matching OAuth exists.
- Grok Build: bearer plus the existing CLI chat-proxy client-version/client-identifier headers, including the Hya package-version/`grok-cli` identity configured by `with_grok_session_auth`, when matching OAuth exists.

Inline `{env:VAR}`/`{file:path}` expansion and Hya auth-file reads stay in `config.rs`/`auth.rs`. Discovery never opens `~/.claude`, `~/.codex`, `~/.grok/auth.json`, Compat/OpenCode files, or plugin resource roots. OAuth access/refresh material stays in memory and is dropped with the snapshot. Error logs contain provider ID, a safe origin/path, status, and error class only; use the non-secret provider status to explain `auth_required`/`auth_rejected`, but never log whether a token value, account ID, or key was present. Never log authorization headers, tokens, account IDs, query keys, or response bodies.

## Normalization and default selection

For every successful adapter result:

1. Require the provider-specific top-level list and a string ID field.
2. Trim IDs, reject empty/control-invalid IDs, and preserve case and provider spelling. Do not add a provider prefix to the model ID itself.
3. Apply authoritative capability filtering only where metadata supports it (`generateContent` for Google; text input/output metadata when xAI data is used by an existing OAuth adapter). Do not add fragile OpenAI prefix heuristics.
4. Deduplicate exact IDs per provider, preserving first-seen order. Do not expand xAI aliases into rows.
5. Attach existing kind-level reasoning defaults only through the established reasoning resolver; no fetched metadata is a reason to claim unsupported capabilities.
6. Sort the final immutable snapshot using the existing stable provider/model catalog order.

Default selection occurs after live rows and the offline fallback are known:

- A CLI `--model` override remains highest priority. If it names no row in a non-offline snapshot, return a clear unknown-model error rather than silently creating a row or selecting a different model.
- Otherwise, accept `HYA_MODEL` only when it matches a live row.
- Otherwise, accept `default_model` from Hya config only when it matches a live row.
- A bare model ID is accepted only when it uniquely identifies one provider row; ambiguous bare IDs require `provider/model`.
- If a configured/env default is stale or points to a provider whose discovery failed, select the deterministic first live row (if one exists) and expose no stale row. If no live row exists, select `hya/offline` and attach the configure-provider notice.
- Never run a model-list request for a non-empty explicit list merely to validate a configured default.

This keeps the active model and visible catalog aligned. The only intentional active model without a remote provider is the reserved offline row, and it is real in the sense that the built-in echo provider claims and serves it.

## Failure handling and rollback

Provider-local discovery failures are warnings, not global config failures. A mixed result behaves as follows:

```text
explicit provider A, no credential -> keep configured rows; build unauthenticated route; no request
empty provider B, no credential -> issue unauthenticated request
empty provider B + 401/403, no credential -> no B rows; status=auth_required
empty provider C + credential + 401/403 -> no C rows; status=auth_rejected
empty provider D + valid response -> publish D's normalized rows
empty provider E + timeout -> publish no E rows; status=failed
all live rows absent -> publish exactly hya/offline + notice
```

An explicit model list is trusted even when no credential is present. `HttpProvider` must still register that route with no auth so inference can be attempted against an endpoint that permits anonymous use. A credentialless discovery request is always attempted for an empty list; only its response determines whether the provider status becomes `auth_required`, `discovered`, `empty`, or another typed result.

Config parse errors and unrecoverable Hya config resolution errors follow the existing runtime error path, but their fallback catalog must also be the single offline snapshot; no stale model ID is copied into it. Do not preserve a previous startup snapshot because there is no safe persistence source and no background refresh.

Rollback is therefore atomic and low-risk:

- Discovery never writes a file. A bad response can only affect the in-memory candidate for that provider.
- The candidate catalog and all per-declared-provider statuses are committed only after outcomes normalize and the reserved-offline invariant is checked.
- A provider adapter can be disabled/reverted without a migration: explicit non-empty lists still build routes (authenticated or unauthenticated); empty/unsupported providers become no-row outcomes with retained statuses; all-zero live outcomes become `hya/offline`.
- A release rollback is a normal code revert. There is no discovery cache or generated config data to clean up.
- The explicit OAuth/Compat import commands remain the only deliberate config-writing paths. Their writes are user-invoked and independent of startup discovery; startup must not invoke them as a recovery action.

## Surface migration and removal of refresh

The following consumers must project the same `CatalogSnapshot`:

- `RuntimeConfig` and provider-router construction in `crates/hya-app/src/runtime.rs`;
- `crates/hya-backend/src/models_cmd.rs` (read-only model rows plus declared-provider status);
- `crates/hya-server/src/compat/catalog.rs` (`catalog_runtime`, provider/model handlers and non-health status metadata);
- `/tui/bootstrap` and SDK/TUI model selection in `packages/hya-tui-ts/src/sdk.ts`, including providers that have no model rows;
- session model selection and workflow route validation, which should resolve against the snapshot but must not perform discovery.

Provider lists must not be built by filtering only rows: a provider with an empty/failed catalog remains visible with its non-secret status. Do not translate `auth_required`, `auth_rejected`, or `failed` into a `connected`/`healthy` claim. `hya/offline` remains a model row only when no live rows exist, while provider statuses remain an independent declared-provider projection.

Remove the `models --refresh` command and its parser/help/tests rather than changing it into a hidden network operation. A user who needs a changed model list restarts the backend, or edits Hya config to provide an explicit list. The explicit `hya --import compat` command remains visible and separate; it imports selected data into Hya config and is never an automatic catalog source.

## Focused verification seams for implementation

These are targeted contract tests, not a project-wide validation plan:

1. **Adapter parser fixtures** (`hya-app` catalog adapter module): OpenAI, Anthropic, Google, and OAuth `CatalogModel` fixtures; empty lists; missing/wrong list fields; blank IDs; exact duplicates; Google prefix/capability filtering; xAI alias non-expansion; malformed, looping, and over-limit pagination.
2. **Injected HTTP client/local server**: assert method/path, configured path-prefix joining, exact auth/session headers, explicit omission of auth headers when no credential exists, redirect rejection, connect/overall timeout, body cap, page cap, and no request for a non-empty explicit model list. Use a request counter to prove the network-free explicit-list contract and a request counter for credentialless empty-list discovery.
3. **Composition tests** (`config::load_with_catalog`): mixed explicit/discovered providers; explicit model list with no credential still yields a router claim and row; empty list with no credential still makes an unauthenticated request; no-auth 401/403 yields typed `auth_required`; credentialed 401/403 yields `auth_rejected`; one provider failure does not remove another; unsupported Codex/Grok without matching OAuth returns no rows; normalized IDs used identically by `CatalogSnapshot` and `HttpProvider` claims.
4. **Provider status tests**: every declared provider appears in the immutable snapshot and provider/bootstrap/TUI projections even with no model rows; statuses expose only configured/discovered/empty/unsupported/failed/auth-required/auth-rejected outcomes; no status is serialized as endpoint health or connection liveness.
5. **Fallback/default tests**: zero live rows produces exactly one `hya/offline`; its source/availability/notice metadata is correct; the DevProvider echoes input; any live row suppresses the offline row; stale configured defaults do not become rows; CLI unknown model is an error; unique/ambiguous bare IDs follow the stated rules.
6. **Surface equality smoke test**: construct one runtime snapshot, then compare IDs, declared-provider statuses, and offline metadata from `models_cmd`, `/api/model`/provider listing, and `/tui/bootstrap`. This catches a second catalog implementation in a projection layer.
7. **Privacy tests**: capture diagnostics for 401/redirect/timeout/malformed responses and assert no token, account ID, query key, or response body appears. Set trap paths for foreign Compat/OpenCode config and verify startup composition never opens them; separately test that explicit `hya --import compat` still reads only its selected source and writes Hya config.
8. **CLI removal test**: `models --refresh` is absent from help and rejected by argument parsing; no refresh network call or config mutation remains.
9. **Existing OAuth tests**: keep provider-specific response parsing in `crates/hya-app/src/oauth/models_catalog.rs`; fixture the helper rather than duplicating private Codex/Grok response schemas in the generic adapter.

This design makes the model catalog a single immutable startup fact: explicit Hya configuration wins without network access and can route without credentials, empty lists always try the configured provider endpoint (authenticated when possible, unauthenticated otherwise), all failures fail closed while retaining non-secret provider status, and the sole synthetic-but-real local row is `hya/offline` when nothing live is available.
