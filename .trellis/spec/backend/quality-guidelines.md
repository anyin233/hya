# Quality Guidelines

> Code quality standards for backend development.

---

## Overview

<!--
Document your project's quality standards here.

Questions to answer:
- What patterns are forbidden?
- What linting rules do you enforce?
- What are your testing requirements?
- What code review standards apply?
-->

(To be filled by the team)

---

## Forbidden Patterns

<!-- Patterns that should never be used and why -->

(To be filled by the team)

---

## Required Patterns

<!-- Patterns that must always be used -->

(To be filled by the team)

---

## Testing Requirements

<!-- What level of testing is expected -->

(To be filled by the team)

---

## Code Review Checklist

<!-- What reviewers should check -->

(To be filled by the team)

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

## Scenario: GitHub Release Binary Workflow

### 1. Scope / Trigger

- Trigger: any change that publishes release binaries, creates GitHub Releases, or modifies the release changelog process.
- Applies to `.github/workflows/release.yml`, root `CHANGELOG.md`, `docs/changes/`, root `AGENTS.md` release rules, and release-related task artifacts.

### 2. Signatures

- Release tag: `vX.Y.Z`, where `X.Y.Z` must match Cargo's `hya` package version.
- Cargo command: `cargo build --release --locked -p hya -p hya-backend -p hya-ts --bins --target x86_64-unknown-linux-gnu`.
- Release archive: `hya-<version>-x86_64-unknown-linux-gnu.tar.gz`.
- Checksum file: `SHA256SUMS` generated beside the release archive.

### 3. Contracts

- Root `CHANGELOG.md` contains only the newest version's release notes.
- Historical changelogs live under `docs/changes/CHANGELOG_<version>.md`.
- The GitHub Release body is read verbatim from root `CHANGELOG.md`.
- Release workflow permissions are read-only by default; only the release publishing job may request `contents: write`.
- Build provenance attestations are generated for the archive and checksum.
- Third-party release actions are pinned to immutable commit SHAs.
- The publishing job uses the `release` environment so repository settings can require manual approval.

### 4. Validation & Error Matrix

- Missing `v` tag prefix -> fail before build.
- Tag version is not semver-shaped -> fail before build.
- Tag version differs from `cargo metadata` package version for `hya` -> fail before build.
- Missing or empty `CHANGELOG.md` -> fail before publishing.
- `CHANGELOG.md` first heading differs from the tag version -> fail before build.
- Build, archive, checksum, or packaged-binary smoke failure -> skip release publishing.
- Missing release assets -> fail `softprops/action-gh-release` with `fail_on_unmatched_files: true`.

### 5. Good/Base/Bad Cases

- Good: `v0.1.0`, `[workspace.package].version = "0.1.0"`, root `CHANGELOG.md` contains only `0.1.0` notes, archive and checksum pass smoke checks.
- Base: first release has no historical changelog; keep `docs/changes/.gitkeep` and root `CHANGELOG.md` for the current version.
- Bad: appending old release notes to root `CHANGELOG.md`; this publishes stale history as the GitHub Release body.

### 6. Tests Required

- Parse workflow YAML, run `actionlint`, and syntax-check every embedded shell `run` block.
- Run the tag/version/changelog validation logic with a representative tag.
- Run the release build command for the configured target.
- Package all three binaries plus the prepared `hya-tui-ts` runtime, verify
  `SHA256SUMS`, extract the archive, run each binary smoke, and assert the legal,
  client-present, and server-absent runtime files.
- Confirm third-party actions are pinned to commit SHAs and release publication uses the `release` environment.

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

- Provider kinds: `openai-completion` and `openai-response`; `openai` and
  `openai-compatible` remain Chat Completions aliases.
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
