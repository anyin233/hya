# Implementation plan: model catalog endpoint discovery

## Execution rules

This is one cross-layer feature. Implement only after the user reviews all task
artifacts and `task.py start` changes the task to `in_progress`.

For every slice:

1. add the smallest failing contract test;
2. run only its focused command and confirm RED fails for the missing behavior;
3. implement the smallest complete change;
4. rerun the same focused command and confirm GREEN;
5. refactor only after GREEN;
6. do not run formatters, workspace lint, or the full suite until the final gate;
7. do not commit partial feature work before the full required verification gate.

No slice may introduce a compatibility alias, empty-token auth, second catalog
vector, background refresh, cache, foreign-config fallback, active/default model
synthesis, or event-schema change.

## Preconditions

- The current workspace contains existing 0.36.7 Workflow changes in files this
  feature will also touch. Before product edits, those changes must be committed
  and pushed by their owning task, or this task must use an isolated worktree
  based on that committed state. Never stage or overwrite unrelated work.
- Confirm `.trellis/tasks/09-01-model-catalog-endpoint-discovery/task.json` is
  `in_progress` and load `prd.md`, `design.md`, `implement.md`, plus the curated
  `implement.jsonl` context.
- Run LSP references before changing each exported symbol:
  `ProviderModel`, `ProviderRouter::catalog`, `HttpProvider::new`,
  `config::load`, `resolve_runtime`, and `SessionEngine::provider_catalog`.
  Migrate every caller in the same cutover; do not leave deprecated aliases.
- Record the initial focused test commands and observed RED failures in the task
  evidence directory. Never record credentials, auth headers, response bodies,
  home-directory config content, or unbounded logs.

## Slice 1 — Canonical catalog and offline contracts

### RED

Add focused `hya-provider` tests for:

- a snapshot containing ordered declared-provider states even when a provider has
  zero model rows;
- deterministic provider/model sorting and exact deduplication;
- configured, discovered, and offline model provenance;
- a row-backed default that must be a snapshot member;
- exactly one local `hya/offline` row when the live set is empty;
- no local row when any live row exists;
- `DevProvider` claims and publishes only `hya/offline`;
- another unknown model does not resolve to `DevProvider`;
- offline output echoes non-empty input and includes the configure-provider
  notice; empty input still includes the notice.

Run the new focused test target and confirm failures against the current
`ProviderModel`/`DevProvider` behavior.

### GREEN

- Add shared `ProviderCatalogSnapshot`, `ProviderCatalogState`, source/auth/result
  enums, and offline notice metadata in `hya-provider`.
- Extend `ProviderModel` with the catalog metadata required by the snapshot,
  including reasoning default and source.
- Keep snapshot mutation private; return borrowed slices.
- Add a canonical snapshot builder that inserts `hya/offline` only after the
  aggregate live model list is empty.
- Change `DevProvider::id`, `capabilities`, `catalog`, configured identity, and
  reply text to the exact `hya/offline` contract.
- Do not retain bare `offline`, `dev/*`, or arbitrary-model route aliases.

### Focused check

```sh
cargo test -p hya-provider catalog
cargo test -p hya-provider dev_provider
```

### Rollback point

The type/local-provider change is reversible before app composition. A rollback
must restore compilation but must not add an arbitrary active/default catalog
row.

## Slice 2 — Optional-auth HTTP provider routes

### RED

Extend provider HTTP tests with a local request recorder for every auth style:

- credentialless OpenAI Chat/Responses requests omit `Authorization`;
- credentialless Anthropic requests omit `x-api-key` but retain
  `anthropic-version`;
- credentialless Google requests omit `x-goog-api-key`;
- credentialless Codex/Grok requests omit bearer, account, and auth-session
  headers while retaining required non-secret protocol/client headers;
- explicit configured models remain routable without a credential;
- no request contains an empty auth header;
- configured provider identity distinguishes authenticated and anonymous routes
  without serializing a secret.

Confirm RED before constructor/auth changes.

### GREEN

- Change `HttpProvider::new` to accept explicit credential absence and migrate all
  LSP-reported callers.
- Represent optional secrets inside every provider-kind `AuthStyle`; never use an
  empty `SecretString` as absence.
- Make auth/header construction omit credential-derived headers when absent.
- Attach OAuth bearer resolvers and refresh hooks only for a matching Hya OAuth
  credential.
- Keep completion retry, pre-stream failover, and established-stream behavior
  unchanged.

### Focused check

```sh
cargo test -p hya-provider http_headers
cargo test -p hya-provider http
```

### Rollback point

Revert the constructor and all callers together. Never roll back by restoring an
empty token.

## Slice 3 — Bounded provider catalog discovery

### RED

Add parser and injected/local-HTTP tests for all six provider kinds:

- OpenAI `data[].id`, path-prefix-safe `/models`, no `/responses/models`;
- Anthropic `data[].id`, cursor pagination, malformed/looping cursor rejection;
- Google `models[].name`, `generateContent` filter, `models/` prefix removal,
  page-token pagination;
- Codex and Grok Build existing Hya model-list shapes and non-secret headers;
- credentialless requests for every safe provider adapter;
- no-credential 401/403 -> `AuthRequired`;
- credentialed 401/403 -> `AuthRejected`;
- valid empty -> `Empty`;
- invalid URL, redirect, timeout, 404/405, 429/5xx, malformed JSON, wrong schema,
  oversized body, page/model cap -> typed failure and no partial rows;
- trim blank IDs, preserve spelling/case, exact first-seen deduplication;
- no aliases, defaults, provider IDs, or name-prefix guesses become model rows;
- provider-local failure does not affect another outcome.

Use test-specific small limits to prove each bound without slow sleeps. Confirm
RED before adding the module.

### GREEN

- Add the deep discovery module in `hya-provider`.
- Reuse one optional-auth/header implementation for completion and discovery;
  do not copy secret handling into `hya-app`.
- Implement the provider adapter matrix from `design.md`.
- Use redirects disabled, safe URL parsing, bounded body/page/model counts, no
  retry/backoff, and a bounded concurrency/batch policy.
- Return typed outcomes only. Do not accept a fallback model parameter.
- Refactor existing OAuth model parsing to call the same provider adapters; Hya
  OAuth credential acquisition/refresh remains in `hya-app`.

### Focused check

```sh
cargo test -p hya-provider catalog_discovery
cargo test -p hya-app oauth::models_catalog
```

### Rollback point

Discovery can be reverted before async composition because no result is
persisted. Explicit routes must remain usable.

## Slice 4 — Async Hya config/runtime composition

### RED

Add `hya-app` composition tests using an injected transport or local server:

- non-empty explicit list: normalized exact rows, zero requests, authenticated
  or anonymous route;
- absent/empty/only-blank list: one discovery sequence per startup;
- two runtime starts: two requests and byte-identical Hya config file;
- no credential: no auth header, successful rows are routable;
- 401/403 auth split is retained in provider state;
- empty/failure/unsupported provider: zero rows but one declared-provider state;
- mixed providers: one failure does not remove valid explicit/discovered rows;
- router membership equals snapshot membership;
- configured reasoning variants/default remain on explicit rows;
- discovered rows use only established kind-level reasoning metadata;
- reserved `hya/offline` configuration collision is rejected;
- stale configured default does not create a row; deterministic live first row is
  selected;
- all-zero-live composition selects the canonical offline snapshot;
- config parse/composition error uses offline with strict permission behavior.

Confirm RED against synchronous `load`/`resolve_runtime`.

### GREEN

- Preserve empty provider declarations in `resolve_providers` and resolve optional
  credentials without credential-gating the plan.
- Move explicit model normalization before the discovery decision.
- Change `config::load` and `resolve_runtime` to the single async composition
  path; migrate every LSP-reported backend/app caller to `await` it.
- Discover eligible providers concurrently with the fixed bound and commit the
  snapshot once all outcomes settle.
- Build `HttpProvider` routes only from final normalized non-empty IDs.
- Remove `ModelEntry`, `model_entries`, `ResolvedConfig.models`,
  `RuntimeConfig.models`, and `has_providers` after all consumers migrate.
- Store `Arc<ProviderCatalogSnapshot>` in `RuntimeConfig`.
- Keep explicit `--model`/environment/Session overrides out of catalog
  membership; unknown use follows the existing typed route.
- Emit only safe provider/outcome diagnostics.

### Focused check

```sh
cargo test -p hya-app config::tests
cargo test -p hya-app runtime::tests
```

### Rollback point

Revert async resolution and every startup caller as one unit. Do not leave CLI and
server on different catalog sources.

## Slice 5 — Automatic import and OAuth persistence removal

### RED

Add tests that:

- place a valid Compat/OpenCode config at each automatic candidate path and prove
  normal first run/startup neither opens nor imports it;
- Hya-only first-run starter creation still works;
- explicit `hya --import compat <selected-source>` still imports and later
  startup reads only Hya config;
- OAuth login retains credential/provider metadata;
- OAuth success does not persist a fetched model list into an empty provider;
- OAuth empty/error does not persist a guessed model or default;
- OAuth login does not overwrite an existing non-empty user model list;
- an empty OAuth provider list remains empty and is discovered on next startup.

Confirm the current automatic import/guessed persistence assertions fail.

### GREEN

- Remove Compat/OpenCode candidate enumeration from
  `first_run_config_bootstrap` and normal `hya-ts` startup.
- Retain the explicit import command and migration helpers only on that call
  path.
- Change OAuth upsert/login behavior to avoid fetched/guessed model persistence
  for empty lists and preserve existing explicit lists.
- Keep Hya auth storage, token refresh, and provider metadata.
- Do not delete explicit maintenance-only Compat tooling outside runtime.

### Focused check

```sh
cargo test -p hya-app first_run
cargo test -p hya-app import_compat
cargo test -p hya-app oauth
cargo test -p hya-ts import
```

### Rollback point

Automatic-import removal and OAuth persistence cleanup are separable. Neither
rollback may restore a runtime foreign-config read or guessed model row.

## Slice 6 — Engine snapshot and routing compatibility

### RED

Add focused core/app tests for:

- engine catalog accessor returns the immutable snapshot and provider states;
- production app builder passes the exact runtime snapshot;
- engine test constructors derive a configured snapshot from their router;
- no server-facing model vector is allocated/rebuilt on every read;
- unknown model refs round-trip in Session events but stay out of catalog;
- unknown use returns the existing typed `UnknownModel` unless an existing
  ordered fallback succeeds;
- category candidates never become rows and preserve order;
- Workflow preferred/fallback/reasoning routes never become rows and preserve
  pre-stream failover/outcome behavior;
- `#variant` remains metadata, not a model row.

Confirm RED before changing `SessionEngine`.

### GREEN

- Add the snapshot field/builder/accessors to `SessionEngine` and migrate all LSP
  references.
- Use borrowed slices for catalog reads.
- Wire `RuntimeConfig.catalog` through the central `build_session_engine` path.
- Keep `Event`, projection, Session state, category, and Workflow wire schemas
  unchanged.
- Do not add eager Session model validation that breaks replay compatibility.

### Focused check

```sh
cargo test -p hya-core provider_catalog
cargo test -p hya-core model_fallback
cargo test -p hya-app workflow
```

### Rollback point

No event/database migration exists. Revert the engine field and builder wiring
together; keep provider/router behavior intact.

## Slice 7 — Server catalog and bootstrap projection

### RED

Update/add route tests in:

- `compat_provider_model_api.rs`;
- `compat_provider_model_catalog_api.rs`;
- `compat_catalog_location_api.rs`;
- `tui_bootstrap_api.rs`.

Required assertions:

- Fake/empty router plus arbitrary `AgentSpec.model` does not publish that model;
- all-zero-live runtime publishes exactly `hya/offline`;
- any live row suppresses offline;
- configured/discovered rows and variants are identical across every route;
- declared zero-row providers remain visible with empty model maps;
- no-auth 401/403 state is `auth_required`; credentialed is `auth_rejected`;
- source/auth/result fields contain no secrets or raw failure body;
- model status is configured/discovered/offline, not unconditional active;
- legacy `connected` excludes explicit-unprobed, failed, auth-required,
  auth-rejected, and offline providers;
- `provider_get` returns a declared zero-row provider and still returns typed 404
  for an undeclared provider;
- location wrapper fields remain unchanged;
- bootstrap carries the row-backed process default and same provider states.

Confirm RED against `catalog_models` active-agent fallback.

### GREEN

- Remove `model_ref_parts(&st.agent.model)` fallback from `catalog_models`.
- Project `SessionEngine` snapshot directly in all legacy/v2/bootstrap routes.
- Build provider DTOs from declared provider states, not reverse-grouped models.
- Add source/auth/result metadata in `compat/catalog/types.rs` and preserve
  variants' insertion order.
- Keep Compat process-local `/config` PATCH state separate from catalog
  authority; add only the row-backed default needed by bootstrap.
- Derive `/provider/auth` from declared auth-required/configured provider state,
  not model rows.

### Focused check

```sh
cargo test -p hya-server --test compat_provider_model_api
cargo test -p hya-server --test compat_provider_model_catalog_api
cargo test -p hya-server --test compat_catalog_location_api
cargo test -p hya-server --test tui_bootstrap_api
```

### Rollback point

Additive metadata can be reverted if a client incompatibility appears, but the
active/default synthetic model fallback must not return.

## Slice 8 — Backend models CLI and refresh removal

### RED

Add CLI/handler tests for:

- `models --refresh` rejected as an unknown argument;
- `models --help` has no refresh option;
- normal and verbose output use identical snapshot IDs;
- provider filter returns only that provider's real rows;
- explicit/discovered/offline rows format deterministically;
- a missing provider returns the existing typed/user-facing error;
- no fallback string is formatted as `hya/<active>`;
- each `models` process invocation runs the normal startup resolver.

Confirm RED against the accepted ignored flag and fallback formatting.

### GREEN

- Remove `refresh` from `Command::Models`, matching code, and tests.
- Remove `_refresh` and `fallback_model` from `cmd_models`/`model_lines`.
- Await the shared runtime composition in `main.rs`.
- Format snapshot rows and safe source/status metadata only.

### Focused check

```sh
cargo test -p hya-backend cli_args::tests::models
cargo test -p hya-backend models_cmd::tests
```

### Rollback point

Revert parser and handler together. Never retain an accepted no-op flag.

## Slice 9 — Rust SDK contract

### RED

Add SDK tests over local HTTP/native transports for:

- explicit and discovered rows;
- offline-only rows;
- variants and stable tuple positions;
- no row synthesized from active Session metadata;
- HTTP and in-process transports return the same IDs;
- provider metadata additions do not make `Client::models` drop configured,
  discovered, or offline rows.

Confirm RED where existing tests depend on synthetic server rows or lack local
metadata.

### GREEN

- Keep `Client::models` and `PendingClient::models` public tuple contracts.
- Parse the shared server snapshot only.
- Add a metadata method only if an existing provider response cannot expose the
  status required by consumers; do not add a second catalog call path.

### Focused check

```sh
cargo test -p hya-sdk models
cargo test -p hya-native model
```

### Rollback point

SDK metadata is additive. Tuple membership must continue to match the server.

## Slice 10 — TypeScript TUI catalog and status presentation

### RED

Add `packages/hya-tui-ts/test/model-catalog.test.ts` and extend the existing
real-backend/PTY seam for:

- bootstrap bundle and multi-call fallback produce identical provider/model
  state;
- configured, discovered, auth-required, auth-rejected, unavailable, and offline
  provider states decode without casts in components;
- Recent/Favorite/variant entries missing from the snapshot remain hidden;
- an unknown Session model does not repopulate the selector;
- local row appears only when the backend supplies `hya/offline`;
- provider/connect UI does not use provider-array length as health;
- auth-required and auth-rejected text is clear without relying on color;
- selecting offline permits prompt submission and renders the configure-provider
  echo notice;
- a live catalog has no offline row;
- first selection uses the backend's row-backed default.

Confirm RED before state/component changes.

### GREEN

- Add one Hya-owned typed decoder/projection in `src/hya/` for additive catalog
  metadata.
- Reuse `sync.tsx` bootstrap bundle and multi-call paths; do not add a client.
- Update `local.tsx`, `DialogModel`, `useConnected`, and prompt presentation to
  consume the decoded snapshot.
- Preserve existing storage format; stale entries remain fail-closed rather than
  being deleted or reintroduced.
- Keep model dialog readable at 80 columns and labels understandable without
  color.

### Focused check

Run from `packages/hya-tui-ts`:

```sh
bun test test/model-catalog.test.ts
bun run typecheck
```

### Rollback point

Rendering changes can roll back independently only if server membership and
fail-closed selection remain. Do not restore TUI-side synthesis.

## Slice 11 — Process and regression proof

### RED

Add a process-level test, preferably
`crates/hya-e2e/tests/p20_model_catalog_discovery.rs`, with a bounded local mock
catalog/inference server and temporary Hya config/auth directories. The contract
must fail against the old behavior and cover:

1. explicit models cause zero list requests;
2. empty models cause one list request per backend process;
3. two `hya-backend models` invocations cause two requests and no config write;
4. credentialless success sends no auth and produces routable rows;
5. credentialless 401 produces `auth_required`, no remote row, and canonical
   offline;
6. credentialed 403 produces `auth_rejected`, no remote row, and canonical
   offline;
7. one failed provider does not remove a valid provider;
8. CLI IDs equal `/api/model`, provider routes, and `/tui/bootstrap` IDs;
9. unknown Session/category/Workflow refs never appear in the catalog;
10. `hya/offline` exec echoes input and tells the user to configure a provider;
11. normal startup never opens a prepared foreign config sentinel.

Use bounded request counters/checksums and safe status evidence only.

### GREEN

Wire only missing integration seams. Do not add production behavior solely for
the test.

### Focused check

```sh
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p20_model_catalog_discovery -- --test-threads=1
```

Then build the actual frontend/backend binaries and use the existing PTY/TUI
harness to:

- open the model picker against a discovered catalog and observe only real rows;
- open it against an auth-required endpoint and observe `hya/offline` plus the
  auth/configuration notice;
- submit one offline prompt and observe the echo/configure response.

Record only model IDs, safe statuses, request counts, timestamps, and binary
checksums.

### Rollback point

The process test is permanent contract coverage. If integration fails, fix the
owning source slice; never weaken the assertions to accept synthetic rows.

## Slice 12 — Documentation, specs, version, and changelog

Do this only after behavioral tests and the actual-surface smoke pass.

### Documentation

Update:

- `docs/configuration.md`: explicit-list authority, empty-list startup discovery,
  optional auth, provider status meanings, offline row/notice, OAuth persistence,
  and explicit import boundary;
- `docs/cli.md`: remove `--refresh`; document that each `models` invocation builds
  a fresh snapshot;
- `docs/architecture/providers.md`: one catalog snapshot, adapter matrix, auth,
  limits, and router membership;
- `docs/architecture/server-client.md`: shared snapshot DTO and no synthetic
  health/default row;
- applicable runtime/TUI architecture pages for startup and selection flow;
- `docs/compat-parity.md` if additive provider fields or `connected` semantics
  change the documented Compat surface.

### Trellis specs

Use `trellis-update-spec` after implementation evidence. Capture executable
contracts in:

- backend error/logging/quality guidance for typed provider-local discovery,
  secret-safe failures, and one immutable snapshot;
- frontend state/type/quality guidance for typed catalog decoding, fail-closed
  stale cache, and no health inference;
- indexes that reference the new scenario documents.

Do not copy the task plan into specs; record only durable project conventions.

### Release metadata

- Move current root `CHANGELOG.md` to
  `docs/changes/CHANGELOG_0.36.7.md`.
- Bump `[workspace.package].version` from `0.36.7` to `0.36.8`.
- Write a root-only `CHANGELOG.md` for `0.36.8` covering startup discovery,
  credentialless/auth-required status, canonical offline model, foreign-config
  boundary, shared surfaces, OAuth persistence cleanup, and `--refresh` removal.
- Let the normal Cargo build update `Cargo.lock`; do not hand-edit the lockfile.
- Do not create a tag or publish a release unless the user separately asks.

## Final quality gate

Dispatch `trellis-check` after all code/docs changes. Fix every confirmed
spec/contract issue, then run the full project gates once:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
cargo build -p hya -p hya-ts -p hya-backend --bins
cargo test -p hya-e2e -- --test-threads=1
```

From `packages/hya-tui-ts`:

```sh
bun run typecheck
bun test
bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
```

Repeat the actual binary/TUI smoke with the final debug binaries. Verification
claims must name the exact commands and observed behavior. A test-only run is not
sufficient for the TUI or CLI surface.

## Security and privacy review

Before commit, inspect focused evidence and logs for:

- Authorization/API-key/account/session headers;
- credential values or presence details beyond the safe auth state;
- response bodies or unbounded upstream error text;
- user home paths or foreign config contents;
- cross-origin redirects;
- unbounded pagination/body/startup waits;
- a cache or config write caused by discovery.

Run the repository's applicable security/release checks after deterministic and
process proof. Do not include secrets or provider response bodies in task
evidence, changelog, or commits.

## Commit and push

Only after every required gate passes:

1. confirm the 0.36.7 baseline changes are already owned by their prior commit;
2. review the feature diff and stage only files from this task;
3. create one atomic semantic commit, recommended:
   `feat: discover provider model catalogs at startup`;
4. push the current branch;
5. record the commit hash, push result, verification commands, and safe smoke
   evidence in the task journal;
6. archive/finish the Trellis task through the normal finish-work flow.

If any required gate fails, do not commit or push. Report the exact failing
command and blocker.

## Final acceptance checklist

- [ ] Explicit normalized lists make zero model-list requests.
- [ ] Empty lists fetch once per startup and never mutate config/cache.
- [ ] Credentialless discovery/inference sends no auth headers.
- [ ] 401/403 status split is `auth_required` vs `auth_rejected`.
- [ ] Every declared provider retains safe startup status even with zero rows.
- [ ] Router claims equal snapshot model membership.
- [ ] CLI, all catalog APIs, bootstrap, SDK, and TUI expose the same rows.
- [ ] No active/default/Session/category/Workflow/cache value creates a row.
- [ ] `hya/offline` is exact, local, echoing, visible only with zero live rows.
- [ ] Unknown Session model replay and existing typed routing remain compatible.
- [ ] Category/Workflow/reasoning/failover behavior remains unchanged.
- [ ] Automatic foreign-config import is removed; explicit import still works.
- [ ] OAuth login writes no fetched/guessed list into an empty provider.
- [ ] `models --refresh` is rejected and absent from docs/help.
- [ ] Docs/specs/version/changelog describe 0.36.8 consistently.
- [ ] Focused RED/GREEN evidence, full gates, final binaries, and actual TUI/CLI
  smoke all pass before commit/push.
