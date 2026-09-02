# Model catalog endpoint discovery

## Goal

Make Hya's visible model catalog authoritative and predictable: use Hya-owned
provider configuration, startup fetches from declared endpoints when model lists
are absent, plus one explicit Hya-owned local `hya/offline` model when no live
rows resolve. Never derive model rows from another agent product's configuration
and never present an unowned or fabricated catalog row.

## User outcomes

- A provider with an explicit `providers.<id>.models` list has a deterministic
  catalog owned by Hya's config.
- A provider with `kind` and `base_url` but no models fetches its model catalog
  from that endpoint on every backend startup, with a Hya credential when one
  exists and without authentication when it does not.
- CLI, HTTP/API, SDK, and TUI surfaces expose the same resolved catalog.
- Missing, failed, empty, malformed, or unsupported endpoint discovery does not
  create a default, active-model, or other synthetic model row.
- Hya does not inspect Claude, Codex, Compat/OpenCode, or another agent product's
  config to populate the runtime model catalog.

## Confirmed current behavior

- `hya_app::config::load` parses `~/.config/hya/config.yaml`, skips provider
  blocks with empty model lists, credential-gates routes, and flattens authorized
  configured models into `ResolvedConfig.models`
  (`crates/hya-app/src/config.rs:1233-1356,1499-1604`).
- Normal startup does not fetch provider catalogs. Only OpenAI Codex and Grok
  Build OAuth login fetch models, then persists them into `config.yaml`
  (`crates/hya-app/src/oauth/mod.rs:202-252`,
  `crates/hya-app/src/oauth/models_catalog.rs:21-123`).
- `hya-backend models --refresh` is accepted but ignored; the command lists the
  authorized config snapshot (`crates/hya-backend/src/models_cmd.rs:5-56`).
- Server catalog endpoints and `/tui/bootstrap` derive from
  `SessionEngine::provider_catalog`. When it is empty, the server currently
  fabricates a row from the active agent model; DTOs also mark every row active,
  enabled, and connected without a provider health check
  (`crates/hya-server/src/compat/catalog.rs:161-229`).
- Compat/OpenCode config can currently be read by the first-run import offer or
  explicit import command (`crates/hya-app/src/config.rs:582-645,777-1037`,
  `crates/hya-ts/src/main.rs:140-169`).

## Requirements

### R1 — Hya-owned source boundary

- The runtime model catalog MUST use only Hya's resolved `config.yaml`, Hya auth
  material for its declared providers, endpoint responses requested from those
  declared providers, and the explicit built-in local `hya/offline` row defined
  by D3.
- A declared provider does not require a credential. Hya MUST support explicit
  and discovered model routes that send no authentication header when Hya has no
  credential for that provider.
- Startup/catalog listing MUST NOT discover, merge, or fall back to another
  agent product's config, model registry, cache, or session files.
- No endpoint may be inferred from another product. Discovery is allowed only
  for a provider already declared in Hya config with its own `kind` and
  `base_url`.
- Automatic first-run detection/import of Compat/OpenCode configuration MUST be
  removed from startup. The user-invoked `hya --import compat` command remains
  an explicit migration exception: it may read the selected source once and
  write Hya-owned config, but runtime discovery MUST never call it.

### R2 — Explicit configured models

- A non-empty `providers.<id>.models` list is authoritative for that provider.
  Startup MUST NOT call the model-list endpoint for that provider.
- Explicit ids MUST be trimmed, blank ids removed, and exact duplicates removed
  before router/catalog construction. Hya does not claim remote existence or
  entitlement for an explicitly authored id; the config author owns that input.
- Reasoning variants/defaults remain metadata on a concrete model, not separate
  catalog rows.
- An explicit model list with no Hya credential MUST still build an
  unauthenticated provider route; absence of a credential is not catalog
  evidence that an endpoint is unusable.

### R3 — Startup endpoint discovery

- On every backend/runtime startup, each configured provider whose `models`
  field is absent or empty MUST request a catalog from its own configured
  endpoint before the process publishes model catalog surfaces.
- Discovery MUST reuse the provider's resolved Hya credential and protocol-
  specific authenticated headers when available. With no Hya credential it MUST
  send the provider-kind request without authentication headers.
- Startup discovery is ephemeral for that process. It MUST NOT write fetched
  rows into `config.yaml`; otherwise the next startup would stop fetching.
- Discovery MUST use bounded timeouts, disabled redirects, bounded response
  bodies, typed errors, and deterministic normalization.
- Blank and duplicate ids MUST be removed. Provider-incompatible modalities
  that Hya cannot execute MUST be removed by an explicit provider adapter rule.
- The immutable startup snapshot MUST retain a non-secret discovery status for
  every declared provider. At minimum it distinguishes explicit configured rows,
  successful discovery, authentication required, credential rejected, empty
  catalog, unavailable/invalid endpoint, and the built-in offline provider.
- A 401/403 response without a Hya credential MUST become `auth_required`; the
  same response with a credential MUST become `auth_rejected`. Both yield no
  catalog rows and MUST NOT invent a fallback model.
- Any other failed, empty, malformed, or unsupported response MUST yield no
  catalog rows for that provider. Other valid providers may still start.

### R4 — Catalog and presentation consistency

- `hya-backend models`, provider/model HTTP endpoints, `/tui/bootstrap`, SDK
  consumers, and the TUI selector MUST expose the same resolved process catalog.
- Catalog endpoints MUST NOT synthesize a row from an arbitrary active/default
  Session model when the provider router has no catalog rows.
- `active`, `enabled`, `connected`, or equivalent presentation MUST NOT claim
  upstream health or existence that startup resolution did not establish.
- Provider HTTP/bootstrap/TUI status surfaces MUST expose the same typed startup
  status without credentials, raw response bodies, or health claims. Legacy
  `connected` output MUST NOT include providers that were only configured or
  failed discovery.
- Config categories, bare-id aliases, `provider/model#variant` forms, Session
  model events, Favorites, and Recent entries MUST NOT add catalog rows. Recent
  and Favorite entries missing from the resolved catalog MUST remain hidden.
- Non-TUI APIs may retain an unknown model in Session state for wire
  compatibility, but it MUST NOT appear in the catalog and MUST fail through the
  existing typed unknown-model route when used.
- When no live provider contributes a resolved model row, publish exactly one
  local catalog row: `hya/offline`. It MUST be identified as local/offline and
  MUST NOT claim upstream connection, entitlement, or health.
- A request to `hya/offline` MUST echo the user's input and include a clear
  notice that no live provider is available and the user must configure one.
- `hya/offline` MUST disappear from the visible catalog when at least one live
  configured/discovered provider model exists; it is not a fallback row added
  beside live models.

### R5 — Compatibility and refresh

- Existing valid explicit provider/model configuration remains supported.
- A running backend uses one immutable resolved catalog snapshot. Config or
  endpoint changes take effect on the next backend startup.
- Remove `hya-backend models --refresh` from CLI parsing, help, docs, and tests.
  Each invocation already builds a fresh startup snapshot; no refresh-specific
  path remains.
- OAuth login keeps Hya credential acquisition and provider metadata writes, but
  MUST NOT persist a fetched or guessed model list into an otherwise empty
  `models` field. Existing user-authored non-empty model lists remain untouched;
  an empty list stays empty so the next startup discovers it again.

## Acceptance criteria

- [ ] A provider with explicit models publishes exactly its normalized explicit
  set and makes zero model-list endpoint requests during startup.
- [ ] A provider with a configured endpoint and no models fetches once per
  process startup; two starts produce two endpoint requests and no config write.
- [ ] A credentialless provider sends no authentication header. A successful
  response creates a routable catalog; 401/403 creates no row and publishes
  `auth_required`. A credentialed 401/403 publishes `auth_rejected`.
- [ ] Normal startup never opens another agent product's configuration.
- [ ] First-run startup offers no Compat/OpenCode import. Explicit
  `hya --import compat` remains functional and later startup reads only the
  resulting Hya config.
- [ ] OAuth login with an empty or failed catalog never writes a guessed model
  or default. Login does not overwrite an existing non-empty model list, and an
  empty list remains eligible for the next startup discovery.
- [ ] Fetch failure, timeout, 401/403, 429/5xx, malformed JSON, oversized body,
  or empty results produce no rows for that provider and no invented model.
- [ ] Provider API/bootstrap/TUI surfaces expose the same non-secret discovery
  status; auth-required endpoints are visible without leaking response bodies or
  credentials.
- [ ] CLI, server bootstrap/catalog APIs, Rust SDK, and TUI expose one consistent
  catalog; stale Recent/Favorite entries and unknown Session models are hidden.
- [ ] No catalog DTO reports synthetic health/connection status as fact.
- [ ] When no live rows resolve, every catalog surface exposes exactly
  `hya/offline` with local/offline metadata. A request echoes the input and tells
  the user that no live provider is available and configuration is required.
- [ ] When any live row resolves, `hya/offline` is absent from the catalog.
- [ ] `hya-backend models --refresh` is rejected as an unknown argument, and
  command help/docs no longer advertise it.
- [ ] Existing inference routing, reasoning variants, category failover,
  Workflow model routes, and pre-stream failover remain unchanged for catalog
  rows that survive resolution.

## Out of scope

- Continuous background catalog polling or hot reload after startup.
- Provider inference health monitoring or entitlement guarantees after startup.
- Reading credentials from any location other than Hya's existing auth/config
  contract.
- Adding image, video, or other model modalities Hya cannot execute.

## Resolved product decisions

- **D1 — Explicit list authority:** trust a non-empty Hya model list after local
  blank/duplicate normalization. Endpoint discovery runs only when that list is
  absent or empty. This keeps explicit configuration deterministic; upstream
  typos or stale entitlements are request-time provider errors, not catalog
  discovery claims.
- **D2 — Other-agent configuration:** remove all automatic first-run discovery
  and reads of other agent configs. Retain `hya --import compat` only as a
  deliberate user-invoked migration; it writes Hya config and is never a
  runtime catalog source.
- **D3 — Offline catalog:** publish the built-in local `hya/offline` row only
  when no live model rows resolve. Offline requests echo input and explicitly
  notify the user that no live provider is available and must be configured.
  This is a named Hya-owned local model, not an active/default synthesis.
- **D4 — Refresh flag:** remove `hya-backend models --refresh`. A normal
  `hya-backend models` invocation resolves a fresh startup catalog, so a second
  refresh contract would be redundant and conflict with D1.
- **D5 — Credentialless providers:** allow unauthenticated discovery and
  inference when a declared provider has no Hya credential. Startup discovery
  classifies 401/403 as `auth_required`, so the product can ask for auth without
  inventing a model row; credentialed 401/403 is `auth_rejected`.


## Notes

- This is a cross-layer backend/API/TUI contract change. Planning requires
  `design.md` and `implement.md` before implementation approval.
- Keep endpoint discovery at the provider/config composition boundary; reducers,
  Session events, and the TUI must consume the resolved catalog rather than
  recreate discovery or validation logic.
