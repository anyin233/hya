# Provider catalog discovery research

## Scope and current composition

This note covers the six `ProviderKind` values in `crates/hya-provider/src/http.rs::ProviderKind` and the path from Hya configuration to every catalog surface. It is repository-grounded; no user credential values or user configuration contents are included here.

The current composition is synchronous and config-owned:

1. `crates/hya-app/src/config.rs::config_path` looks only for Hya's own `XDG_CONFIG_HOME/hya/config.yaml`, then `HOME/.config/hya/config.yaml`.
2. `crates/hya-app/src/config.rs::load` parses that YAML, calls `resolve_providers`, resolves one Hya credential per provider with `resolve_provider_credential`, builds an `HttpProvider`, and returns `ResolvedConfig`.
3. `resolve_providers` currently skips a provider when `provider.models` is empty. `model_entries` flattens the remaining configured IDs into `ResolvedConfig.models`. A provider with no Hya credential is also skipped before it can reach the router or catalog.
4. `crates/hya-app/src/runtime.rs::resolve_runtime` calls `config::load`, chooses the configured/default/environment model, and places the same `ProviderRouter` and `models` in `RuntimeConfig`. When no usable config exists it uses `offline_router`, which contains `DevProvider` and the internal model string `offline`, while `RuntimeConfig.models` is empty.
5. `crates/hya-provider/src/router.rs::ProviderRouter::catalog` aggregates `ProviderModel` rows from registered providers. `HttpProvider` claims only the models passed to `HttpProvider::new`; this is the correct place to ensure the router and the flat catalog receive exactly the same final IDs.

The backend startup path (`crates/hya-backend/src/serve.rs`, runtime construction) and the CLI model path (`crates/hya-backend/src/models_cmd.rs`) both consume this resolved composition. The compatibility HTTP catalog handlers in `crates/hya-server/src/compat/catalog.rs` (`catalog_runtime`, `provider_list`, and model-list handlers) project the runtime catalog for API clients. TUI bootstrap (`packages/hya-tui-ts/src/sdk.ts`, `/tui/bootstrap`) and SDK/API consumers must continue to consume this one snapshot; discovery must not be reimplemented in reducers, `SessionEngine`, or TUI code.

## Existing authentication and HTTP behavior

`crates/hya-app/src/config.rs::resolve_provider_credential` establishes the source boundary:

- `crate::auth::load_credential(provider.id)` reads Hya's auth material under `~/.config/hya/auth/<provider>.yaml` (or the XDG equivalent) and takes precedence over an inline `api_key`.
- Inline `api_key` accepts a literal, `{env:VAR}`, or `{file:path}` through `resolve_secret`.
- OAuth credentials carry an access token, optional account ID, and a refresh marker. `load` wires `BearerResolver` to `oauth::ensure_access_token` and `AuthRefresher` to `oauth::force_refresh_access_token` for streamed requests.
- `ProviderKind::GrokBuild` always receives the CLI chat-proxy session identity through `HttpProvider::with_grok_session_auth`; `ProviderKind::OpenAiCodex` receives the optional account identity through `with_codex_session_auth`. The configured bearer is still the source of authentication. The config module explicitly documents that Grok does not read `~/.grok/auth.json` and Codex OAuth is stored in Hya's own auth directory.

`crates/hya-provider/src/http.rs::HttpProvider` owns the request auth style and endpoint protocol:

- OpenAI-compatible and Responses routes use a sensitive `Authorization: Bearer …` header.
- Anthropic uses a sensitive API-key header plus the configured Anthropic API-version header (`AuthStyle::Anthropic`).
- Google uses the Google API-key auth style (`AuthStyle::Google`).
- Codex uses bearer plus its optional ChatGPT account header (`AuthStyle::CodexSession`).
- Grok Build uses bearer plus the CLI chat-proxy client-version/client-identifier headers (`AuthStyle::GrokSession`). Discovery must use the same style as completion, including the Grok `CARGO_PKG_VERSION`/`grok-cli` identity supplied at composition.

The completion client deliberately disables redirects: `HttpProvider::new` uses `reqwest::redirect::Policy::none`, because following a cross-origin redirect with an API credential is unsafe. Its normal streamed-completion constants are `RESPONSE_HEADER_TIMEOUT` (60 seconds), `STREAM_IDLE_TIMEOUT` (300 seconds), and up to three request attempts with retry/backoff. Those settings are for long-running SSE completions, not startup discovery. Reusing the three-attempt completion policy would make startup latency unpredictable.

The OAuth catalog helper (`crates/hya-app/src/oauth/models_catalog.rs::fetch_oauth_models`, re-exported by `crates/hya-app/src/oauth/mod.rs`) already has provider-specific live catalog handling for OAuth login. It uses access token/account information and normalizes its provider-specific response into `CatalogModel`. `oauth/mod.rs` invokes it during login, then `config::upsert_oauth_provider` writes non-secret catalog data into Hya config. This is an explicit OAuth/login path, not an automatic runtime read of another product's config.

## Provider endpoint matrix

The public endpoint details below come from first-party API documentation. A configured custom gateway can still return 404/405 or a different schema; that is a discovery failure, not a reason to invent a model row.

| Hya kind | Model-list endpoint and response | Discovery disposition |
| --- | --- | --- |
| `OpenAiCompatible` | Append `/models` to the configured OpenAI-compatible API root (normally a root ending in `/v1`). OpenAI's `GET /v1/models` returns an object with `data`, where each model object has a non-empty `id`. | Safe generic adapter when the configured route claims OpenAI model-list compatibility. Parse only `data[].id`; custom providers that do not implement it fail closed. |
| `OpenAiResponse` | The OpenAI Responses request path differs (`/responses`), but the OpenAI model resource remains `GET /v1/models` with `data[].id`. | Safe generic OpenAI list adapter for a Responses-compatible API root. Do not derive an endpoint from a different provider or from a Responses request URL. |
| `Anthropic` | `GET /v1/models` returns `data[]` model objects with `id`, and is paginated with `after_id`, `before_id`, `limit`, `has_more`, and cursor fields. Authentication is `x-api-key` plus `anthropic-version`. | Safe provider-specific adapter. Request the largest documented page size, follow bounded cursors, and require a complete valid result before publishing it. |
| `Google` | `GET https://generativelanguage.googleapis.com/v1beta/models` returns `{ "models": [...], "nextPageToken": ... }`. Entries use a resource `name` such as `models/gemini-…`, and expose `supportedGenerationMethods`. | Safe provider-specific adapter. Keep only entries advertising `generateContent`, strip the `models/` resource prefix to the ID expected by `GoogleProtocol`, and follow bounded page tokens. |
| `OpenAiCodex` | The completion route is the ChatGPT Codex backend, not the public OpenAI API. There is no stable public `/models` contract for that private backend in `ProviderKind`/`OpenAiResponsesProtocol`. The existing OAuth-only `fetch_oauth_models` helper is the repository's provider-specific catalog authority for a logged-in Codex account and returns normalized `CatalogModel` values. | Do not probe a guessed public `/models` URL. Reuse the existing OAuth catalog adapter only when the configured Hya credential is the matching OAuth/session credential. If that adapter is unavailable or fails, publish no rows. |
| `GrokBuild` | The Hya route is the Grok Build CLI chat-proxy protocol (`GrokBuildProtocol`), with special session headers and encrypted reasoning behavior. It is not the public xAI Inference contract. The public xAI `GET /v1/models` endpoint returns `data[]` with IDs and aliases, but that endpoint must not be inferred for a Grok Build base URL. The OAuth catalog helper is the existing provider-specific live-catalog seam for a logged-in Grok Build credential. | Do not guess a private Grok `/models` path. Reuse the OAuth adapter only when it is supported for the Hya credential; otherwise treat the kind as unsupported for automatic startup discovery. A user who configures the public xAI API should use an OpenAI-compatible kind and its documented `/models` contract. |

Primary references:

- [OpenAI List models](https://developers.openai.com/api/reference/resources/models/methods/list)
- [Anthropic Models API](https://docs.anthropic.com/en/api/models-list)
- [Google Gemini `models` REST resource](https://ai.google.dev/api/models)
- [xAI Inference Models](https://docs.x.ai/developers/rest-api-reference/inference/models)

## Normalization, filtering, and deduplication

Discovery must produce real IDs only; it must never add a configured default, a provider name, a protocol name, an alias placeholder, or `offline` as a row.

Recommended common rules:

- Require a successful 2xx response and a valid object with the provider-specific list field. A missing field, wrong JSON type, invalid UTF-8, or malformed cursor response invalidates that provider's discovery result.
- Accept only string IDs that trim to a non-empty value. Preserve the provider's ID spelling; do not lowercase, rewrite, or synthesize IDs. For Google, the documented resource prefix is the one deliberate normalization because `GoogleProtocol` addresses a bare model ID.
- Deduplicate exact IDs per provider, preserving first-seen order before the existing router catalog sort. Do not expand xAI `aliases`: aliases are not additional canonical model rows and can make one model appear multiple times. If alias support is required later, it needs an explicit model-reference contract rather than implicit rows.
- Apply capability filters only where the endpoint publishes authoritative metadata. Google must advertise `generateContent`; xAI can require text input/output metadata when those fields are present. The generic OpenAI shape does not reliably distinguish chat, embedding, image, and moderation models, so do not invent prefix heuristics. For a generic route, the configured `kind` is the user's protocol assertion and `data[].id` is the only trustworthy common field; explicit Hya model lists remain the curation mechanism.
- For Anthropic and Google pagination, use a bounded maximum page count and response/body budget. If the cursor chain is malformed, loops, or exceeds the bound, discard the whole provider result instead of exposing a partial catalog that looks authoritative.
- Keep reasoning metadata separate from model identity. A fetched model with no advertised reasoning data receives the existing kind-level reasoning defaults only where the provider protocol already defines them; it does not get a fabricated model-specific capability claim.

`ProviderRouter::catalog` already gives the final aggregated list a stable provider/model ordering. The discovery layer should deduplicate before `HttpProvider::new` and `model_entries`, so the router's claimed model set and every surface have identical membership.

## Failure policy and startup latency

Discovery is optional per provider, not fatal to the entire config. The composition boundary should:

- Fetch only when the provider has a valid Hya credential and no configured model IDs. A non-empty `providers.<id>.models` list is authoritative and must not trigger a network request.
- Fetch all eligible providers concurrently, with one bounded discovery request (or one bounded, capped pagination sequence) per provider. Do not use the completion client's three-attempt backoff loop. A strict connect/overall deadline in the low-seconds range, a bounded response body (for example, a small MiB cap), and `Policy::none` redirects keep six configured providers from adding their timeouts serially.
- Treat invalid URL, DNS/TLS failure, timeout, redirect, 401/403, 404/405, rate limit, 5xx, malformed response, unsupported schema, and successful empty lists as zero discovered models for that provider. A zero result never falls back to a default ID, an active model, or a synthetic/offline row.
- Continue composing other valid providers after one failure. Log only provider ID, safe endpoint origin/path, status, and a bounded sanitized error class. Never log authorization headers, tokens, account IDs, query keys, or response bodies.
- Build `HttpProvider` routes and `ResolvedConfig.models` only after each provider's final IDs are known. This prevents a route from claiming a model that the visible catalog does not contain.
- Resolve the active default only against the final catalog. The current `choose_default` accepts a file/env value even when it is not in `models`; that can leave an active but unroutable reference after discovery failure. A live configured runtime with no resolved rows must not silently turn a failed provider into an `offline` catalog row. Whether the internal DevProvider remains available for a no-config/offline runtime is a separate UX decision; its internal fallback must stay out of the live catalog.
- Keep the fetched result in the in-memory `ResolvedConfig`/`RuntimeConfig` snapshot only. Do not write startup results to `config.yaml`, auth files, another agent's config, or a cache that can later be mistaken for a current authoritative list. Every backend startup therefore performs a fresh fetch when the Hya model list is absent.

The current `config::load` and `runtime::resolve_runtime` signatures are synchronous. The least surprising implementation seam is an async composition function (for example, a single `load_with_discovery`/runtime resolver) called before `ProviderRouter` construction, followed by the existing synchronous engine assembly. All API, SDK, CLI, and TUI surfaces then read one immutable startup snapshot; none performs lazy or background discovery. This changes startup bootstrap call sites, but avoids race-prone catalog updates and keeps `SessionEngine` focused on routing and event sourcing.

## Config privacy and explicit migration boundaries

The runtime path must not inspect another agent product's config. Current external-config code is isolated from `config::load`:

- `crates/hya-app/src/config.rs::default_compat_config_path` and `compat_config_candidates` inspect `COMPAT_CONFIG`, XDG Compat locations, and OpenCode locations under the home directory.
- `first_run_config_bootstrap(interactive)` calls those candidates only while interactively creating a brand-new Hya config. It prompts before importing.
- `import_compat_models_into_config` explicitly parses the selected Compat/OpenCode JSON/JSONC config, maps provider metadata/models, and writes Hya's own `config.yaml`. `imported_compat_providers`, `render_imported_hya_config`, and `merge_import_into_existing_config` are migration helpers, not startup catalog sources. This is the explicit migration command/path discussed in the PRD and must not be called by runtime discovery.
- `crates/xtask/src/sync_compat/discover.rs::load_compat_config` and `crates/xtask/src/sync_compat/apply.rs` are explicit maintenance tooling for Compat/OpenCode skill/MCP synchronization. They are not part of backend startup and must not become a model inventory fallback.
- `crates/hya-plugin-compat/adapter/src/initialize.ts` can establish compatibility resource roots for the plugin adapter. That bridge is not a provider catalog authority. Runtime model discovery must not ask it for model rows.
- There are no runtime reads of Claude, Codex, Grok, or another agent's config in `config::load`; in particular, the Grok comment in `config.rs` states that `~/.grok/auth.json` is not read. Hya auth and Hya config are the only automatic sources.

The explicit OAuth login path is different from automatic startup: `fetch_oauth_models` obtains a live provider catalog with Hya OAuth material, and `upsert_oauth_provider` writes a Hya-owned model list. However, its current empty/error branch preserves old models or falls back to `default_model_id`. That behavior can create a stale or synthetic row when the fetch returns nothing, so the implementation plan must either remove that fallback or clearly keep it outside the new no-synthetic catalog contract. Startup discovery itself must never call the Compat/OpenCode import helpers and must never persist a failed/empty response.

## Test seams already present

No network test suite was run for this research. Existing seams to use during implementation are:

- `crates/hya-app/src/config.rs` test helpers and tests around `parse_config`, `resolve_provider_credential_with`, provider-kind parsing, reasoning metadata, and model-entry flattening. These can cover explicit-vs-discovered composition without exposing secrets.
- `crates/hya-app/src/oauth/models_catalog.rs` is the natural parser/adapter seam for OAuth response fixtures; `fetch_oauth_models` should remain the only owner of private Codex/Grok OAuth catalog schemas.
- `crates/hya-provider/src/http.rs` has isolated header/auth and timeout builder methods (`with_response_header_timeout`, `with_idle_timeout`) but no general model-list mock transport. Discovery should have an injectable HTTP client/transport or a small adapter trait so parser and status/timeout tests do not require the Internet.
- `crates/hya-e2e/src/backend.rs::BackendSpec` and `BackendProcess::start` write a temporary Hya config and currently always include explicit `models`; `model_id`/`additional_models` plus a local fake upstream are a suitable end-to-end seam for an absent-model-list startup case. The fixture must add a model-list response without putting discovered IDs in the config.
- `crates/hya-server/src/compat/catalog.rs` and backend catalog tests exercise projection/API shape. They should assert that all surfaces see the same resolved snapshot and that failed/empty discovery yields no row/status claim rather than an `offline` placeholder.

## Design options

### Option A — async discovery at the Hya config/composition boundary (recommended)

Parse Hya config into provider specs, resolve Hya credentials, dispatch a provider-kind catalog adapter only for providers without models, normalize results, then construct `HttpProvider`, `ResolvedConfig.models`, and the router from the same final IDs.

**Advantages:** one authoritative snapshot; no `Provider` trait churn; explicit config remains deterministic; private OAuth handling stays in the existing helper; easy to keep all other-agent config out of the call graph; failures are local to one provider.

**Costs:** `config::load`/runtime bootstrap must become async or accept a runtime-owned discovery future; startup call sites need a coordinated update; an injectable discovery client is additional composition code.

### Option B — add model discovery to the provider abstraction

Extend `hya_provider::Provider`/`HttpProvider` with an async `list_models` capability, and let each concrete provider own its list endpoint and parser before router registration.

**Advantages:** endpoint/auth knowledge sits beside protocol knowledge; provider-specific Codex/Grok behavior can be encapsulated.

**Costs:** the router currently needs model claims to resolve requests, so construction becomes circular (discover provider, then register claims); every `DevProvider`/`FakeProvider` implementation and trait object changes; the flat config catalog can diverge from provider claims; config privacy and startup policy become scattered across provider implementations. This is deeper than needed for a startup-only catalog.

### Option C — start from config, then refresh/cache asynchronously

Start immediately with configured rows, fetch missing catalogs in the background, emit a catalog update, and optionally persist a cache or update `config.yaml`.

**Advantages:** low initial startup delay and potentially better offline behavior.

**Costs:** CLI, API, SDK, and TUI can observe different catalogs; a model can disappear between selection and use; stale cache rows violate the no-nonexistent-row rule; persistence makes a network response silently authoritative and undermines config ownership. This conflicts with the requested predictable startup snapshot and is not recommended.

## Recommended boring design

Use Option A with these boundaries:

1. Keep `providers.<id>.models` authoritative. When it is empty/absent, dispatch only a known adapter for that `ProviderKind`; do not infer provider kinds or read external configs.
2. Use the existing Hya credential resolver and exact provider auth/session headers. For OAuth, obtain a current Hya access token and use the existing private OAuth catalog helper for Codex/Grok rather than a guessed `/models` URL.
3. Use a dedicated discovery HTTP client with no redirects, strict connect/overall/body limits, bounded pagination, concurrent providers, and no completion retries. Return a provider-local failure outcome, not a global config error.
4. Parse only real provider response IDs, apply authoritative generation-capability filters, strip Google's documented resource prefix, deduplicate exact IDs, and reject empty/malformed/partial results. Never expand aliases or fill a default.
5. Build the `HttpProvider` model claims and `ResolvedConfig.models` from one normalized result. Make default/active-model selection require a matching resolved row. Keep offline DevProvider behavior internal to the no-config path; do not expose it as a synthetic live catalog row.
6. Keep discovery ephemeral for each backend startup. If `hya-backend models --refresh` is retained, make it an explicit one-shot fetch that uses the same composition and never mutates Hya config. Preserve the separate explicit OAuth/Compat migration paths, but remove any empty-fetch fallback that writes a fake model.

This preserves the existing deep seam—config composition builds the router—while making the catalog contract true end to end: Hya config first, live provider endpoint only when needed, no cross-agent reads, and no fabricated rows.
