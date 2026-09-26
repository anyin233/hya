# Quality Guidelines

> Code quality standards for backend development.

---

## Overview

This guide records source-backed backend quality contracts. Each scenario names
its trigger, executable signatures, invariants, failure cases, required tests,
and unsafe alternatives. Apply the narrow scenarios that own the changed
boundary; do not replace them with generic checklist prose.

---

## Scenario: Non-blocking prompt admission routes

### 1. Scope / Trigger

- Trigger: any HTTP/API route that admits a prompt or shell turn and starts optional model-side work such as auto-title, summarization, compaction, or background metadata generation.
- Applies to the `hya.v1` turn admission rpc (`POST /v1/sessions/{session}/turns` with a `prompt`/`command`/`shell` body) and future admission-style routes that must acknowledge work before the provider stream completes.

### 2. Signatures

- Route shape: `POST /v1/sessions/{session}/turns` (Turn `CreateTurn` with a `prompt`/`command`/`shell` oneof body) and equivalent admission endpoints.
- Core sequence: parse session ID, validate/load session, durably admit the user request, schedule optional follow-up work, return the admission response.

### 3. Contracts

- The route may await storage/projection work required to admit the request.
- The route must not await optional provider streams before responding.
- Auto-title and similar optional follow-up work must run in a background task or be driven by a separate worker path.
- The event log remains authoritative: background follow-up state changes must still write normal events such as `SessionTitled` through `SessionEngine` helpers.

### 4. Validation & Error Matrix

- Invalid session ID -> typed bad-request/not-found response before any background work starts.
- Admission/storage failure -> route returns the admission error and does not claim the prompt was accepted.
- Optional provider/title/summarizer hangs -> prompt route still returns after admission; only the optional follow-up remains pending.
- Optional follow-up failure -> do not fail the already-returned admission response; surface through logs/events only if an owning error path exists.

### 5. Good/Base/Bad Cases

- Good: a pending title provider cannot block `POST /v1/sessions/{session}/turns`; transcript reads can still show the admitted unfinished assistant state.
- Base: title generation eventually writes `SessionTitled` after the prompt response when the provider completes.
- Bad: awaiting `auto_title_session(...)` or another optional provider call inside the prompt handler before sending the HTTP response.

### 6. Tests Required

- Add a route-level regression with a fake provider whose optional follow-up future never resolves; assert the prompt route returns within a bounded timeout.
- Assert list/context APIs still reflect durable admission state while unfinished assistant/provider work has no completion timestamp.
- For eventual background results, poll a condition with a bounded timeout instead of asserting immediate replacement.

### 7. Wrong vs Correct

#### Wrong

```rust
admit_prompt(&state, session, request).await?;
state.engine.auto_title_session(session, model).await?;
Ok(Json(response))
```

#### Correct

```rust
let response = admit_prompt(&state, session, request).await?;
let engine = state.engine.clone();
tokio::spawn(async move {
    let _ = engine.auto_title_session(session, model).await;
});
Ok(Json(response))
```

---

## Scenario: v1 permission and question interactions

### 1. Scope / Trigger

- Trigger: changes to the v1 interaction routes, pending interaction storage,
  or interaction event serialization on the streams.
- The v1 surface unifies permissions and questions into one Interactions
  service (`crates/hya-server/src/v1/interaction.rs`); there are no separate
  root permission/question queues anymore.

### 2. Signatures

- `GET /v1/interactions` (optional `type=INTERACTION_TYPE_PERMISSION` /
  `INTERACTION_TYPE_QUESTION` and `session` filters) -> `Interaction` rows
  with `id`, `session`, `type`, `title` (`"<action> <resource>"` for
  permissions, the question text for questions).
- `POST /v1/interactions/{request}/respond` with one of
  `{ "permission": { "allowed": bool, "persist": bool } }`,
  `{ "question": { "answer": "..." } }`, or
  `{ "question": { "rejected": true } }`; the response is
  `{ "applied": bool }`.
- Stream events: `permissionRequested` / `questionRequested` (each carrying
  the request id plus an `Interaction` summary) and `interactionResolved`,
  delivered on `GET /v1/sessions/{session}/events/stream` and
  `GET /v1/events/stream` as `StreamFrame`s.

### 3. Contracts

- Pending insertion precedes the requested event. Pending removal plus
  successful reply-channel completion precedes `interactionResolved`.
- A respond call for an already-resolved request returns `applied: false`
  (idempotent replay) instead of an error; an unknown request id returns
  `not_found`.
- `persist: true` on an allowed permission additionally saves a durable
  saved-rule row (listed by `GET /v1/permissions/rules`); the in-process
  permission plane grant is what authorizes the current process.
- Question answers submit per-question option selections; rejected questions
  surface the rejection to the awaiting tool call.

### 4. Validation & Error Matrix

- Unknown or already-removed request id -> `not_found` (404/`NotFound`).
- Invalid session filter -> `invalid_argument`.
- Missing response oneof -> `invalid_argument` before any side effect.
- Successful respond -> `applied: true`, exactly one `interactionResolved`
  event, request absent from the next list response.

### 5. Good/Base/Bad Cases

- Good: a client lists pending permissions, responds
  `{permission:{allowed:true}}`, observes one `interactionResolved`, and a
  duplicate respond returns `applied: false`.
- Base: an empty pending set returns `[]` on both type filters.
- Bad: publishing `interactionResolved` before the reply channel succeeds,
  or answering a permission through the question oneof.

### 6. Tests Required

- Route tests (`crates/hya-server/tests/v1_api.rs`) assert the Interaction
  field sets, type/session filters, duplicate `applied: false`, and
  unknown-id `not_found`.
- Stream tests assert `permissionRequested`/`questionRequested`/
  `interactionResolved` ordering relative to pending insertion/removal.
- Process E2E (Track P `p03_permissions`, `p04_questions`) covers the
  once/reject and reply flows through the real binary.

### 7. Wrong vs Correct

#### Wrong

```json
{"question":{"answers":"Yes"}}
```

#### Correct

```json
{"question":{"answer":"Yes"}}
```

---

## Scenario: Session ID compatibility across routes and fixtures

### 1. Scope / Trigger

- Trigger: any v1 route, client URL builder, sync/projector path, or test fixture that accepts or emits a session ID.
- Applies to v1 path parameters (`{session}`) and body fields, client helpers, and test helpers that create sessions.

### 2. Contracts

- New sessions are identified by the server-returned `hysec_[A-Za-z0-9]{20}` string.
- Legacy `ses_<uuid-simple>` and raw UUIDs may be parsed only through the shared `SessionId` parser for compatibility.
- Route-local prefix checks such as `starts_with("ses")` are forbidden.
- Test fixtures must use the session ID returned by the API or `SessionId::to_string()`; they must not rebuild IDs with string formatting.
- Storage and replay code must use the shared storage/display contract instead of assuming UUID bytes.

### 3. Tests Required

- Creation tests assert the `hysec_` shape.
- Every route family that accepts a session ID should include at least one flow using the returned `hysec_` ID.
- Legacy parser coverage belongs in the shared ID/parser tests or explicit compatibility tests, not by rewriting new IDs into legacy-looking strings.

### 4. Wrong vs Correct

#### Wrong

```rust
if !payload.session_id.starts_with("ses") {
    return Err(ApiError::bad_request("invalid session id"));
}
let session = parse_session(&payload.session_id)?;
```

```rust
let session_id = format!("ses_{}", created_session.replace('-', ""));
```

#### Correct

```rust
let session = parse_session(&payload.session_id)?;
```

```rust
let session_id = created_session;
```

---

## Scenario: CLI session persistence through database-backed commands

### 1. Scope / Trigger

- Trigger: any `hya` command that creates, mutates, replays, lists, or serves sessions while accepting a SQLite database path.
- Applies to headless `exec` / `run`, `sessions`, `tail-session`, `serve`, and future CLI commands that share the event-sourced session store.

### 2. Signatures

- Headless execution: `hya --db <path> exec <prompt>` and `hya --db <path> run <prompt>`.
- Listing: `hya sessions --db <path>`.
- Server: `hya serve --db <path> --bind <addr>`.
- Empty `--db ""` remains the in-memory store mode; a non-empty path is a persistent SQLite store.

### 3. Contracts

- A command that receives a non-empty database path and emits session events must open that exact SQLite store before constructing `SessionEngine`.
- Headless `exec` / `run` output may render a transcript or JSONL stream, but the same events must be replayable from `sessions --db <path>` after the process exits.
- `serve --db <path>` and headless commands share the same `SessionStore` contract: `hysec_` IDs, projection replay, and list filtering all come from the SQLite event log.
- In-memory execution is allowed only when the effective DB path is empty; do not silently fall back to memory when a path is supplied.

### 4. Validation & Error Matrix

- Missing parent directory or invalid SQLite path -> command returns the store-open error and does not claim session persistence.
- `exec --db <path>` succeeds -> a subsequent `sessions --db <path>` lists the emitted `hysec_` session.
- `exec --json --db <path>` succeeds -> JSONL envelopes and persisted DB replay describe the same session ID.
- `serve --db <path>` prompt flow succeeds -> `sessions --db <path>` can list the same non-empty session after the HTTP request.

### 5. Good/Base/Bad Cases

- Good: `hya --db /tmp/hya.db exec "Say hello"` writes events to `/tmp/hya.db`, and `hya sessions --db /tmp/hya.db` prints the resulting `hysec_...` row.
- Base: omitting `--db` uses in-memory execution and does not leave a durable session after process exit.
- Bad: `exec` constructs `SessionStore::connect_memory()` even though the top-level CLI parsed `--db <path>`.

### 6. Tests Required

- Add a CLI integration regression that runs `hya --pure --db <tmp>/hya.db exec <prompt>` and then asserts `hya sessions --pure --db <tmp>/hya.db` contains `hysec_`.
- Manual QA should run a rendered `exec`, a JSONL `exec --json`, and `sessions --db` against the same DB to prove both output modes persist.
- HTTP QA should run `serve --db`, create/prompt a session, then list the same DB through the CLI.

### 7. Wrong vs Correct

#### Wrong

```rust
let store = SessionStore::connect_memory().await?;
let (engine, ..) = build_session_engine(store, router, &model, mcp, plugins).await;
```

#### Correct

```rust
let store = open_store(db).await?;
let (engine, ..) = build_session_engine(store, router, &model, mcp, plugins).await;
```

---

## Scenario: Immutable runtime generation and per-turn binding

### 1. Scope / Trigger

- Trigger: changing tool, skill, MCP, prompt-discovery, or runtime-refresh
  behavior visible to an admitted assistant or direct-shell turn.
- Applies to `hya-core::RuntimeRegistry`, `TurnBinding`, startup/deferred
  candidate construction in `hya-app`, and the lightweight binding event in
  `hya-proto`.

### 2. Signatures

- `RuntimeRegistry::bind_turn(workdir) -> Result<TurnBinding, RuntimeRefreshError>`.
- `RuntimeRegistry::refresh(|candidate| ...) -> Result<ConfigGeneration, RuntimeRefreshError>`.
- `Event::TurnBindingRecorded { session, message, generation }`.
- `MessageProjection.config_generation: Option<ConfigGeneration>`.

### 3. Contracts

- `ToolRegistry` is a mutable offline candidate builder. `SessionEngine`
  snapshots it at construction and owns the sole effective
  `RuntimeRegistry`.
- A successful admission binds exactly once before prompt discovery, provider
  schemas, or tool behavior. All rounds, resolution, dispatch, and skill-tool
  reads use that retained immutable snapshot.
- A refresh builds and validates a complete candidate under the single
  publication owner, then allocates the next generation and replaces one
  active `Arc`. In-flight bindings retain the prior `Arc`.
- Failed and logically unchanged candidates preserve both the active
  generation and exact view. Deferred MCP publishes its complete observed tool
  set and never mutates an engine-visible builder.
- The event stores generation identity only. Registry contents remain outside
  events/projections; existing permission and namespace behavior is unchanged.

### 4. Validation & Error Matrix

- Duplicate tool or invalid candidate -> typed refresh error; no publication
  and no generation consumption.
- Generation overflow -> `GenerationExhausted`; active snapshot unchanged.
- Concurrent successful refreshes -> unique monotonic generations and one
  complete final candidate, never a merged/partial view.
- No logical tool/skill change -> return the current generation.

### 5. Good/Base/Bad Cases

- Good: refresh between provider rounds leaves the current turn on generation
  N and makes the next turn observe the complete N+1 snapshot.
- Base: repeated discovery of the same workdir skill catalog is a no-op.
- Bad: retaining an `Arc<ToolRegistry>` in `SessionEngine` and registering one
  deferred MCP tool at a time into the effective view.

### 6. Tests Required

- Integration: an in-flight turn keeps old prompt skills, schemas, MCP/tools,
  and dispatch while the next turn sees the new complete view.
- Unit/integration: failed, no-op, and concurrent publications preserve the
  generation invariants.
- App wiring: mutating the retained initial builder is invisible and a
  deferred multi-tool candidate appears atomically.
- Event/replay: the binding event round-trips and folds only the generation
  identity; direct shell records one binding.

### 7. Wrong vs Correct

#### Wrong

```rust
let tools = Arc::new(ToolRegistry::builtins());
let engine = SessionEngine::new(..., tools.clone(), ...);
tools.register(deferred_tool)?;
```

#### Correct

```rust
let engine = SessionEngine::new(..., Arc::new(initial_candidate), ...);
engine.refresh_runtime(|candidate| {
    candidate.register_tool_with_permission(deferred_tool, ToolPermission::Mcp)
})?;
```

---

## Scenario: MCP/plugin desired-observed-effective reconciliation

### 1. Scope / Trigger

- Trigger: startup/deferred/v1 MCP control changes, startup plugin tool
  declarations, or plugin crash/respawn declaration validation.
- Applies to the app-owned reconciler, `RuntimeRegistry` source manifests,
  MCP preparation, plugin initialize validation, and the server's narrow MCP
  control trait.

### 2. Contracts

- `hya-app::RuntimeReconciler` owns desired revision/tickets and observed
  results only. It has no resolve/dispatch surface and no effective-tool cache.
- `RuntimeRegistry` remains the sole effective authority. A snapshot owns each
  source's client/child, declaration digest, resources, and tool exports.
- Source identity is `(mcp|plugin, configured_id)`. External tool names remain
  compatible; duplicate IDs, exports, canonical names, or aliases reject the
  complete candidate before generation allocation.
- Process I/O completes before reconciliation state is locked. Stale prepared
  successes are closed after releasing the state lock; stale failures are
  discarded.
- Current additions publish only when the whole revision succeeds. Failure
  records typed observed state, closes every unpublished success, and preserves
  the prior effective generation exactly.
- Explicit removal publishes a drop-only candidate before unrelated additions.
  Old `TurnBinding` snapshots retain their source owner until the last binding
  is dropped.
- Candidate publication always derives from the registry's current snapshot;
  it must not overwrite a newer skill or source publication with an old base.
- Plugin respawn compares a deterministic encoding of the full initialize
  declaration. Drift closes the replacement and future calls fail closed.
  This is not plugin hot reload; hooks and `PermissionPlane` remain unchanged.
- Server routes receive only a dependency-inverted MCP control trait. They own
  no manager, desired map, status map, or effective registry.
- `McpControl::tools` reads each MCP source's canonical exports from the
  effective manifest; `GET /v1/mcp` reports them only for a `Connected`
  status, so a stale or failed observation never shows tools it cannot call.

### 3. Required tests

- Stale success closes and cannot publish over a newer ticket.
- Explicit removal reaches the next binding despite unrelated connect failure;
  the old binding remains callable until dropped.
- Current partial failure closes unpublished owners and preserves generation.
- Duplicate source/export/canonical/alias and plugin handshake-ID mismatch fail
  before publication and consume no generation.
- Mixed MCP/plugin startup publishes one complete snapshot exactly once.
- v1 MCP add/connect/disconnect changes callability through the same registry.
- `GET /v1/mcp` lists a connected server's namespaced tools and none after
  disconnect (`hya-e2e` `p06_mcp`).
- Reordered equivalent plugin initialize declarations compare equal; changing
  tool, command/permission hook, or workspace declarations detects drift.
- Cargo manifests and `Cargo.lock` add no dependency for declaration hashing.

---

## Scenario: GitHub Release Binary Workflow

### 1. Scope / Trigger

- Trigger: any change that publishes release binaries, creates GitHub Releases, or modifies the release changelog process.
- Applies to `.github/workflows/release.yml`, root `CHANGELOG.md`, `docs/changes/`, root `AGENTS.md` release rules, and release-related task artifacts.

### 2. Signatures

- Release tag: `vX.Y.Z`, where `X.Y.Z` must match Cargo's `hya-backend` package version.
- Release targets (build job matrix): `x86_64-unknown-linux-gnu`
  (`ubuntu-22.04`), `aarch64-unknown-linux-gnu` (`ubuntu-22.04-arm`),
  `aarch64-apple-darwin` (`macos-15`).
- Cargo command per target: `cargo build --release --locked -p hya-backend --bins --target "$TARGET"`,
  plus the five tool-family libraries.
- Release archive per target: `hya-<version>-<target>.tar.gz`.
- Bundle assets: `hya-<name>-<version>-<target>.hyabundle` (native tool
  families, per target) and `hya-<name>-<version>.hyabundle` (the seven
  platform-independent first-party bundles, once).
- Checksum files: `SHA256SUMS-<target>` from each build job and a combined
  `SHA256SUMS` from the release job, written with `shasum -a 256`.
- Non-publishing rehearsal on a host of the rehearsed target (requires Bun
  `1.4.2`, `actionlint` `1.7.12`, 7-Zip `7z`, and `shasum` on `PATH`):

```sh
cargo run -p xtask -- release-rehearsal \
  --workflow .github/workflows/release.yml \
  --version <workspace version> \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --no-publish
```

### 3. Contracts

- Root `CHANGELOG.md` contains only the newest version's release notes.
- Historical changelogs live under `docs/changes/CHANGELOG_<version>.md`.
- The GitHub Release body is read verbatim from root `CHANGELOG.md`.
- Release workflow permissions are read-only by default; only the release publishing job may request `contents: write`.
- Build provenance attestations are generated for every archive, bundle asset,
  and checksum file.
- Third-party release actions are pinned to immutable commit SHAs.
- The publishing job uses the `release` environment so repository settings can require manual approval.
- Within the release archive, the payload includes the shipped `hya`
  binary, the twelve first-party bundles under `bundles/`, the production
  `lib/hya/bun-adapter`, and the generated member
  `examples/hya-argus-example.hyabundle`; it does not add `hya-updater`.
- Platform-independent bundles must be byte-identical across targets; the
  release job compares them before publishing.
  (The legacy frontend launcher/runtime payload was removed with the legacy
  TUI.)
- `scripts/package-argus-example.sh` generates that member from tracked source
  `bundles/examples/argus-example`; no root `examples/` artifact is an input.
- The rehearsal requires the explicit `--no-publish` guard, builds and packages
  in a temporary directory, and never creates a tag or GitHub Release.
- `release-rehearsal` owns the pinned `actionlint` and embedded-shell checks.
  The current CI workflow does not run `actionlint` as a separate gate.

### 4. Validation & Error Matrix

- Missing `v` tag prefix -> fail before build.
- Tag version is not semver-shaped -> fail before build.
- Tag version differs from `cargo metadata` package version for `hya-backend` -> fail before build.
- Missing or empty `CHANGELOG.md` -> fail before publishing.
- `CHANGELOG.md` first heading differs from the tag version -> fail before build.
- Build, archive, checksum, or packaged-binary smoke failure -> skip release publishing.
- Missing release assets -> fail `softprops/action-gh-release` with `fail_on_unmatched_files: true`.
- Missing `--no-publish` -> rehearsal rejects before validation or build.
- `actionlint` missing or not version `1.7.12`, or Bun not version `1.4.2` ->
  rehearsal fails its pinned prerequisite check.
- Bun adapter `bun.lock` written by a Bun newer than the pinned version ->
  rehearsal fails before build and names the lockfile version.
- Rehearsal target outside the build matrix, a matrix that differs from the
  supported targets, or a target other than the host -> rehearsal fails before
  build.
- Platform-independent bundle bytes differ between targets -> the release job
  fails before publishing.
- Missing Bun adapter runtime, Argus package, locked production dependency,
  first-party bundle, or archive member -> package/rehearsal smoke fails
  before publication.
- Missing adapter payload, locked production dependency, or archive member in
  the staged/extracted tree -> workflow validation fails closed and names the
  exact missing command before prerequisites or build.
- Installer staging lacks `node_modules` or a required runtime dependency ->
  fail before backups/swaps with the missing relative path; clean temporary
  staging and retain the previous install.

### 5. Good/Base/Bad Cases

- Good: `v0.1.0`, `[workspace.package].version = "0.1.0"`, root `CHANGELOG.md` contains only `0.1.0` notes, archive and checksum pass smoke checks.
- Base: first release has no historical changelog; keep `docs/changes/.gitkeep` and root `CHANGELOG.md` for the current version.
- Bad: appending old release notes to root `CHANGELOG.md`; this publishes stale history as the GitHub Release body.
- Good: the no-publish rehearsal validates the real workflow, exact payload,
  Compat adapter handshake, Argus package closure, and
  checksum without publishing.
- Base: a rehearsal uses temporary package/extract roots and leaves the source
  checkout and release provider untouched.
- Bad: validating only the binary while omitting the
  adapter or Argus archive from assertions.
- Bad: repairing an incomplete checked-in workflow inside a test fixture
  or accepting an empty `node_modules` directory.

### 6. Tests Required

- Parse workflow YAML, require `actionlint` `1.7.12`, and syntax-check every
  embedded shell `run` block.
- Run the tag/version/changelog validation logic with a representative tag and
  require the explicit `--no-publish` rehearsal guard.
- Run the release build command for the configured target.
- Package the `hya` binary and the production Compat adapter; verify
  `SHA256SUMS`, extract the archive, and run
  each binary smoke.
- Assert the Compat adapter's locked files and initialize/shutdown handshake.
- Generate `examples/hya-argus-example.hyabundle` inside the temporary package
  from `bundles/examples/argus-example`, then assert its canonical root closure.
- Confirm third-party actions are pinned to commit SHAs and release publication
  uses the `release` environment.

### 7. Wrong vs Correct

#### Wrong

```yaml
permissions: write-all
```

```markdown
# CHANGELOG

## 0.2.0
- New release.

## 0.1.0
- Old release.
```

#### Correct

```yaml
permissions:
  contents: read

jobs:
  release:
    permissions:
      contents: write
```

```markdown
# 0.2.0

- New release.
```

---

## Scenario: OpenAI Protocol Selection And Reasoning Replay

### 1. Scope / Trigger

- Trigger: changes to OpenAI-compatible provider configuration, model reasoning
  metadata, request encoding, stream decoding, or event replay.

### 2. Signatures

- Provider kinds: `openai-completion`, `openai-response`, and `grok-build`;
  `openai` and `openai-compatible` remain Chat Completions aliases.
- Model entries accept a string ID or
  `{ id, reasoning: { default?, variants? }, limit: { context?, output? } }`.
- A known per-model `limit.output` (configured or cached) is the default and
  ceiling for `CompletionRequest.max_output_tokens`, applied by `HttpProvider`
  before `Protocol::encode_with_output_limit`; without one the request is
  encoded unchanged (Anthropic's required `max_tokens` falls back to 4096).
- Provider behavior stays behind `Protocol::encode(CompletionRequest)` and a
  protocol-specific `Decoder` selected by `HttpProvider` construction.

### 3. Contracts

- Chat Completions posts to `/chat/completions`; Responses posts to `/responses`.
  The shared HTTP/SSE transport must not branch on API-specific payloads.
- Responses emits `instructions`, ordered `input` items, flat function tools,
  `store: false`, and `reasoning: { effort, summary: "auto" }`.
- Responses preserves `none`, `minimal`, `low`, `medium`, `high`, `xhigh`, and
  `max` on the wire. Chat omits `none` and maps `max` to `xhigh`.
- Grok Build uses `/responses` with Bearer authentication, adds only
  `include: ["reasoning.encrypted_content"]`, and advertises fallback efforts
  `low`, `medium`, and `high` in ascending order so `high` is the default.
- Both `response.reasoning_summary_text.delta` and
  `response.reasoning_text.delta` emit normalized reasoning events.
- Grok Build requires `response.completed` or `response.incomplete`; bare
  `[DONE]` and EOF are decode errors. Other Responses routes stay permissive.
- The selected model's configured default reaches the initial `AgentSpec`.
  Explicit Compat variants/options may override it per turn.
- Completed opaque Responses reasoning is stored in
  `ReasoningEnd.provider_data`, survives projection and fork replay, and is sent
  unchanged before the matching `function_call` and `function_call_output`.

### 4. Validation & Error Matrix

- Unknown provider kind -> configuration error.
- Unknown reasoning effort -> configuration error.
- Default effort absent from configured variants -> configuration error.
- `limit` not a mapping, an unknown `limit` key, a zero/negative/non-integer or
  over-`u32` value, or `output` above `context` -> configuration error.
- Legacy string model or Chat alias -> preserve existing Chat behavior.
- `response.failed` or top-level Responses `error` -> `ProviderError`.
- Grok Build transport termination without a typed terminal event ->
  `ProviderError::Decode`.

### 5. Good/Base/Bad Cases

- Good: a configured Responses model defaults to `max`, performs a stateless
  tool round, and replays its opaque reasoning item before the tool result.
- Base: `kind: openai` with string models still uses Chat Completions and its
  existing supported fallback.
- Bad: decoding Responses with the Chat decoder or retaining opaque reasoning
  only in process memory.

### 6. Tests Required

- Config tests assert string/object parsing, all effort labels, defaults,
  variants, aliases, and rejection cases.
- Runtime tests assert the selected default reaches the first agent and provider
  catalog metadata retains per-model variants.
- Local HTTP/SSE tests assert endpoint and JSON shape, ordered canonical events,
  parallel tool assembly, usage, failures, and stateless continuation.
- Grok Build HTTP/SSE tests assert encrypted reasoning inclusion, all fallback
  efforts, both reasoning delta names, typed completion, and truncated streams.
- Event/projection/core tests assert opaque reasoning survives serde, replay,
  request reconstruction, and session forks.

### 7. Wrong vs Correct

#### Wrong

```rust
// API-specific behavior leaks into the shared transport and replay drops state.
if endpoint.ends_with("/responses") {
    encode_responses_in_stream(&request)?;
}
```

#### Correct

```rust
let protocol: Arc<dyn Protocol> = Arc::new(OpenAiResponsesProtocol::new());
let body = protocol.encode(&request)?;
```

---

## Scenario: Tool Invocation And Resource Permissions

### 1. Scope / Trigger

- Trigger: changes to permission config, tool registration, model/direct-shell dispatch, permission asks, or headless execution.

### 2. Contracts

- Invocation policy and wildcard resource rules are separate layers. Do not convert path, URL, external-directory, or legacy action rules into invocation regexes.
- Registry metadata explicitly classifies canonical tools as read-only, task, standard tool, command, or MCP. Never infer MCP classification from a name prefix.
- Dispatch order is before-hook, successful registry lookup, post-hook input validation, one native authorization, then execution with the returned call-scoped plane. Unknown or malformed calls do not prompt.
- Native `AllowAlways` remembers one exact target/value subject; legacy `AllowAlways` remains action-wide. Effective denies and external-directory checks are not bypassed by a call grant.
- Server asks keep their existing interaction endpoints. Headless `exec`, RPC, and goal modes reject residual asks; `--yolo` sets the effective invocation model to `danger` before engine construction.

### 3. Tests Required

- Evaluator tests cover all models, ordered regex matching, defaults, and invalid regexes.
- Dispatch tests cover lookup-before-ask, post-hook command matching, call correlation, and one prompt per invocation.
- Permission-plane tests cover exact native grants, legacy action grants, deny precedence, and the external-directory exception.
- Config/runtime tests cover omission, permission-only offline config, strict malformed-config fallback, yolo override, and fail-closed headless asks.

---

## Scenario: Model Variant Selection And Agent Lifecycle Presentation

### 1. Scope / Trigger

- Trigger: changes to model-variant parsing on turn admission, projected
  model variant, TypeScript subagent observation lifetime, or lifecycle
  status rendering.

### 2. Signatures

- The v1 turn surface carries model selection as a
  `provider/model[#variant]` reference string: `CommandTurn.model` overrides
  the turn model, and `PATCH /v1/sessions/{session}` / `UpdateSession`
  switches the session's `model`.
- `resolveLifecyclePresentation(node)` returns a visible lifecycle `label` and
  a `working` flag from the existing member/roster projection.
- Observation panes close only through the workspace `close` action or
  `reconcileSessions` when the child session is absent.

### 3. Contracts

- Variant parsing follows the shared `ModelRef` decoder; a trimmed non-empty
  `#variant` segment overrides category/effort resolution before the provider
  request is built.
- Missing or empty variant segments preserve the configured/category variant.
  Unparseable model refs are rejected at admission, not silently defaulted.
- Lifecycle presentation prefers transient member status over roster status.
  `spawning`, `running`, and `busy` map to `Working`; `done` maps to `Finished`;
  `failed`, `cancelled`, and true idle remain distinct.
- Working rows show both visible text and the existing spinner. Terminal events
  update presentation but do not discard synchronized transcript content.
- Reasoning remains projection-backed; do not synthesize reasoning parts or add
  another lifecycle/message store.

### 4. Validation & Error Matrix

- Non-string or unparseable model ref -> admission-time invalid-argument
  before the turn starts.
- Whitespace-only variant segment -> preserve the configured variant.
- Member status present with stale roster `idle` -> render the member state.
- Session absent from successful reconciliation -> remove its observation pane.

### 5. Good/Base/Bad Cases

- Good: a `provider/model#high` command-turn override records `high` effort on
  the turn's provider request; clients preserve and label the finished
  observation.
- Base: category-configured models behave as before; an idle roster-only
  row displays `Idle` without a spinner.
- Bad: letting a missing variant clear effort, preferring roster `idle`
  over member `running`, or removing a pane solely because a child completed.

### 6. Tests Required

- Route integration tests assert model-ref parsing, variant precedence,
  invalid-ref rejection, response projection, and session model state.
- Workspace tests assert terminal observations survive completion and focus
  changes while explicit close and stale-session reconciliation still remove them.
- Lifecycle tests assert member precedence, every label, and each working flag;
  PTY coverage asserts visible `Working` text in the observation header.

### 7. Wrong vs Correct

#### Wrong

```typescript
const status = node.roster?.status ?? node.member?.status
dispatchWorkspace({ type: "terminal", sessionIDs: [node.session] })
```

#### Correct

```typescript
const lifecycle = resolveLifecyclePresentation(node)
// Completion changes lifecycle presentation; pane removal stays user- or reconciliation-owned.
```

---

## Scenario: Adding an Additive `Event` Variant

### 1. Scope / Trigger

- Trigger: adding a variant to `hya_proto::Event`, or a field to an existing variant.
- Applies to `crates/hya-proto/src/event.rs`, `projection.rs`, and every crate that
  matches `Event` exhaustively.

### 2. Signatures

- New fields use `#[serde(default)]` plus `skip_serializing_if` so an empty value
  never reaches the wire. Precedent: `MemberSpawned.agent_type`, `.mode`,
  `.directive`, `.tool_call`.
- `Event::Unknown` carries `#[serde(other)]`: an older binary folds newer variants
  instead of failing to replay.

### 3. Contracts

- Extend `Event::session()` so the variant reports its owning session, or `None`.
- Add the variant to the reducer's no-op arm in `projection.rs` when it is an
  observability record rather than a state transition. The reducer match is
  exhaustive by design — it fails the build rather than silently ignoring a
  variant, so never add a `_ =>` catch-all.
- A record-only variant must not change reduced state. It still advances
  `last_seq`; assert on `projection.session` / `.team`, not the whole `Projection`.
- Compile-driven site list (the build enumerates these; do not hand-maintain):
  `hya-core/src/engine/text_complete.rs` and the v1 curated-stream mapping in
  `hya-server/src/v1/convert.rs` (`stream_event`). A new variant that should
  reach clients also needs a `StreamEvent` payload mapping plus a reducer
  decision there.

### 4. Validation & Error Matrix

- Missing `Event::session()` arm -> non-exhaustive match, build fails.
- Missing reducer arm -> non-exhaustive match, build fails.
- New required field without `serde(default)` -> pre-existing logs fail to replay.
- Field serialized when empty -> wire drift against older consumers.

### 5. Good/Base/Bad Cases

- Good: variant added, `session()` + reducer no-op arm extended, round-trip test,
  and a test proving a pre-change payload still decodes and folds.
- Base: a variant with no reduced state; assert `before.session == after.session`.
- Bad: reusing an existing field for new meaning. `SessionCreated.parent` means
  *subagent lineage* and drives depth accounting, governor budgets, and team root —
  a fork must use `SessionForked`, not `parent`, or it corrupts the spawn tree.

### 6. Tests Required

- serde round-trip for the variant and any new enum.
- `session()` returns the expected owner.
- Replay proves reduced state is unchanged for record-only variants.
- Backward compatibility: encode with empty additions, assert the field names are
  absent from the JSON, then decode and fold it.

---

## Scenario: Workspace Version Bump

### 1. Scope / Trigger

- Trigger: any fix or feature change, per the root `AGENTS.md` release rule.

### 2. Contracts

Bumping the version means updating **all** of these together:

| File | What to change |
| --- | --- |
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | every `hya` / `hya-*` package version (a build refreshes it) |
| `README.md` | the `workspace version \`X.Y.Z\`` string |
| `CHANGELOG.md` | first heading is exactly `# X.Y.Z` |
| `docs/changes/CHANGELOG_<prev>.md` | move the previous root changelog here first |

### 3. Validation & Error Matrix

- Bumping `Cargo.toml` alone -> `version_metadata` fails on `EXPECTED_RELEASE`.
- Stale `README.md` / `package.json` -> same test fails later in the same run.
- Root `CHANGELOG.md` retaining old releases -> stale history is published verbatim
  as the GitHub Release body.

### 4. Good/Base/Bad Cases

- Good: all seven updated in one `chore(release): X.Y.Z` commit.
- Bad: bumping `Cargo.toml` and running only the crate's own tests — the failure
  lives in `-p hya`, so a scoped test run misses it entirely.

---

## Scenario: Reducing a Transcript Before a Provider Request

### 1. Scope / Trigger

- Trigger: any change to what the turn loop sends the model — compaction
  thresholds, eviction, summarization, or token accounting.
- Applies to `crates/hya-core/src/compaction.rs` and the reduction block in
  `crates/hya-core/src/engine/turn.rs`.

### 2. Contracts

- **A measured token count describes the transcript at the moment it was
  measured.** `tokens_in_use` prefers the provider-reported usage on the most
  recent assistant message. After any request-local edit (tool-output eviction,
  message rewriting) that number is stale: re-measuring reports no saving.
  Carry one running count and apply each reduction as a delta instead.
- A function that re-derives the compaction decision must not be called after
  such an edit. `fold_prefix` exists for callers that already decided;
  `plan_compaction_at` is the guarded wrapper.
- Request-local reductions must never write the store. The event log stays a
  sufficient statistic for offline reconstruction (see 0.34.15).
- Thresholds scale to `Capabilities::max_context` when advertised; a route with
  no window keeps the configured flat threshold. Clamp resolved thresholds to a
  floor — a near-zero threshold compacts every turn, which is worse than never.

### 3. Validation & Error Matrix

- Re-measuring after eviction -> saving invisible, summarizer runs anyway.
- No floor on a scaled threshold -> compact-every-turn loop on a small window.
- Trusting an out-of-range `context_fraction` -> nonsense threshold.
- Eviction inside `keep_recent` -> the agent loses the result it just fetched.

### 4. Good/Base/Bad Cases

- Good: eviction alone drops under the threshold, `ContextEvicted` is recorded,
  no summarizer call, and the log still holds the full tool output.
- Base: a route reporting no usage falls back to `chars / 4` with behaviour
  identical to before.
- Bad: testing eviction within a single turn. Every tool part of one turn lands
  in the **same** assistant message, which sits inside `keep_recent`; eviction is
  a cross-turn reduction and a single-turn test will always see zero evicted.

### 5. Tests Required

- A table test over the threshold resolver, including the clamp and bad input.
- A regression proving unchanged behaviour when no usage is reported.
- A cross-turn test that eviction alone avoids the summarizer.
- A test that the event log retains full tool output after an evicted turn.

---

## Scenario: Replay-Safe Provider Recovery And Liveness

### 1. Scope / Trigger

- Trigger: changes to HTTP request retries, route ordering, OAuth refresh,
  category model chains, response-header deadlines, or SSE liveness.
- Applies to `hya-provider` transport/router code and the `hya-core` turn path.

### 2. Signatures

- `HttpProvider::with_auth_refresher(AuthRefresher)` installs one forced-refresh
  callback for a failed bearer value.
- `HttpProvider::with_response_header_timeout(Duration)` overrides the
  per-attempt header deadline; the default is 60 seconds.
- `HttpProvider::with_idle_timeout(Duration)` overrides the established SSE
  frame-idle deadline; the default is five minutes.
- `HttpProvider::with_retry(RetryConfig)` overrides the replay budget
  (`max_attempts`, `backoff_base`, `backoff_max`); the default is 3 attempts,
  100 ms backoff seed, 30 s cap. The `provider_retry:` config block sets the
  global default, per-provider `retry:` blocks override fields, and
  `HYA_PROVIDER_RETRY_*` env vars win over both.
- `SessionEngine::with_model_fallbacks(HashMap<ModelRef, Vec<ModelRef>>)` installs
  ordered category chains whose first candidate must equal the map key.

### 3. Contracts

- Recovery is allowed only before an `EventStream` exists. A returned stream is
  the strict no-replay boundary for request retries, route failover, model
  failover, and auth refresh.
- Zero-event replay window: a response that dies before delivering any event to
  the consumer is treated as if no stream existed — the whole request is
  re-issued inside the shared attempt budget. Link-level failures only (byte
  stream decode errors, connection resets, pre-first-frame idle stalls);
  provider-decided failures (200-with-error-body frames, malformed payloads,
  missing terminal frames) surface immediately even at zero events. The first
  delivered event closes the window permanently.
- One HTTP route uses at most `max_attempts` request attempts (default three)
  for transport errors, 429, and 5xx. A valid bounded `Retry-After` overrides
  exponential jittered backoff.
- A pre-stream 401/403 may force-refresh once, only while an attempt slot remains.
  Header resolution runs again and the token must differ from the failed value.
- Router failover preserves model identity and advances to the next matching
  route only after a retryable pre-stream failure.
- Core model fallback re-enters the router with the next category candidate on a
  retryable pre-stream error or `UnknownModel`; it never consumes non-retryable
  protocol, compatibility, decode, or human-action auth errors. Once that chain
  stops (any class), the `model.fallback` hook may name one more model per
  consult; already-tried models are refused, a round is capped at eight
  attempts, and the hook is never consulted after a stream exists or on a
  Workflow-routed turn.
- The header deadline is a retryable transport failure. The SSE idle deadline is
  delivered once on the established stream; before the first frame it joins the
  zero-event replay window, after any frame it is terminal and not retryable. A
  stream that keeps producing frames has no total lifetime deadline.

### 4. Validation & Error Matrix

- Invalid fallback chain head -> ignore that chain; the preferred model keeps
  single-model behavior instead of partially honoring an unsafe order.
- Transport/header timeout, 429, or 5xx before stream -> bounded same-route
  retry, then matching-route/model-chain failover when available.
- Zero delivered events + link-level body failure or pre-first-frame idle ->
  transparent re-issue inside the remaining shared budget; exhaustion surfaces
  the last error once on the stream.
- Zero delivered events + provider-decided failure (error frame, decode,
  missing terminal) -> surface immediately without consuming replay budget.
- One or more delivered events + any failure -> surface exactly once; zero
  replay or failover.
- 401/403 with no refresher, failed refresh, unchanged token, or no remaining
  attempt -> original status error; no synthetic auth success.
- `AuthExpired`, incompatible request, decode error, or other non-retryable
  failure -> surface immediately without advancing a route/model chain.
- SSE idle after the first delivered frame -> one stream error; zero replay or
  failover.

### 5. Good/Base/Bad Cases

- Good: a header-stalled first route exhausts its bounded attempts, then a second
  route serves the request before any event exists.
- Base: a healthy stream resets its five-minute window on every frame and may run
  longer than five minutes in total.
- Bad: wrapping the complete stream in a request timeout or replaying a request
  after one streamed event; either can duplicate visible output and tool effects.

### 6. Tests Required

- Paused-time HTTP tests cover attempt count, backoff, bounded `Retry-After`,
  response-body deadline, header timeout, and one forced refresh inside budget.
- Zero-event tests cover: a truncated body before any frame replays within the
  budget and succeeds; `max_attempts: 1` fails fast; a failure after a
  delivered event is never replayed; pre-first-frame idle stalls replay within
  the budget; provider error frames never replay.
- Router tests cover matching-route order, retryable/non-retryable classification,
  and zero failover after stream construction.
- Core tests cover configured chain order, forward suffixes, `UnknownModel`,
  non-retryable termination, and no fallback after stream construction.
- SSE tests cover first-frame idle, inter-frame reset, one timeout error, a
  continuously active stream with no total lifetime cap, and dropping the
  EventStream aborting the HTTP body before the idle deadline.

### 7. Wrong vs Correct

#### Wrong

```rust
// A total timeout crosses the replay boundary and aborts healthy long streams.
timeout(Duration::from_secs(300), provider.stream(request, session, message)).await
```

#### Correct

```rust
// Bound headers before stream ownership; bound silence inside the stream pump.
// Dropping the EventStream closes `tx` and must abort the HTTP body immediately.
let response = timeout(header_deadline, request.send()).await??;
pump(response, decoder, tx, stream_idle_deadline);
```

---

## Scenario: File Tool Workdir Containment

### 1. Scope / Trigger

- Trigger: adding or changing a file tool path argument, default search root, or
  external-directory permission check.
- Applies to read/write/edit/find/glob/grep and future filesystem tools.

### 2. Signatures

- File tools resolve user paths with `resolve_file(&ToolCtx.workdir, path)`.
- Read and Grep derive a kind-blind wildcard from the lexical parent and call
  their external-directory assertion before metadata or target-kind probing;
  directory traversal starts only after that admission succeeds.
- An omitted `find.path` means the bound workdir; it does not mean process cwd.

### 3. Contracts

- Relative paths resolve under the Session workdir.
- Paths inside any workspace root and the omitted workdir default proceed
  without an external-directory grant.
- Absolute paths and `..` traversal outside every workspace root use the same
  permission plane as every other file tool; containment is decided once, by
  `hya_tool::ProjectScope`, after symlink resolution (ADR-0026). A tool must
  not normalize away the escape and then operate directly.
- Containment is an authorization rule, not a search-result filter: reject the
  root before metadata/existence probing or any partial result. A denied file
  and denied directory sibling use the same lexical permission resource.

### 4. Validation & Error Matrix

- Relative in-workdir path -> resolve and execute.
- Absolute in-workdir path -> execute.
- Absolute out-of-workdir path without grant -> `ToolError::Permission` before
  metadata, existence, or target-kind observation.
- Parent traversal out of workdir without grant -> `ToolError::Permission`.
- Omitted path -> search exactly the Session workdir.

### 5. Good/Base/Bad Cases

- Good: `find {"path":"src"}` searches `<workdir>/src`.
- Base: `find {}` searches the whole workdir and needs no external assertion.
- Bad: `PathBuf::from(input.path)` makes a relative path process-cwd dependent
  and lets `../outside` bypass the shared permission contract.

### 6. Tests Required

- Every path-taking file tool needs relative, absolute in-workdir, absolute
  outside, and parent-traversal behavior tests where applicable.
- Read/Grep permission regressions compare the first requested resource for an
  external file and directory sibling and prove denial precedes metadata.
- A containment regression must assert the typed permission failure, not only an
  empty result or OS error.
- Mutation proof for `find` replaces the shared resolution with `PathBuf::from`;
  the relative/outside/traversal tests must fail.

### 7. Wrong vs Correct

#### Wrong

```rust
let root = PathBuf::from(input.path.unwrap_or_else(|| ".".to_string()));
```

#### Correct

```rust
let root = resolve_file(&ctx.workdir, input.path.as_deref().unwrap_or("."))?;
assert_external_directory(ctx, &root, false).await?; // lexical parent scope
let metadata = tokio::fs::metadata(&root).await?;
```

---

## Scenario: Immutable Startup Model Catalog

### 1. Scope / Trigger

- Trigger: changing provider configuration, model discovery, routing, catalog
  APIs, OAuth provider upsert, or the models CLI.

### 2. Signatures

- Composition: `pub async fn hya_app::config::load() -> anyhow::Result<Option<ResolvedConfig>>`.
- Snapshot: `ProviderCatalogSnapshot::{models,providers,default_model,notice}`.
- Discovery: `discover_models(CatalogDiscoveryRequest) -> ProviderDiscoveryOutcome`.

### 3. Contracts

- A normalized non-empty Hya model list is network-free and authoritative.
- An empty list prefers `$XDG_CONFIG_HOME/hya/models.yml.cache` (same directory
  as `config.yaml`) so startup does not block on discovery HTTP. Cache hits
  publish discovered rows immediately (including `limit.context` /
  `limit.output` and reasoning variants/default) and queue background refresh.
- Cache miss still performs one bounded optional-auth discovery sequence during
  `config::load`, then writes `models.yml.cache`.
- Background refresh (`refresh_pending_catalogs`) rewrites the cache and swaps the
  live engine router/catalog; frontends pick up the swap on their next
  bootstrap/catalog fetch (the old Compat `catalog.updated` SSE nudge is
  deleted).
- Explicit `providers.*.models` in config always wins over cache.
- Router, engine, CLI, HTTP/bootstrap, and SDK clients consume one shared
  snapshot;
  the snapshot may be replaced after background refresh.
- Discovery never mutates `config.yaml` or reads foreign product configuration.

### 4. Validation & Error Matrix

- Credentialless 401/403 -> `auth_required`; credentialed -> `auth_rejected`.
- Empty -> zero provider rows plus `empty`; malformed/oversized -> `invalid`;
  timeout, redirect, and non-auth HTTP failure -> `unavailable`.
- Any provider-local failure keeps other valid rows and cannot invent a model.

### 5. Good/Base/Bad Cases

- Good: empty anonymous endpoint discovers rows and builds an anonymous route.
- Base: no live rows publishes exactly local `hya/offline` with a notice.
- Bad: deriving a row from an agent, Session, category, default, or OAuth guess.

### 6. Tests Required

- Provider tests assert headers, URL/parser rules, limits, typed outcomes,
  normalization, and offline suppression.
- Process tests assert request counts per startup, no config write, cross-surface
  row equality, auth-state split, foreign-config isolation, and offline echo.

### 7. Wrong vs Correct

#### Wrong

```rust
let models = configured.or_else(|| Some(vec![agent.model.clone()]));
```

#### Correct

```rust
let snapshot = ProviderCatalogSnapshot::build(rows, states, configured_default);
let models = snapshot.models();
```
---

## Scenario: Native Coding Tools And Synchronized Presentation Contract

### 1. Scope / Trigger

- Trigger: changing a built-in Read, Edit, Grep, Write, Bash, or Task schema or
  executor; the tool-result cap; provider tool replay; durable tool events; or
  the client completed-tool presentation contract.
- Applies across `crates/hya-tool` (schema, permission, execution, and native
  hashline runtime), `hya-core` (dispatch and event commit), `hya-proto`
  (projection), `hya-provider` (tool-result text reconstruction), and the v1
  curated event mapping (`hya-server/src/v1/convert.rs`). The presentation
  boundary below is the contract any client renderer follows; the legacy
  TypeScript TUI renderer that implemented it was removed (a future TUI on
  `hya-sdk-v1` keeps the same boundary).
- Hashline behavior is pinned to `pi-hashline-edit` 0.8.3, npm `gitHead`
  `ba7db9943d0f58499b24c1f6bd64722580f772a5` and tarball SHA-1
  `8985f24c3493be375cc225a5522ed54de8daabc9`. Host Write/Bash behavior is
  derived from `@oh-my-pi/pi-coding-agent` 18.1.3 at
  `can1357/oh-my-pi@0b769cc4dd9771373335430385d1d2f696dc3498`.
- The source-derived port remains MIT-licensed with attribution to
  `Copyright (c) 2026 RimuruW` for `pi-hashline-edit`, whose hashline source is
  adapted from Oh My Pi (`Copyright (c) 2025 Mario Zechner; Copyright (c)
  2025-2026 Can Bölük; Copyright (c) 2026 Stencil Labs, Inc.`). Ship the full
  applicable permission text in the tool notice; do not replace it with credits
  only.

### 2. Signatures

- `Tool::schema() -> ToolSchema` publishes only canonical model-facing names;
  `Tool::execute(&ToolCtx, Value) -> Result<Value, ToolError>` returns one
  bounded result envelope.
- Read publishes this closed schema (the legacy `filePath` key is not listed):

  ```json
  {"type":"object","additionalProperties":false,"required":["path"],"properties":{"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1},"raw":{"type":"boolean"}}}
  ```

- Edit publishes a closed `{ "path": string, "edits": Edit[] }` object.
  Each closed `Edit` variant is `replace { op, pos, end?, lines }`,
  `append { op, pos?, lines }`, `prepend { op, pos?, lines }`, or
  `replace_text { op, oldText, newText }`; `op` is required and every variant
  rejects unknown properties.
- Grep publishes `{ "pattern": string, "path"?: string, "glob"?: string,
  "ignoreCase"?: boolean, "literal"?: boolean, "context"?: integer,
  "limit"?: integer }`, with `pattern` required, `context` in `0..=5`, and
  `limit` in `1..=200`.
- Write publishes the closed `{ "path": string, "content": string }` object.
  Bash publishes the closed `{ "command": string, "env"?: {string: string},
  "timeout"?: number, "cwd"?: string, "pty"?: boolean }` object. `bash` is
  canonical; `shell` resolves only through a hidden runtime alias.
- Task publishes top-level and member `inline_agent` objects with only
  `name`, `prompt`, `category`, `model`, and `resident`. The hidden parser may
  retain `description` solely to normalize stale calls: blank/whitespace is
  `None`, while non-empty input remains available for typed rejection.
- `ToolRegistry::builtins()` creates one `Arc<HashlineRuntime>` shared by
  Read/Edit/Write/Grep and a separate Bash executor. `ToolCallRequested` is
  admitted through the existing permission plane, then produces `ToolResult`
  or `ToolError`; projection yields an SDK `ToolPart`, which is consumed by
  `presentCodingTool(part) -> CodingToolView | undefined` and
  `CodingToolPresentation(props) -> JSX.Element`.

### 3. Contracts

- The schema is a publication contract, not a parser dump. Hidden model-facing
  compatibility is limited to Read `filePath` and `offset: 0`, Task empty nested
  `inline_agent.description`, and Bash `shell`. Edit may accept only the pinned
  package `prepareArguments` boundary (`file_path`, JSON-string `edits`, inferred
  `replace_text`, and complete camel/snake old/new pairs); it must not expose or
  accept hya's fuzzy `filePath + oldString + newString` surface. Grep's removed
  `include` key is not a compatibility path.
- Read resolves `path` and `filePath` as distinct untrimmed strings: one
  non-empty value succeeds, equal non-empty values succeed, conflicting
  non-empty values fail, and both absent/empty values fail. `offset: 0` means
  line 1 only at the hidden compatibility boundary. A lexically external path
  uses one kind-blind parent wildcard permission before metadata, existence, or
  file-kind probing. Text uses BOM removal, CRLF/lone-CR normalization,
  contextual XXH32 hashline anchors, raw mode, positive offset/limit slicing,
  truncation notices, and invalid UTF-8 warnings; directory listings and
  image/PDF attachments remain explicit hya extensions.
- Edit validates every anchor against one pre-edit document, resolves all
  spans before mutation, rejects duplicates/conflicts/no-empty-file results,
  applies spans bottom-up, and uses exact context-3/fuzz-0 stale recovery from
  bounded snapshots only after a direct stale-anchor failure. Grep uses native
  regex/literal matching and gitignore-aware traversal, merges context ranges,
  observes one extra match for truncation, and records snapshots only for
  successfully rendered files. Caller globs stop at 4,096 bytes; both `[!x]`
  and `[^x]` are negated classes in caller and ignore patterns. Ignore inputs
  and rule counts are bounded. Grep streams logical lines, discards a line over
  1 MiB through its newline with one bounded warning, continues with later
  lines, and checks cancellation inside traversal, ignore, and scan loops.
- Hashline state is private, process-local, and bounded by
  `(SessionId | no-session, normalized workdir, resolved target path)`: lexical
  permission uses the requested path first, then symlink resolution establishes
  the target identity for locking and snapshots. At most 8 target entries, 4
  versions per target, and 32 MiB total are retained. Fixed lock shards
  serialize one target without an attacker-growing lock map. Contents never
  enter logs or error payloads. A non-raw Read clears the repeated-edit marker;
  two identical no-op payloads are soft success and the third is hard failure.
- A target lock spans live load, validation/recovery, atomic write, formatter,
  BOM/line-ending restoration, LSP, final reload, diff/display generation, and
  snapshot update. The prepared target identity is revalidated immediately
  before regular rename or hard-link truncate/open; a changed pathname/alias is
  rejected before mutation. Fresh anchors, diffs, diagnostics, and UI metadata
  describe final post-formatter bytes. If an atomic write committed but
  synchronization, formatter, LSP, reload, preview, or a later cancellation
  occurs, the adapter reconciles at the commit boundary, records the actual
  final snapshot and payload guard, and never reports a pre-write fiction.
- Write uses the shared atomic writer: follow relative/absolute symlink chains
  for at most 40 hops and reject cycles; write an existing hard-linked inode in
  place; otherwise use a same-directory mode-0600 temp, fsync, rename, and
  cleanup on every failure. Preserve mode/BOM/line endings, strip a complete
  unambiguous copied hashline prefix only, and warn (not silently fail) when a
  leading-shebang execute-bit update cannot be applied. Bash defaults to 300
  seconds; zero disables the deadline; finite nonzero values clamp to
  `1..=3600`. Cancellation and timeout terminate and reap the complete process
  group, including PTY descendants that retain the slave after leader exit.
  PTY execution is real when requested; PTY unavailability is explicit, not a
  silent fallback. Output is captured incrementally, bounded to 50 KiB inline
  with a complete artifact when truncated. Spill ownership is armed before the
  first write, uses mode `0600` on Unix, removes partial/unpublished files on
  every failure or cancellation, and disarms only when a successful result
  publishes `outputPath`. Timeout/clamp notices are applied before the spill
  decision. Nonzero exit and timeout are completed structured results, while
  explicit cancellation is typed cancellation. Environment values never appear
  in titles, output summaries, diagnostics, or any client surface.
- Results preserve `{ "title": string, "output": value, "metadata": object }`.
  Unrelated built-in/MCP/plugin results retain the 5,000-character default;
  coding tools use their declared bounded output policy and independently
  bounded metadata/display rows, with explicit truncation flags. Nested rows
  and groups are serialized exactly once for budget accounting, and capping is
  structurally idempotent. The engine reapplies the coding cap after post-tool
  hooks, immediately before durable publication, so a hook cannot bypass the
  final envelope bound. Provider replay uses an object's string `output` field
  first and JSON-stringifies only an object without that field.
- Task's empty nested description normalizes to absence before admission, so
  the captured call can create a child and resume its parent. A non-empty direct
  or stale nested description returns the existing typed
  `UnsupportedInlineAgentField { field: "description" }` before child/session
  side effects. Authorization, model/category precedence, resident behavior,
  admission ownership, and run-tree projection do not change.
- The client consumes only projected SDK `ToolPart` state through the SDK. It
  does not fetch, poll, replay Events, hydrate a second message store, schedule
  a presentation timer, or create another result owner. Completed Read/Write
  render titled, file-grammar-aware numbered text; Edit uses the semantic diff
  primitive and keeps removed/added rows distinct at 80 columns; Grep renders
  per-file titled rows from bounded `metadata.display.groups[]`; Bash and hidden
  Shell share one command/output block, accept nullable exit for timeout/signal,
  highlight only the command, strip ANSI from plain output, and never display
  `env`. Top-level truncation is combined with adapter-specific display/row/
  group/diff flags. Diagnostics retain only the first three severity-one items
  with a positioned range and render one-based `Error [line:column]` labels.
  Malformed or unsupported metadata returns `undefined` and uses the existing
  readable fallback. Collapse is reversible UI state, distinct from backend
  truncation.
- Historical `Event` rows are immutable. Replay must expose old errors exactly
  as stored while a restarted 0.36.9 backend handles new calls; an already
  running 0.36.8 backend must restart before it can use these schemas.

### 4. Validation & Error Matrix

| Boundary or condition | Required result |
| --- | --- |
| Read path values equal/sole non-empty | Execute after external-path and Read permission checks |
| Read paths conflict, both absent, or both empty | `ToolError::Input`; no path I/O or permission ask |
| Read `offset`/`limit` is zero or invalid type (except hidden offset zero) | Input failure; schema advertises positive values only |
| Edit malformed/unknown variant or mixed compatibility fields | Hashline `E_BAD_REQUEST`/`E_BAD_OP`, mapped to typed `ToolError::Input` |
| Edit malformed anchor, out-of-range span, stale anchor, duplicate/conflict, or would-empty result | `E_BAD_REF`/`E_RANGE_OOB`/`E_STALE_ANCHOR`/`E_DUPLICATE_EDIT`/`E_EDIT_CONFLICT`/`E_WOULD_EMPTY`; bounded hints contain no file text |
| Edit/Write formatter, LSP, final reload, or preview fails after commit | Commit-boundary reconciliation; contextual `ToolError::Other` beginning with `File changed at <path>` plus final snapshot |
| Hashline output/config/result capacity is exceeded | `E_BAD_CONFIG`/`E_OUTPUT_LIMIT`/`E_CAPACITY`, or explicit bounded truncation where the contract permits it |
| External path or tool permission is denied | Existing `ToolError::Permission`, in existing lexical-path order; no hashline state mutation |
| External Read/Grep target is missing, a file, or a directory | One kind-blind lexical parent resource is authorized before metadata; denial reveals no target kind |
| Prepared regular/hard-link pathname changes before commit | Non-committed contextual I/O error; swapped inode is not replaced or truncated |
| Grep line exceeds 1 MiB or ignore input exceeds its rule budget | Skip the bounded source with one warning, continue later work, and remain cancellable |
| Task description is blank vs non-empty | Blank becomes absent and proceeds; non-empty is typed unsupported-field rejection before admission |
| Bash timeout is zero, finite out of range, non-finite, nonzero exit, timeout, or cancellation | Zero disables; finite value clamps with notice; nonzero/timeout complete structurally; cancellation is `ToolError::Cancelled` |
| PTY requested but unavailable | Explicit input/runtime error; never run non-PTY as an implicit fallback |
| PTY leader exits while a descendant retains the slave | Continue deadline/cancellation observation; kill and reap the process group on either signal |
| A post-tool hook expands a coding result | Reapply the shape-aware cap before Event publication; keep a structured envelope within the hard bound |
| Client sees unknown keys, malformed metadata, syntax parser failure, or pending/error state | Omit specialized view and use existing fallback; syntax failure becomes readable plain text; no secret/unknown field is rendered |
| Replayed historical Event or old backend process | Do not rewrite the Event; restart the old process before issuing new 0.36.9 calls |

Hashline failures use the private `{ code, message, hints }` envelope and retain
the stable bracketed code in `ToolError::Input` (for example `[E_STALE_ANCHOR]`).
They must not collapse into generic I/O, permission, or success results.

### 5. Good / Base / Bad Cases

- Good: the captured four-key Read request `{filePath, path, offset: 0,
  limit}` resolves one path, returns a correlated `ToolResult`, and produces a
  titled numbered SDK part; a later formatter changes an Edit, and its preview,
  diff, anchors, and snapshot all describe the formatted bytes.
- Good: a completed Grep result has bounded per-file groups, the client displays
  matches at both 80 and 140 columns, and reopening the Session renders the same
  projected blocks without a presentation request.
- Base: a raw Read, directory, attachment, hidden `shell` call, two no-op edits,
  or Task empty description keeps its documented compatibility path; ordinary
  tools still use the 5,000-character result default.
- Bad: aliasing `filePath` onto the canonical Read field, restoring fuzzy Edit,
  storing snapshots by process-global path, retaining unbounded output, or
  treating a post-commit formatter failure as if no write occurred.
- Bad: making a client fetch tool results, render arbitrary metadata keys, expose
  `env`, sort Grep groups differently from backend order, or replace historical
  errors during replay.

### 6. Tests Required

- `hya-tool` Read tests assert published schema and canonical/legacy/equal,
  one-empty/conflict/missing/offset-zero/raw/directory/media/UTF-8/truncation/
  permission behavior; the exact four-key request must produce a completed
  part, not a duplicate-field error.
- Hashline tests assert golden anchors (`alpha/beta/gamma/delta -> KT/JB/KJ/PX`),
  widths 2..=4, parser and hint collisions, every stable failure code,
  bounded LRU/lock isolation, duplicate/no-op guards, exact stale recovery, and
  filesystem mode/BOM/line-ending/symlink/hard-link/cleanup behavior.
- Edit/Grep/Write/Bash tests assert every operation, native traversal and
  context/limit boundary, final formatter/LSP reconciliation, closed schemas,
  prefix/shebang behavior, timeout/cancel/exit/PTY-descendant/artifact behavior,
  inner-loop cancellation, logical-line/ignore/glob bounds, target-identity
  swaps, and one-pass metadata/result caps. Include the provider
  `wire::tool_result` object-output preference and JSON fallback, plus a direct
  post-hook expansion regression at the final Event boundary.
- Task and app admission tests assert both nested schemas omit `description`,
  the captured empty call creates/resumes with no unsupported-field error, and a
  non-empty direct value has no child/session/event side effect.
- Core/server tests assert permission ordering, `ToolCallRequested` correlation,
  unchanged historical Events, projection replay, and no second result store.
  (The frontend presentation tests listed by earlier revisions of this section
  asserted allowlisted semantic views, malformed fallback, titles, syntax
  spans, offset/line numbers, diff mode, per-file Grep, bash/shell
  normalization, ANSI-safe output, secret exclusion, live part replacement, and
  no network or timer from presentation, at 80 and 140 columns; they were
  removed with the legacy TUI and return with a future `hya-sdk-v1` frontend.)

### 7. Wrong vs Correct

#### Wrong

```rust
#[derive(Deserialize)]
struct ReadInput {
    #[serde(alias = "filePath")]
    path: String,
}

// This aliases two wire keys onto one Serde field, so the captured request can
// fail before path resolution, permission admission, or I/O.
let output = cap_tool_output(tool.execute(&ctx, input).await?);
```

```typescript
// WRONG: fetching a per-tool surface that no longer exists
createEffect(async () => {
  const result = await fetch(`/session/${sessionID}/tool/${part.id}`)
  setLocalTool(result)
})
```

#### Correct

```rust
#[derive(Deserialize)]
struct ReadInput {
    path: Option<String>,
    #[serde(rename = "filePath")]
    file_path: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
    raw: bool,
}

let result = tool.execute(&ctx, input).await?;
// The engine applies the declared shape-aware cap without discarding:
// {"title": string, "output": value, "metadata": object}.
Ok(result)
```

```typescript
const view = presentCodingTool(part)
return view ? (
  <CodingToolPresentation part={part} width={width} diffStyle={diffStyle} diffWrapMode={diffWrapMode} />
) : (
  <GenericTool {...toolprops} />
)
```

The first path creates a schema/execution contradiction and destroys semantic
metadata. The second validates compatibility at the adapter boundary and lets
one projected SDK part drive live rendering and replay.
