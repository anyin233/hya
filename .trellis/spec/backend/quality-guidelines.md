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
- Applies to Compat-compatible prompt routes, native prompt routes, and future admission-style routes that must acknowledge work before the provider stream completes.

### 2. Signatures

- Route shape: `POST /api/session/{session_id}/prompt` and equivalent prompt-admission endpoints.
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

- Good: a pending title provider cannot block `POST /api/session/{id}/prompt`; session context can still show the admitted unfinished assistant state.
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

## Scenario: Root Compat permission and question lifecycle

### 1. Scope / Trigger

- Trigger: changes to the root OpenCode-compatible permission/question routes,
  pending interaction storage, or `/global/event` serialization.
- The root routes implement the pinned SDK contract. `/api/*` routes retain
  their separate V2 wrappers and field names.

### 2. Signatures

- `GET /permission` -> `LegacyPermissionRequestView[]` with `id`, `sessionID`,
  `permission`, `patterns`, `metadata`, `always`, and
  `tool.{messageID,callID}`.
- `POST /permission/:request/reply` with
  `{ "reply": "once" | "always" | "reject", "message"?: string }`.
- `GET /question` -> entries with `id`, `sessionID`, and `questions`.
- `POST /question/:request/reply` with `{ "answers": string[][] }`.
- `POST /question/:request/reject` with no required body.
- `GET /global/event` -> SSE data shaped as
  `{ "directory": string, "payload": { "id", "type", "properties" } }`.

### 3. Contracts

- `permission.asked.properties` uses the same legacy view as `GET /permission`;
  do not substitute the `/api/*` `action/resources/save` view.
- `question.replied.properties` includes `sessionID`, `requestID`, and the
  submitted `answers`; `question.rejected.properties` includes `sessionID` and
  `requestID`.
- Every `/global/event` item, including connected, engine, permission,
  question, and heartbeat events, carries the requested project `directory`.
- Pending insertion precedes the asked event. Pending removal plus successful
  reply-channel completion precedes the completion event. This makes duplicate
  replies return not-found without publishing a second completion.

### 4. Validation & Error Matrix

- Invalid root permission/question request ID -> `400 Bad Request`.
- Missing, wrong-session, or duplicate request -> `404 Not Found`; no
  completion event.
- Invalid permission reply or non-`string[][]` question answers ->
  `400 Bad Request`.
- Successful root reply/reject -> JSON `true`; exactly one completion event;
  request absent from the next list response.
- Dropped reply channel -> no successful response claim and no completion
  event.

### 5. Good/Base/Bad Cases

- Good: the pinned SDK receives a live request, replies once, observes one
  completion event, and a duplicate reply returns `404`.
- Base: an empty pending set returns `[]` and the global stream still emits a
  directory-bearing connected/heartbeat envelope.
- Bad: publishing `question.replied` before the reply channel succeeds, or
  emitting a root permission view with only `action/resources/save`.

### 6. Tests Required

- Route tests assert the complete root permission/question field sets and
  duplicate `404` behavior.
- `/global/event` tests assert `directory` on connected and interaction events,
  and assert question reply `answers`.
- A real pinned-SDK test must cover permission once/reject and question
  reply/reject, side effects, exactly-once events, and final empty pending lists.

### 7. Wrong vs Correct

#### Wrong

```json
{"payload":{"type":"question.replied","properties":{"requestID":"q_1"}}}
```

#### Correct

```json
{"directory":"/project","payload":{"type":"question.replied","properties":{"sessionID":"hysec_...","requestID":"q_1","answers":[["Yes"]]}}}
```

---

## Scenario: Session ID compatibility across routes and fixtures

### 1. Scope / Trigger

- Trigger: any API route, client URL builder, sync/projector path, TUI/control route, or test fixture that accepts or emits a session ID.
- Applies to Compat-compatible `sessionID` payload fields, native path parameters, experimental routes, sync replay/history routes, and test helpers that create sessions.

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

- Trigger: any `hya-backend` command that creates, mutates, replays, lists, or serves sessions while accepting a SQLite database path.
- Applies to headless `exec` / `run`, `sessions`, `tail-session`, `serve`, and future CLI commands that share the event-sourced session store.

### 2. Signatures

- Headless execution: `hya-backend --db <path> exec <prompt>` and `hya-backend --db <path> run <prompt>`.
- Listing: `hya-backend sessions --db <path>`.
- Server: `hya-backend serve --db <path> --bind <addr>`.
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

- Good: `hya-backend --db /tmp/hya.db exec "Say hello"` writes events to `/tmp/hya.db`, and `hya-backend sessions --db /tmp/hya.db` prints the resulting `hysec_...` row.
- Base: omitting `--db` uses in-memory execution and does not leave a durable session after process exit.
- Bad: `exec` constructs `SessionStore::connect_memory()` even though the top-level CLI parsed `--db <path>`.

### 6. Tests Required

- Add a CLI integration regression that runs `hya-backend --pure --db <tmp>/hya.db exec <prompt>` and then asserts `hya-backend sessions --pure --db <tmp>/hya.db` contains `hysec_`.
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

- Trigger: startup/deferred/Compat MCP changes, startup plugin tool
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

### 3. Required tests

- Stale success closes and cannot publish over a newer ticket.
- Explicit removal reaches the next binding despite unrelated connect failure;
  the old binding remains callable until dropped.
- Current partial failure closes unpublished owners and preserves generation.
- Duplicate source/export/canonical/alias and plugin handshake-ID mismatch fail
  before publication and consume no generation.
- Mixed MCP/plugin startup publishes one complete snapshot exactly once.
- Compat MCP add/remove changes callability through the same registry.
- Reordered equivalent plugin initialize declarations compare equal; changing
  tool, command/permission hook, or workspace declarations detects drift.
- Cargo manifests and `Cargo.lock` add no dependency for declaration hashing.

---

## Scenario: GitHub Release Binary Workflow

### 1. Scope / Trigger

- Trigger: any change that publishes release binaries, creates GitHub Releases, or modifies the release changelog process.
- Applies to `.github/workflows/release.yml`, root `CHANGELOG.md`, `docs/changes/`, root `AGENTS.md` release rules, and release-related task artifacts.

### 2. Signatures

- Release tag: `vX.Y.Z`, where `X.Y.Z` must match Cargo's `hya` package version.
- Cargo command: `cargo build --release --locked -p hya -p hya-backend -p hya-ts --bins --target x86_64-unknown-linux-gnu`.
- Release archive: `hya-<version>-x86_64-unknown-linux-gnu.tar.gz`.
- Checksum file: `SHA256SUMS` generated beside the release archive.
- Non-publishing rehearsal (requires Bun `1.3.14` and `actionlint` `1.7.12` on
  `PATH`):

```sh
cargo run -p xtask -- release-rehearsal \
  --workflow .github/workflows/release.yml \
  --version 0.36.7 \
  --target x86_64-unknown-linux-gnu \
  --no-publish
```

### 3. Contracts

- Root `CHANGELOG.md` contains only the newest version's release notes.
- Historical changelogs live under `docs/changes/CHANGELOG_<version>.md`.
- The GitHub Release body is read verbatim from root `CHANGELOG.md`.
- Release workflow permissions are read-only by default; only the release publishing job may request `contents: write`.
- Build provenance attestations are generated for the archive and checksum.
- Third-party release actions are pinned to immutable commit SHAs.
- The publishing job uses the `release` environment so repository settings can require manual approval.
- Within the release archive, the payload includes the three shipped Rust
  binaries, the prepared `lib/hya/hya-tui-ts` runtime, the production
  `lib/hya/compat-adapter`, and the generated member
  `examples/hya-argus-example.hyabundle`; it does not add `hya-updater`.
- `scripts/package-argus-example.sh` generates that member from tracked source
  `bundles/examples/argus-example`; no root `examples/` artifact is an input.
- The rehearsal requires the explicit `--no-publish` guard, builds and packages
  in a temporary directory, and never creates a tag or GitHub Release.
- `release-rehearsal` owns the pinned `actionlint` and embedded-shell checks.
  The current CI workflow does not run `actionlint` as a separate gate.

### 4. Validation & Error Matrix

- Missing `v` tag prefix -> fail before build.
- Tag version is not semver-shaped -> fail before build.
- Tag version differs from `cargo metadata` package version for `hya` -> fail before build.
- Missing or empty `CHANGELOG.md` -> fail before publishing.
- `CHANGELOG.md` first heading differs from the tag version -> fail before build.
- Build, archive, checksum, or packaged-binary smoke failure -> skip release publishing.
- Missing release assets -> fail `softprops/action-gh-release` with `fail_on_unmatched_files: true`.
- Missing `--no-publish` -> rehearsal rejects before validation or build.
- `actionlint` missing or not version `1.7.12`, or Bun not version `1.3.14` ->
  rehearsal fails its pinned prerequisite check.
- Missing Compat adapter runtime, Argus package, locked production dependency,
  or archive member -> package/rehearsal smoke fails before publication.

### 5. Good/Base/Bad Cases

- Good: `v0.1.0`, `[workspace.package].version = "0.1.0"`, root `CHANGELOG.md` contains only `0.1.0` notes, archive and checksum pass smoke checks.
- Base: first release has no historical changelog; keep `docs/changes/.gitkeep` and root `CHANGELOG.md` for the current version.
- Bad: appending old release notes to root `CHANGELOG.md`; this publishes stale history as the GitHub Release body.
- Good: the no-publish rehearsal validates the real workflow, exact payload,
  launcher handoff, Compat adapter handshake, Argus package closure, and
  checksum without publishing.
- Base: a rehearsal uses temporary package/extract roots and leaves the source
  checkout and release provider untouched.
- Bad: validating only the three binaries and TUI runtime while omitting the
  adapter or Argus archive from assertions.

### 6. Tests Required

- Parse workflow YAML, require `actionlint` `1.7.12`, and syntax-check every
  embedded shell `run` block.
- Run the tag/version/changelog validation logic with a representative tag and
  require the explicit `--no-publish` rehearsal guard.
- Run the release build command for the configured target.
- Package all three binaries plus the prepared `hya-tui-ts` runtime and
  production Compat adapter; verify `SHA256SUMS`, extract the archive, and run
  each binary smoke.
- Assert the TUI legal/client-present/server-absent runtime files and the Compat
  adapter's locked files and initialize/shutdown handshake.
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
  `{ id, reasoning: { default?, variants? } }`.
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
- Interactive TUI/server asks keep their existing channels. Headless `exec`, RPC, and goal modes reject residual asks; `--yolo` sets the effective invocation model to `danger` before engine construction.

### 3. Tests Required

- Evaluator tests cover all models, ordered regex matching, defaults, and invalid regexes.
- Dispatch tests cover lookup-before-ask, post-hook command matching, call correlation, and one prompt per invocation.
- Permission-plane tests cover exact native grants, legacy action grants, deny precedence, and the external-directory exception.
- Config/runtime tests cover omission, permission-only offline config, strict malformed-config fallback, yolo override, and fail-closed headless asks.

---

## Scenario: Legacy Prompt Variants And Agent Lifecycle Presentation

### 1. Scope / Trigger

- Trigger: changes to the legacy Compat message route, projected model variant,
  TypeScript subagent observation lifetime, or lifecycle status rendering.

### 2. Signatures

- `POST /session/{session_id}/message` accepts object-form `model` plus optional
  top-level `variant: string`.
- `resolveLifecyclePresentation(node)` returns a visible lifecycle `label` and
  a `working` flag from the existing member/roster projection.
- Observation panes close only through the workspace `close` action or
  `reconcileSessions` when the child session is absent.

### 3. Contracts

- A trimmed, non-empty top-level variant overrides an object model's nested
  variant before the existing model decoder and session switch run.
- Missing or empty top-level variants preserve nested variants. String-form
  models retain their existing behavior and ignore the separate variant.
- Lifecycle presentation prefers transient member status over roster status.
  `spawning`, `running`, and `busy` map to `Working`; `done` maps to `Finished`;
  `failed`, `cancelled`, and true idle remain distinct.
- Working rows show both visible text and the existing spinner. Terminal events
  update presentation but do not discard synchronized transcript content.
- Reasoning remains projection-backed; do not synthesize reasoning parts or add
  another lifecycle/message store.

### 4. Validation & Error Matrix

- Non-string top-level variant -> request deserialization error before prompt admission.
- Whitespace-only top-level variant -> preserve the nested object variant.
- Top-level variant with string-form model -> keep string-form compatibility;
  do not attach the separate variant.
- Member status present with stale roster `idle` -> render the member state.
- Session absent from successful reconciliation -> remove its observation pane.

### 5. Good/Base/Bad Cases

- Good: nested `low` plus top-level `high` records `high` on both the response
  user message and session model, then the TUI preserves and labels the finished
  observation.
- Base: nested-only and string-form models behave as before; an idle roster-only
  row displays `Idle` without a spinner.
- Bad: letting a missing response variant clear effort, preferring roster `idle`
  over member `running`, or removing a pane solely because a child completed.

### 6. Tests Required

- Route integration tests assert top-level precedence, nested/empty compatibility,
  string-form behavior, response projection, and session model state.
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
  `hya-core/src/engine/text_complete.rs`, and in `hya-server/src/compat/`:
  `event.rs` (SSE payload passthrough), `message_parts.rs` (x2),
  `session_context_messages.rs` (x2), `session_prompt.rs`.

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
- Enforced by `crates/hya/tests/version_metadata.rs`, which fails the workspace
  suite when any file below disagrees.

### 2. Contracts

Bumping the version means updating **all** of these together:

| File | What to change |
| --- | --- |
| `Cargo.toml` | `[workspace.package].version` |
| `Cargo.lock` | every `hya` / `hya-*` package version (a build refreshes it) |
| `crates/hya/tests/version_metadata.rs` | the `EXPECTED_RELEASE` constant |
| `README.md` | the `workspace version \`X.Y.Z\`` string |
| `packages/hya-tui-ts/package.json` | `"version"` |
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
- `SessionEngine::with_model_fallbacks(HashMap<ModelRef, Vec<ModelRef>>)` installs
  ordered category chains whose first candidate must equal the map key.

### 3. Contracts

- Recovery is allowed only before an `EventStream` exists. A returned stream is
  the strict no-replay boundary for request retries, route failover, model
  failover, and auth refresh.
- One HTTP route uses at most three request attempts for transport errors, 429,
  and 5xx. A valid bounded `Retry-After` overrides exponential jittered backoff.
- A pre-stream 401/403 may force-refresh once, only while an attempt slot remains.
  Header resolution runs again and the token must differ from the failed value.
- Router failover preserves model identity and advances to the next matching
  route only after a retryable pre-stream failure.
- Core model fallback re-enters the router with the next category candidate on a
  retryable pre-stream error or `UnknownModel`; it never consumes non-retryable
  protocol, compatibility, decode, or human-action auth errors.
- The header deadline is a retryable transport failure. The SSE idle deadline is
  delivered once on the established stream and is not retryable. A stream that
  keeps producing frames has no total lifetime deadline.

### 4. Validation & Error Matrix

- Invalid fallback chain head -> ignore that chain; the preferred model keeps
  single-model behavior instead of partially honoring an unsafe order.
- Transport/header timeout, 429, or 5xx before stream -> bounded same-route
  retry, then matching-route/model-chain failover when available.
- 401/403 with no refresher, failed refresh, unchanged token, or no remaining
  attempt -> original status error; no synthetic auth success.
- `AuthExpired`, incompatible request, decode error, or other non-retryable
  failure -> surface immediately without advancing a route/model chain.
- SSE idle after headers -> one stream error; zero replay or failover.

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
- Router tests cover matching-route order, retryable/non-retryable classification,
  and zero failover after stream construction.
- Core tests cover configured chain order, forward suffixes, `UnknownModel`,
  non-retryable termination, and no fallback after stream construction.
- SSE tests cover first-frame idle, inter-frame reset, one timeout error, and a
  continuously active stream with no total lifetime cap.

### 7. Wrong vs Correct

#### Wrong

```rust
// A total timeout crosses the replay boundary and aborts healthy long streams.
timeout(Duration::from_secs(300), provider.stream(request, session, message)).await
```

#### Correct

```rust
// Bound headers before stream ownership; bound silence inside the stream pump.
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
- Directory traversal then calls `assert_external_directory(ctx, &root, true)`
  before reading, walking, or mutating the target.
- An omitted `find.path` means the bound workdir; it does not mean process cwd.

### 3. Contracts

- Relative paths resolve under the Session workdir.
- Absolute in-workdir paths and the omitted workdir default proceed without an
  external-directory grant.
- Absolute paths and `..` traversal outside the workdir use the same permission
  plane as every other file tool. A tool must not normalize away the escape and
  then operate directly.
- Containment is an authorization rule, not a search-result filter: reject the
  root before any partial result is returned.

### 4. Validation & Error Matrix

- Relative in-workdir path -> resolve and execute.
- Absolute in-workdir path -> execute.
- Absolute out-of-workdir path without grant -> `ToolError::Permission`.
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
assert_external_directory(ctx, &root, true).await?;
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
- An empty list makes one bounded optional-auth discovery sequence each startup.
- Router, engine, CLI, HTTP/bootstrap, SDK, and TUI consume one immutable
  snapshot. Only an all-zero-live snapshot contains `hya/offline`.
- Discovery never writes config/cache or reads foreign product configuration.

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
