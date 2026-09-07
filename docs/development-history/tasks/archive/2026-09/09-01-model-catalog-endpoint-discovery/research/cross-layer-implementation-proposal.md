# Cross-layer implementation proposal

## Contract and architecture

This proposal converts `research/catalog-surface-audit.md` into a test-first
cutover. It is planning-only. The implementation MUST preserve the resolved
D1-D5 decisions:

- A non-empty `providers.<id>.models` list is Hya-owned authority after trimming
  blank ids and removing exact duplicates. It does not trigger discovery.
- An absent/empty normalized list triggers one provider-owned catalog request at
  each backend startup. Results are ephemeral and never written to `config.yaml`.
- A provider may have no credential. Explicit models still build an
  unauthenticated route. Empty-list discovery sends no auth header. A 401/403
  becomes `auth_required` without a credential and `auth_rejected` with one.
- Each declared provider remains in an immutable, non-secret startup-status
  snapshot even when it contributes no rows. Status is a resolution fact, never
  an entitlement or upstream-health claim.
- Runtime startup never reads another agent product's config, cache, registry,
  or sessions. Automatic first-run Compat/OpenCode import is removed; explicit
  `hya --import compat` remains a migration and writes Hya config.
- If no configured/discovered live row resolves, publish exactly `hya/offline`.
  It is a real local row, not a fallback made from an active/default/session
  model. It echoes input and says that a live provider must be configured, and
  disappears as soon as any live row exists.
- Remove `hya-backend models --refresh`; `models` performs fresh startup
  composition like every other backend entry point.
- CLI, server, `/tui/bootstrap`, SDK, and TUI consume one snapshot. Categories,
  Workflow assignments, aliases, session events, and Recent/Favorite cache
  entries never add rows.

Keep discovery at the composition seam (`hya-app` config/runtime), before
router, engine, or HTTP publication. The current synchronous
`crates/hya-app/src/config.rs::load` and `crates/hya-app/src/runtime.rs::resolve_runtime`
should gain one async equivalent (proposed `load_with_discovery`) rather than
adding lazy discovery to `SessionEngine`, reducers, APIs, or the TUI.

Use shared types below `hya-server` (the dependency graph already has
`hya-app -> hya-server`, so server cannot own them), preferably in
`crates/hya-provider/src/lib.rs`:

```text
CatalogSource = configured | discovered | local
ProviderResolution = configured | discovered | empty | auth_required |
                     auth_rejected | discovery_failed | unsupported | local
ProviderModel = provider_id, model_id, capabilities, reasoning_variants, source
ProviderCatalogSnapshot = model rows + ordered declared-provider resolutions
```

Names may change during implementation; these invariants may not. Extend
`ProviderRouter` to expose the immutable snapshot/status list while keeping
`ProviderRouter::catalog()` as the model projection. Extend `ProviderModel` with
source/provenance so the server can identify `hya/offline` without an id-only
special case. Reserve provider id `hya` for the local row and reject a collision.

`HttpProvider::new`/private `AuthStyle` in
`crates/hya-provider/src/http.rs` currently require a key. Add optional auth or
`AuthStyle::None`; credentialless routes MUST omit bearer, API-key, account, and
session headers, not send empty values. Discovery uses the same Hya credential
and provider-kind headers as completion, no redirects, bounded body/pagination,
a strict startup deadline, and no completion retry loop. Codex/Grok use existing
Hya OAuth catalog adapters only when the declared Hya credential supports them;
never guess a private/public endpoint.

## Cross-layer cutover map

| Layer | Exact symbols/files | Required change |
| --- | --- | --- |
| Config | `crates/hya-app/src/config.rs`: `ResolvedConfig`, `ModelEntry`, `resolve_provider_credential`, `resolve_providers`, `model_entries`, `choose_default`, `load` | Preserve declarations without credentials; normalize explicit ids; discover only empty lists; classify outcomes; build routes after final ids; return snapshot/status; choose defaults only from rows. Remove empty-provider skip and fallback rows. |
| Provider | `crates/hya-provider/src/lib.rs`: `ProviderModel`, `Provider`; `router.rs`: `ProviderRouter::catalog`, `resolve`; `http.rs`: `AuthStyle`, `HttpProvider::new`, `HttpProvider::catalog`; `dev.rs`: `DevProvider` | Add source/status types, optional auth, injected provider-kind adapters, exact final claims, and local row. Narrow DevProvider to `hya/offline` plus any deliberate bare `offline` route alias; unknown refs must not echo. |
| Runtime | `crates/hya-app/src/runtime.rs`: `offline_router`, `RuntimeConfig`, `resolve_runtime`, `HyaRuntime::start`; `lib.rs` re-exports | Await one startup snapshot in every path, use `hya/offline` for no-live default, separate command override from row-backed default, pass router/status through engine and force-offline startup. |
| CLI | `crates/hya-backend/src/cli_args.rs::Command::Models`; `main.rs`; `models_cmd.rs::cmd_models`/`model_lines` | Delete `refresh`, `_refresh`, parser/help/match handling. Consume router/snapshot rows; remove `fallback_model` and `hya/{fallback}` construction. |
| Server | `crates/hya-server/src/compat/catalog.rs`: `CatalogModel`, `catalog_models`, `provider_ids`, `provider_infos`, `default_models`, `bootstrap_provider_payload`; `compat/catalog/types.rs`: `ProviderInfo`, `ModelInfo`, `model_info`; `compat/tui.rs::bootstrap` | Project snapshot only; remove `st.agent.model` fallback. Include declared provider status (including zero-row failures), local/source metadata, shared defaults, and non-health status semantics on all legacy/v2/bootstrap routes. |
| Engine/session | `crates/hya-core/src/engine.rs::provider_catalog`; `engine/session_state.rs`; proto `event.rs`/`projection.rs`; server `compat/model_ref.rs`, session routes, `reference.rs` | Add snapshot/status accessor without changing event wire shape. Unknown `ModelRef`/`ModelSwitched` values round-trip, remain absent from catalog/TUI, and fail through existing `UnknownModel` or existing fallback. |
| SDK | `crates/hya-sdk/src/client.rs::Client::models` (implementation 451-494); `pending.rs::PendingClient::models` | Keep tuple API and `/config/providers`; parse the shared row set, variants, and local row. Additive status/source fields must not shift tuple positions; add a separate metadata method only if required. |
| TUI | `packages/hya-tui-ts/src/upstream/context/sync.tsx`: `bootstrap`, `bootstrapViaBundle`, `applyBootstrapBundle`, `bootstrapViaMultiCall`; `context/local.tsx`: `isModelValid`, `getFirstValidModel`, `fallbackModel`, `currentModel`; `component/dialog-model.tsx::DialogModel`; `component/use-connected.tsx::useConnected`; prompt | Bundle and multi-call paths consume identical rows/statuses. Keep stale cache fail-closed, expose local row only from snapshot, show configure notice, and stop using `provider.length > 0` as live connectivity. No TUI discovery/cache authority. |
| Routing | `crates/hya-core/src/category.rs::CategoryRegistry::resolve_servable`, `apply_spawn_model_policy`; `crates/hya-core/src/workflow/run.rs::resolve_agent`, `resolve_model_route`; `crates/hya-app/src/workflow_control.rs` | Keep category/Workflow candidates request-local and router-servability based; preserve ordered failover/reasoning. Never add candidates to catalog. |
| Import/OAuth | `config.rs::first_run_config_bootstrap`, `default_compat_config_path`, `import_compat_models_into_config`; `crates/hya-ts/src/main.rs`; OAuth `fetch_oauth_models`, login/upsert | First run creates Hya starter only. Preserve explicit import. Remove login empty/error guessed-model persistence so stale OAuth rows do not become implicit startup authority; retain Hya credential refresh/adapters. |

## Ordered TDD slices and rollback points

### 1. Snapshot/source/status types (RED → GREEN → REFACTOR)

**RED:** Tests represent declared providers with zero rows, source metadata,
`hya/offline`, deterministic order, and credentialless explicit routes.

**GREEN:** Add shared types/accessors in `hya-provider/src/lib.rs`; extend
`ProviderModel` and `ProviderRouter` snapshot/status handling; update fake model
constructors. No server-local duplicate status type.

**Rollback:** Revert this type-only commit if downstream work cannot compile.

### 2. Optional auth and discovery adapters

**RED:** `crates/hya-provider/tests/http_headers.rs` and focused `http.rs` tests
assert no auth headers for absent credentials; provider adapters cover OpenAI,
Anthropic, Google, OAuth kinds, normalization, modality filtering, malformed or
oversized bodies, pagination bounds, redirects, and credentialless/credentialed
401/403 classification.

**GREEN:** Add `AuthStyle::None`/optional key and an injectable discovery client
in `crates/hya-provider/src/`. Return typed `Rows`, `Empty`, `AuthRequired`,
`AuthRejected`, `Failed`, or `Unsupported` outcomes. Keep discovery separate
from `Provider` stream methods.

**Rollback:** Revert adapter/auth commit independently before composition; never
restore an empty auth header.

### 3. Async config composition and fresh startup

**RED:** Focused `hya-app` tests assert explicit ids make zero discovery calls;
empty/absent/only-blank lists fetch once per startup; no-credential success sends
no auth; 401/403 split to `auth_required`/`auth_rejected`; all failures produce
zero rows but retain status; two starts make two requests; config bytes stay
unchanged; Compat/OpenCode paths are never opened.

**GREEN:** In `config.rs`, preserve provider plans and optional credentials,
normalize, discover empty plans concurrently, build `HttpProvider` only after
final ids, and return immutable rows/status. Update `resolve_runtime` and every
backend/app caller to await it. Keep category/subagent parsing local.

**REFACTOR:** Bound timeout/body/pagination, continue other providers after one
failure, log only safe provider/status/error class. No fetched cache.

**Rollback:** Revert resolver plus all startup callsite changes together; do not
keep a half-migrated CLI/server split.

### 4. Local offline route and default selection

**RED:** Assert no-live composition has exactly `hya/offline` with local metadata
and echo/config notice; any live row suppresses it; unknown refs do not become
rows and use typed unknown behavior.

**GREEN:** Update `DevProvider`, `offline_router`, `RuntimeConfig`, and
`HyaRuntime::start`; make local route claim only canonical/intentional aliases.
Remove rows derived from `AgentSpec.model`, config/env defaults, categories,
Workflow, sessions, or cache.

**Rollback:** Keep only a route-only bare `offline` alias if fixtures need it;
never restore arbitrary-model catalog publication.

### 5. CLI and refresh removal

**RED:** `cli_args.rs` rejects `models --refresh`; help omits it. `models_cmd`
tests cover canonical rows, filters, blank/duplicate normalization, local row,
and no fallback-string construction.

**GREEN:** Remove `refresh` from `Command::Models`, `main.rs`, and
`models_cmd.rs`; list `runtime.router.catalog()`/snapshot rows and await the
same startup composition.

**Rollback:** Revert the whole CLI slice only; never retain a silently ignored
flag.

### 6. Server/catalog DTO and bootstrap projection

**RED:** Update `crates/hya-server/tests/compat_provider_model_api.rs`,
`compat_provider_model_catalog_api.rs`, `compat_catalog_location_api.rs`, and
`tui_bootstrap_api.rs` to assert no active-agent synthetic row, exactly local
row in no-live runtime, zero-row declared provider statuses, auth split, shared
rows/defaults, location preservation, and no synthetic health/connection claim.

**GREEN:** Remove fallback in `compat/catalog.rs`; expose engine snapshot/status;
add additive source/status fields in `compat/catalog/types.rs`; carry them via
`compat/tui.rs::bootstrap`. Keep Compat `/config` patch state separate.

**Rollback:** Additive DTO fields may revert, but the active-agent fallback must
not return.

### 7. SDK and TUI consumers

**RED:** SDK tests cover empty/explicit/discovered/local rows, variants, and
HTTP/native parity. TUI tests cover stale/missing Recent/Favorite/provider keys,
unknown session model selection, no-live selector, local-only notice, and
variant filtering.

**GREEN:** Keep SDK tuple and pending forwarding. Update TUI sync bundle/multi-call
projection, local validation/display, `DialogModel`, `useConnected`, and prompt
usage paths. Treat source/status fields as additive and cache as non-authoritative.

**Rollback:** Revert rendering only if client schema support lags; keep server
row validation and optional fields.

### 8. Session/category/Workflow non-leakage

**RED:** Extend `compat_session_legacy_message_model_api.rs`, session suites,
`crates/hya-core/tests/model_fallback.rs`, `model_selection.rs`,
`crates/hya-e2e/tests/p19_workflow_model_routing.rs`, and Workflow tests:
unknown refs round-trip but stay out of catalog/TUI; unknown use yields typed
failure unless existing category/Workflow fallback succeeds; category/Workflow
candidates never become rows; valid failover and route outcomes stay unchanged.

**GREEN:** Keep proto events/projection and model-ref parsing wire-compatible;
ensure the existing `resolve_servable`, `apply_spawn_model_policy`,
`resolve_agent`, and `resolve_model_route` use router claims only.

**Rollback:** No event-schema rollback is needed because schemas must not change;
revert any attempted eager API validation that breaks replay.

### 9. Import and OAuth boundaries

**RED:** Isolated `compat_agent_cli.rs`/config tests place a candidate external
config and assert first run does not read/import it. Explicit import tests remain.
OAuth tests assert empty/error responses do not persist guessed ids/defaults.

**GREEN:** Strip external candidate calls from `first_run_config_bootstrap`,
retain `import_compat_models_into_config` and `crates/hya-ts/src/main.rs` command,
and remove OAuth empty/error fallback persistence while retaining Hya auth.

**Rollback:** Import removal and OAuth cleanup are separate commits; never roll
back by reintroducing automatic external-config reads.

### 10. Docs/spec/release

After implementation gates pass, update `docs/cli.md`,
`docs/configuration.md`, `docs/architecture/providers.md`,
`docs/architecture/server-client.md`, `docs/architecture/runtime.md`, and
`docs/architecture/tui.md` for startup discovery, optional auth/status split,
source semantics, local row, stale cache, and explicit import. Update relevant
`.trellis/spec/backend/{error-handling,logging-guidelines,quality-guidelines}.md`
and `.trellis/spec/frontend/{state-management,type-safety}.md` plus indexes.
Update `CHANGELOG.md` and `docs/changes/CHANGELOG_<version>.md` with startup
resolution, `--refresh` removal, import boundary, auth statuses, and offline row.
Workspace version is currently `0.36.7` in `Cargo.toml`; do not hand-bump during
implementation. If that release is already cut, use release tooling to bump
according to the project's 0.x minor-version policy and regenerate
`Cargo.lock`.

## Acceptance matrix and gates

- Explicit model route: exact normalized rows, zero list requests, with or
  without credential; credentialless request carries no auth header.
- Empty model route: one bounded request per startup, two starts make two
  requests, no config write, only declared endpoint/headers.
- Status split: no-credential 401/403 is `auth_required`; credentialed is
  `auth_rejected`; neither creates a row; all other failures retain safe status.
- Surfaces: CLI, all catalog endpoints, bootstrap, SDK, and TUI have identical
  row sets/variants; declared zero-row statuses remain visible where provider
  metadata is exposed; no status claims health/entitlement.
- Offline: exactly `hya/offline` and local metadata when no live rows; response
  echoes input plus configuration notice; absent whenever a live row exists.
- Stale/unknown: Recent/Favorite/cache and unknown Session models are hidden;
  unknown use follows existing typed error; categories/Workflow remain local
  routing policy; reasoning variants remain metadata, not rows.
- CLI: `models --refresh` is rejected and absent from help/docs.

Run gates only after implementation (not in this research task): focused provider
and app tests; backend/server/SDK tests; TUI type-check/interaction tests;
session/category/Workflow/e2e tests; format/lint/type-check; workspace tests; and
an actual isolated-backend smoke that observes startup request count, catalog,
and `hya/offline` echo. Review logs/headers for credential, body, account-id,
and external-config leakage.

## Rollback summary

Use one revertable commit per slice. Revert composition and all callers together;
revert additive DTO rendering separately but never restore active-agent fallback;
keep route-only `offline` alias rather than arbitrary rows; keep explicit import
separate from automatic startup; and revert docs/release notes with their code
contract. No rollback may reintroduce a second authority, stale cache, cross-agent
scan, or arbitrary active/default catalog row.

## Evidence

Exact current seams are in `config.rs:1235-1356,1458-1604`,
`runtime.rs:89-95,1258-1368,4532-4548`, provider `lib.rs:194-204,486-517`,
`router.rs:53-109`, `http.rs:174-240,749-801`, `dev.rs:53-80`, backend
`models_cmd.rs:5-56`, `cli_args.rs:141-152,339-352`, server
`compat/catalog.rs:161-229` and `compat/catalog/types.rs:46-173`, SDK
`client.rs:86-91,451-494`/`pending.rs:122-123`, and TUI
`sync.tsx:443-580`, `local.tsx:61-64,196-243,319-404`,
`dialog-model.tsx:11-145`, `use-connected.tsx:1-7`. Category/Workflow/session
and import/OAuth boundaries are detailed in the completed catalog surface audit.
