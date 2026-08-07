# Runtime

The runtime lives in [`../../crates/hya-core`](../../crates/hya-core). Its
central type is `SessionEngine` in
[`engine.rs`](../../crates/hya-core/src/engine.rs).

## `SessionEngine`

`SessionEngine` owns:

- `SessionStore` for persistence.
- `ProviderRouter` for model streaming.
- `RuntimeRegistry` for one atomically published immutable tool/skill/MCP
  snapshot, including source ownership metadata for MCP/plugin tool
  contributions. `ToolRegistry` is only an offline candidate builder.
- `PermissionPlane` for allow/ask/deny decisions.
- `InteractionPlane`, `SpawnerPlane`, `TodoPlane`, `WebSearchPlane`,
  `LspPlane`, and `FormatterPlane` for cross-cutting tool services.
- `EventBus` for live subscribers.
- optional hook dispatcher for plugins.

### Publish seams

Runtime events do **not** all pass through a single `emit` path. There are
three write/publish seams:

1. **`emit`** — appends the event to SQLite, takes the returned sequence
   number, **then** publishes an `Envelope` with that seq. A live observer
   never sees a non-durable event on this path.
2. **`publish_live`** — publishes an `Envelope` at `seq: 0` with **no** store
   write. Used only for high-frequency **text** streaming
   (`TextStart` / `TextDelta` / `TextEnd`, and live `TextReplace` from the
   `text_complete` hook). At round end those text parts are re-emitted
   **durably** as a `TextStart` / `TextReplace` / `TextEnd` triple. Reasoning
   and other non-text stream events are **not** live-only: they go straight to
   `emit_for_actor` and are durable on first emit (no reasoning re-emission
   loop).
3. **`emit_for_actor`** — the fencing seam for resident work: when given
   `Some(&ActorClaim)` it routes through `commit_resident_mutation` (fenced
   SQLite commit, publish only after commit); when `None` it falls through to
   plain `emit`. Every resident-originated event goes through this method;
   transient turns pass `None` and never look up an actor claim.

All three eventually call **`publish_envelope`**, the single publish seam: it
dispatches to global hooks (`HookDispatcher::dispatch_event`), then activation
(sidecar) hooks for the envelope's session, then the `EventBus`.

### `AgentSpec`

`AgentSpec` is the resolved agent for one turn: `name`, `model`,
`system_prompt`, `workdir`, and optional `reasoning` effort. It is what a
`TurnBinding`'s agent resolution produces and what the server holds as its
process-level default for new sessions.

### `RuntimeCatalogRefresh`

`RuntimeCatalogRefresh` is the trait hook `bind_root_runtime` calls before a
**root** turn binds its snapshot. `hya-app` implements it so the installed
bundle catalog can refresh when the registry generation changed. It fires only
on root binds; child/bound turns reuse the parent's pinned `TurnBinding` and
never consult the registry.

## Session Creation

`create` mints a `SessionId` and emits `Event::SessionCreated` with:

- optional parent session
- agent name
- model reference
- workdir

`create_with_id(id, spec)` is **idempotent**: if the supplied id already has
events in the log it returns immediately without re-emitting `SessionCreated`.
That makes resume, fork, and recovery paths safe to call unconditionally.

`create_for_actor` is the `ActorClaim`-fenced variant: it commits
`SessionCreated` through `commit_resident_mutation` under the claim.

Parent sessions are used by goal, loop, and team-related helpers to keep child
runs connected to a lead session.

## Prompt Admission

`admit_user_prompt` writes a complete user message as:

1. `MessageStarted`
2. `TextStart`
3. `TextDelta`
4. `TextEnd`
5. `MessageFinished`

The same shape is used by `inject_system_message` for system messages.
`admit_command_prompt` records command metadata while admitting a user message.

`record_user_prompt_context` emits `UserPromptContextRecorded { files, agents }`
for Compat-compatible v2 prompt admission. It **short-circuits** to `Ok(())`
and emits **nothing** when both vectors are empty — a prompt with no
`@mentions` leaves no context event in the log, and consumers must not expect
one per user message. When present, that metadata is replayed through the
projection and provider request builder.

## Session-state mutators

These methods are thin single-event emitters (append + publish) with no other
side effects:

| Method | Event |
| --- | --- |
| `switch_agent` | `AgentSwitched` |
| `switch_model` | `ModelSwitched` |
| `set_title` | `SessionTitled` |
| `set_workdir` | `SessionMoved` |
| `set_metadata` | `SessionMetadataSet` |
| `set_permission` | `SessionPermissionSet` |
| `set_archived` | `SessionArchived` |
| `set_share` | `SessionShareSet` |
| `clear_share` | `SessionShareCleared` |
| `delete_message` | `MessageDeleted` |
| `delete_part` | `PartDeleted` |
| `replace_text_part` | `TextReplace` |
| `replace_reasoning_part` | `ReasoningReplace` |
| `update_tool_part` | `ToolPartUpdated` |

## Assistant Turn Loop

### Turn activation modes

`TurnActivation` selects how a turn obtains its runtime binding:

| Mode | Behavior |
| --- | --- |
| `Root` | Re-binds the runtime (may start a sidecar) |
| `Bound` | Reuses a `TurnBinding` captured by the parent turn |
| `Resolved` | Reuses pre-resolved agents, resource policy, and sidecar tools |

### Entry points

| Entry point | Activation | Notes |
| --- | --- | --- |
| `run_turn` | `Root` | Interactive / top-level path |
| `run_turn_with_external_dirs` | `Root` | Adds external-directory permission rules |
| `run_turn_with_external_dirs_and_guidance` | `Root` | Also attaches request-scoped guidance |
| `run_bound_turn` | `Bound` | Child path reusing parent binding |
| `run_bound_turn_for_actor` | `Bound` | Resident path with `ActorClaim` |
| `run_resolved_turn_with_sidecar_tools` | `Resolved` | Child with pre-resolved sidecar tools |
| `run_resolved_turn_with_sidecar_tools_for_actor` | `Resolved` | Resident resolved path |

The `_for_actor` variants carry an `ActorClaim` and are the resident path.

After prompt admission succeeds, a root turn resolves the session workdir,
refreshes its skill candidate if the logical view changed, and captures one
`TurnBinding`. It then records `MessageStarted` and
`TurnBindingRecorded { generation }`. The binding retains an `Arc` to the
complete immutable runtime snapshot for the entire assistant turn.

### Per-round sequence

Each round runs **in this order** (see `run_turn_rounds` in
[`engine/turn.rs`](../../crates/hya-core/src/engine/turn.rs)):

1. Validate the actor claim (if any).
2. Check activation-hook health (`is_healthy`); unhealthy → `CoreError::Cancelled`.
3. Check the cancel token; if cancelled, emit `MessageFinished` with
   `FinishReason::Cancelled` and return.
4. Read the current projection from the store.
5. Maybe compact context (see [Compaction and Summaries](#compaction-and-summaries)).
6. Run the `chat_params` hook (may rewrite the `CompletionRequest`).
7. Acquire a governor stream permit (reserved or general by depth).
8. Emit `StepStarted`.
9. Stream the provider round (`collect_stream_round`) — live **text** via
   `publish_live` (then durable text triple at round end); reasoning, tool
   calls, and other events via durable `emit_for_actor` immediately.
10. Emit `StepFinished`.
11. **Drop the stream permit** before any tool work.
12. If the round produced no tool calls, emit `MessageFinished` and end the turn.
13. Otherwise run the tool-dispatch pipeline (below), then repeat.

If a provider round produces tool calls, the engine starts another round with
the updated projection. The turn continues until the provider finishes,
cancellation is observed, or execution returns an error.

> **Stream permit lifetime (deadlock invariant)**  
> The governor stream permit is held **only** around provider streaming and is
> dropped **before** tool execution. A member blocked inside the `task` tool
> waiting on its children holds no permit, so nested fan-out cannot deadlock
> the semaphore. Moving tool dispatch inside the permit scope reintroduces
> that deadlock.

### Stream permit class by session depth

Depth is derived from the session parent chain:

- **Depth 0** (root / interactive) takes a **reserved** stream permit
  (`acquire_reserved_stream`).
- **Depth > 0** (subagent) takes a **general** stream permit
  (`acquire_general_stream`).

General work cannot borrow from the reserved pool, so root progress never
queues behind background subagent work.

Defaults (see [`orchestrator.rs`](../../crates/hya-core/src/orchestrator.rs)):

- `DEFAULT_GENERAL_STREAM_PERMITS = 100` (`max_concurrency` normalized to
  `1..=100`)
- `RESERVED_STREAM_PERMITS = 28` (fixed)

Together that is a 128-permit live stream budget.

### Tool dispatch pipeline

For each `ToolCallRequested` collected in the round, in order:

1. Re-validate the actor claim.
2. **`tool.execute.before` hooks** — global plugin host first, then activation
   (sidecar). A `Veto { reason }` produces `ToolError` with message
   `blocked by plugin: <reason>` and the tool never runs.
3. After the before-hook batch, re-check activation-hook health (unhealthy →
   cancel the turn).
4. `resolve_tool` against the bound runtime resource view.
5. Permission authorize.
6. **Re-validate the actor claim at the dispatch boundary** — a stale resident
   cannot dispatch a tool even if it passed validation at round start.
7. Execute the tool.
8. **`tool.execute.after` hooks** — may rewrite the outcome (unless the error
   was a permission denial, which is preserved).
9. Re-check activation-hook health after the after-hook batch.
10. `cap_tool_output` on success so one oversized result cannot blow the next
    model context window.
11. Emit `ToolResult` or `ToolError`.

Formatter/LSP post-edit work for file mutations runs through the tool planes
when configured (inside tool execution, not as a separate round step).

### Root-turn admission cleanup

A completed depth-0 turn calls `finalize_root_spawn_admissions(root)`, which:

- cancels every live governor operation for that root
- cancel-finalizes every nonterminal admission journal row
- releases the root's per-run subagent budget entry

Without this, a long-lived root session leaks budget and never recovers spawn
capacity.

### Runtime registry publication

Refresh builds and validates a complete candidate while the active snapshot
remains readable, then replaces the active `Arc` once. Only a changed,
successful candidate advances `ConfigGeneration`; failure and logical no-op
leave both generation and effective view unchanged. New turns bind the
published snapshot, while in-flight turns continue on their retained snapshot
without a dispatch-path registry lock. Direct shell turns use the same binding
and audit event.

#### `ToolRegistrySnapshot` and dispatch identity

A turn takes an immutable, lock-free `ToolRegistrySnapshot` of the tool
registry so tool resolution cannot change mid-turn. `ToolRegistry` remains
the offline candidate builder; only the snapshot is live.

Each builtin entry carries a SHA-256 **dispatch identity** computed over:

- the domain string `hya.tool.builtin-dispatch/v1`
- the `hya-tool` crate version (`CARGO_PKG_VERSION`)
- the canonical tool name

MCP and plugin tools receive a **per-source** identity instead
(`runtime_source_dispatch_identity` over source kind, configured id,
declaration digest, resources, and export names).

`ToolRegistry::logically_matches` compares a candidate builder against a
published snapshot using tool maps, alias maps, and those dispatch identities.
That is how a reconciliation can tell a no-op refresh from a real change
without diffing full tool schemas.

#### Permission policy semantic identity

`PermissionPlane::semantic_identity_v1` produces a SHA-256 fingerprint over:

- the domain string `hya.permission.semantic-identity/v1`
- the snapshot resource rules
- the invocation model and compiled invocation rule selectors
- the installed interceptor's own identity (if any)

Any change to the effective policy — including swapping the interceptor —
changes the fingerprint, so a reload can detect that permissions actually
changed rather than re-deriving the rule set.

`hya-app::RuntimeReconciler` owns only desired/observed coordination for MCP
and startup plugin declarations. It has no resolve or dispatch API and caches
no effective tool set. Stable source IDs are `(mcp|plugin, configured_id)`;
the effective source manifest, declaration digest, client/child owner, and
exports live only in `RuntimeSnapshot`. Startup, deferred MCP, and Compat MCP
control all submit to this reconciler. Complete current-revision candidates
publish through `RuntimeRegistry`; stale successes are dropped, failures leave
the prior generation unchanged, and explicit removals publish before unrelated
additions. The publication closure always starts from the current snapshot, so
it cannot overwrite a concurrent skill refresh with an older candidate.

#### Deferred MCP startup

When sideplane deferral is enabled (`HYA_DEFER_SIDEPLANES` defaults to on;
set to `0`/`false`/`off`/`no` to disable) **and** MCP servers are configured,
plugins are reconciled synchronously while MCP connection is moved to a
background task. A slow or hanging MCP server cannot block hya from starting.
The user-visible consequence is that MCP tools may not be present for the very
first turn. A refresh rejected by the reconciler is non-fatal and only prints
`hya: MCP runtime refresh rejected` to stderr.

#### MCP resources

Beyond tools, hya performs a best-effort `resources/list` per connected MCP
server and exposes the result through `McpManager::resources()`. Each entry is
keyed `<sanitized server>:<sanitized resource name>`, where sanitizing keeps
ASCII alphanumerics, `_`, and `-`, and replaces every other character with
`_`. Each value carries a `client` field naming the owning server.

Resources are **not** registered as tools and are **not** reachable by the
model through the tool registry today.

Plugin reconciliation in `0.34.6` covers startup tool exports and their RPC
binding. Plugin hook/command/permission callback lifecycle remains owned by the
existing `PluginHost` and `PermissionPlane`; there is no dynamic hook plane,
plugin watcher, or plugin hot-reload API. A respawn must reproduce the complete
canonical initialize declaration or the new child is closed and calls fail
closed.

`RuntimeSnapshot` owns exactly one `BundleCatalog`. For installed bundles,
`hya-app` reads the bundle registry generation before binding each new root turn
and before TUI/catalog refresh, builds one complete built-ins-plus-installed
public candidate, and publishes it atomically. An unchanged generation
is a no-op; validation or load failure preserves the old snapshot. In-flight
turns and child turns retain their pinned snapshot. There is no bundle watcher,
per-provider-round or per-tool-call database check, or second catalog authority.

Core preserves that child pinning with a typed `BoundSpawnRequest` carrying the
parent `TurnBinding` through the application supervisor into both transient and
resident execution. The child path does not query the bundle registry database
for a generation or bind whichever runtime is current when a queued request
runs.

## AgentBundle activation sidecars (0.34.11)

`RuntimeSnapshot` retains the sole `Arc<BundleCatalog>` authority and the
shipped `hya-core -> hya-bundle` dependency. For an executable public Bundle,
`hya-app` resolves and materializes the validated resources from the captured
snapshot/`TurnBinding` and constructs the activation-bound factory. The
core-facing start request carries only `activation_id` and `lifecycle`; the
factory returns an opaque lifecycle handle. It introduces no Bundle, package,
path, digest, or `hya-plugin` types. `hya-plugin` owns the child, stdio,
bounded stderr, shutdown, termination, and reap. `hya-app` depends on and
coordinates `hya-core`, `hya-plugin`, and existing `hya-bundle` catalog types.
Dependency directions remain `hya-core -> hya-bundle`, `hya-plugin -> hya-core`,
`hya-app -> hya-core`, and `hya-app -> hya-plugin`; `hya-bundle` remains
independent and no `hya-core -> hya-plugin` edge is added.

Prepared canonical hook IDs are limited to `event`, `tool.execute.before`, and
`tool.execute.after`. Selected Tool/Hook resources exact-path join to exactly
one JS Extension in the referenced resource's owning bundle; the captured
`TurnBinding` determines a deduplicated canonical entrypoint list, and
staged-but-unselected Extensions never activate. Independently initialized
Tool and Hook sets must each exactly equal the selected expected sets before
model polling. A static selected view starts no process, and old bindings stay
generation-pinned. There is no second resolver, catalog, DTO, or import scan.

Harness remains the sole agent/model/task/mailbox/event/`MemberOutcome` and
recovery runtime. The sidecar wire is newline-delimited JSON-RPC 2.0 using hya
plugin protocol version 1. Initialize remains request/reply: initialize retains
existing `protocol_version` and `host` fields, and the only activation-specific
metadata is `{ activation_id, lifecycle }`. Declaration drift fails before
Running or model polling. `tool/call` and `hook/*` are request/reply and `event` is a
one-way notification without an id or result.

### Sidecar lifecycle cleanup

A turn ending with `FinishReason::Stop` or `FinishReason::Length` calls
`SidecarHandle::shutdown()` (graceful). **Every other outcome** — error,
cancellation, or a round that ended in tool calls without completing — calls
`terminate()`. Sidecar authors must assume `terminate()` is the common path on
abnormal exit and must not rely on shutdown-time flushing for durability.

A transient activation owns one child through its whole activation and then
shuts down/reaps it. A healthy resident reuses one child across mailbox
messages; idle loss lazily creates a fresh child, running loss aborts and
fences the current item without replay, and queued-after work resumes with a
fresh ACK on the same pinned binding. Explicit stop is final and idempotent,
canceling queued work, removing the resident, and releasing its claim. There
is no TTL, heartbeat, reclaim, second runtime, or persisted process state.

### Activation-hook health gate

A task-local activation hook dispatcher is scoped to one session and runs
**in addition to** (not instead of) the global plugin host for
`tool.execute.before` / `tool.execute.after` (and event dispatch).

The turn loop checks the activation hook dispatcher's `is_healthy()`:

- at the top of every round
- again after each before/after hook batch

An unhealthy dispatcher aborts the turn with `CoreError::Cancelled` (not a
hard error). That is why a lost sidecar shows up to clients as
`MessageFinished { Cancelled }` rather than `{ Error }`.

## Resident recovery and actor fencing

Resident subagents retain the immutable runtime `TurnBinding` behavior above,
but additionally carry an internal `ActorClaim` containing their stable session
identity, current monotonic epoch, and per-process owner identity. Actor epochs
and runtime configuration generations are independent: takeover does not
terminate old snapshot owners, and a runtime refresh does not take over an
actor.

Before runtime readiness, `hya-app` advances all active resident claims, aborts
non-actor admissions through the existing startup seam, folds the canonical
projections, terminalizes each old epoch's actor-bound admissions and running
work through the recovered claim, and recreates the existing
`ResidentSupervisor` slots.
Committed queued mail resumes under the new epoch; work that crossed the
durable start marker is aborted and never automatically retried.

The single fencing write seam is **`emit_for_actor`**: with `Some(claim)` it
routes to `commit_resident_mutation` (fenced, publish-after-commit); with
`None` it falls through to plain `emit`. Provider/tool events, mailbox writes,
spawn admissions, and child transitions use that claim-aware path. A stale
claim returns `StaleActorClaim` without appending or waking successful work.
Full-tuple claim release atomically aborts any still-bound admission before the
claim becomes reusable; only the first logical release refunds a live governor.

The claim is TTL-free and local to one harness process incarnation. There is no
heartbeat, wall-clock expiry, lease daemon, distributed coordination, HA, or
active-active behavior. Canonical-state fencing cannot promise exactly-once
filesystem/network/API side effects, and this release does not certify the
future 100/256 workload envelope.

## Turn termination guarantees

`run_turn` receives a `CancellationToken`. Whenever the turn ends with the
assistant message still open (or after a hard failure path), the engine
force-emits `MessageFinished`:

| Path | Finish reason |
| --- | --- |
| Cancel token observed before/during a round | `Cancelled` |
| Sidecar loss token fires while the message is open | `Cancelled` |
| Non-cancel provider/tool error after `MessageStarted` | `Error` |
| Normal completion with no further tool calls | provider finish reason |

Contract for clients: a UI that has seen `MessageStarted` is guaranteed to
eventually see `MessageFinished`, so it never spins forever waiting for a
finish event.

The shell tool also checks the token before spawning a command and kills the
spawned Unix process group on cancellation.

## Compaction and Summaries

Compaction lives in [`compaction.rs`](../../crates/hya-core/src/compaction.rs)
and is driven from the turn loop when
`needs_compaction` fires (`messages.len() > keep_recent` and estimated tokens
exceed `token_threshold`).

`CompactionConfig` fields:

- `token_threshold` (default `100_000`)
- `keep_recent` (default `6`)

`SummarizeOptions` fields for local summarizer calls:

- `system: Option<String>`
- `model: Option<ModelRef>`
- `reasoning: Option<ReasoningEffort>`

### Two-tier compact path

When the window is over threshold, the turn:

1. **Resolves the fixed `compaction` system agent** from the bound catalog.
   Missing definition **fails closed** (`AgentDefinitionMissing`) before any
   provider compact call.
2. **Tier 1 (native):** calls `ProviderRouter::compact_if_supported`, which
   resolves the model's provider and delegates to
   `Provider::compact_responses`. Only `openai-response`, `openai-codex`, and
   `grok-build` routes advertise support. The endpoint is derived by appending
   `/compact` to an endpoint already ending in `/responses`, otherwise
   `/responses/compact` on the trimmed endpoint. The call POSTs
   `{ model, input }` (optional system item prepended) and requires an
   `output` array in the reply, else it fails with
   `Decode("responses compact reply missing output array")`. Success yields a
   `CompactedWindow` whose item array is persisted as a system message via
   `format_responses_compact_system` — body starts with `HYA_COMPACTED_CONTEXT`,
   then `<<<RESPONSES_COMPACT_ITEMS>>>`, then the JSON array. Subsequent
   `/responses` requests re-inject those items verbatim into `input`.
3. **Tier 2 (fallback):** on `Ok(None)` (no compact endpoint) or **any** error,
   the turn falls back to the local `ModelSummarizer` via `compact_with`. That
   helper returns an **in-memory** `Vec<Message>` for the current round only:
   older messages are replaced by a system message of the form
   `Summary of {n} earlier messages:\n{summary}` — **no**
   `HYA_COMPACTED_CONTEXT` marker, and **no** store write. The turn assigns
   `messages = compacted` for the next provider request; the fallback is
   recomputed from scratch whenever thresholds still require it.

The `HYA_COMPACTED_CONTEXT` marker (and the history drop in
`compacted_messages` that selects on that prefix) is written only by durable
paths: **Tier 1** native Responses compact inject, and the explicit
`SessionEngine::compact_context` / `/compact` path
(`engine/summary.rs`), not by Tier 2 `compact_with`.

The CLI exposes local compact via `/compact`; legacy Compat summarize routes
persist the same native summary shape.

## Session Titles

`auto_title_session` issues a separate provider completion to generate a
session title. Guards:

1. **Root sessions only** — children (`parent.is_some()`) are skipped.
2. Skips any session that already has a non-default / non-fallback title.
3. Requires **exactly one** user message in the projection (multiple user
   messages → no title).

It resolves the fixed `title` system agent from the bound catalog and calls
the provider at `temperature: 0.0` with `max_output_tokens: 128`, honoring the
definition's model and reasoning effort (falling back to the caller's model
when the definition has none). Then it emits `SessionTitled`.

This is an extra billed provider call per successful title generation, on the
route resolved for that model.

### Fixed system agents

`FixedSystemAgent` is the closed set of Harness system operations:

| Id | Use |
| --- | --- |
| `compaction` | Over-threshold context compact |
| `title` | Auto session title |
| `summary` | Explicit summarize path |

Callers cannot pass an arbitrary agent id into these seams.

### Empty-session cleanup

`cleanup_empty_unnamed_session` deletes a session if and only if
`title::is_empty_unnamed_session` holds: the session exists, has **no** title,
and has **no** messages (no user content and no assigned title). It calls
through to `SessionStore::delete_session`, which removes `token_ledger` rows
then `event_log` rows in one transaction.

## Shell turns

`run_shell` admits a shell user message, binds the root runtime, then emits a
full synthetic assistant message around one `shell` tool call:

1. `MessageStarted`
2. `TurnBindingRecorded`
3. `ToolInputStart`
4. `ToolCallRequested` (after optional `tool.execute.before`; a veto emits
   `ToolError` and finishes with `Error`)
5. `ToolResult` or `ToolError`
6. `MessageFinished`

There is **no** provider call, so no `StepStarted` / `StepFinished` and no
stream permit is taken.

## Forking a session

`copy_messages_to_session` replays a source `Projection` into the target
session as **fresh** events with newly minted `MessageId` / `PartId` per copy
(ids are never reused), stopping before an optional `before` message id.

Tool parts cannot be replayed as their original streaming events, so they are
recreated as `ToolInputStart` followed by `ToolPartUpdated` carrying the final
`ToolPartState`.

Consequence: a forked session's event log is not byte-identical to the
source's, and its sequence numbering is independent.

## Hooks

[`hooks.rs`](../../crates/hya-core/src/hooks.rs) defines `HookDispatcher`, the
runtime hook boundary used by `hya-plugin` and activation sidecars.

### `HookDispatcher` methods

There is **no** permission-ask method on `HookDispatcher`. Permission
callbacks are owned by `PermissionPlane` and the `hya-plugin` permission
bridge (see below).

| Method | Input | Outcome |
| --- | --- | --- |
| `dispatch_event` | `&Envelope` | (void) — fires for **every** published envelope, including seq-0 live-only ones |
| `is_healthy` | — | `bool` (default `true`) |
| `command_execute_before` | `CommandExecuteBeforeInput { session, command, arguments, text }` | `Continue { text }` |
| `text_complete` | `TextCompleteInput { session, message, part, text }` | `Continue { text }` |
| `message_user_before` | `MessageUserBeforeInput { session, text }` | `Continue { text }` |
| `chat_params` | `ChatParamsInput { session, message, request }` | `Continue { request }` |
| `tool_execute_before` | `ToolExecuteBeforeInput { session, message, call, tool, input }` | `Continue { input }` or `Veto { reason }` |
| `tool_execute_after` | `ToolExecuteAfterInput { …, result: ToolOutcomeNative }` | `Continue { result }` where result is `Ok { output, time_ms }` or `Err { message }` |

The CLI installs a `PluginHost` when `plugins:` are configured.

### `tool.execute.before`

Runs before every model-issued tool call and also for direct shell turns. A
hook may return rewritten input JSON (replaces the model's arguments) or a
`Veto` with a reason. A veto skips execution entirely and emits a `ToolError`
whose message is `blocked by plugin: <reason>`. Both the global plugin host
and the per-session activation (sidecar) dispatcher run this hook (global
first).

### `text_complete`

The engine accumulates streaming text per `PartId` in a `TextPartAccumulator`.
On `TextEnd`, the `text_complete` hook is offered the accumulated text and may
return replacement text. A rewrite:

1. publishes a live `TextReplace` (`publish_live`, seq 0), and
2. changes what is persisted in the durable
   `TextStart` / `TextReplace` / `TextEnd` triple at round end

So this hook can alter the stored transcript, not just the display.

### `permission.ask` (plugin bridge, not `HookDispatcher`)

`PermissionBridge` implements `PermissionInterceptor` over the plugin host
(wired in `hya-app` when plugins are present). Every plugin that declared the
`permission.ask` hook is polled in declaration order; the **first non-`defer`
reply decides** and later plugins are not consulted.

Valid wire outcomes (`outcome` tag, snake_case):

- `allow_once`
- `allow_always`
- `reject` with optional `feedback` string (folded into
  `PermissionError::Denied` as `— user says: <feedback>` for the model)
- `defer` — try the next plugin

If every plugin replies `defer` (or none declare the hook), the request falls
through to the normal user prompt.

### Activation hooks

A task-local activation hook dispatcher is scoped to one session and runs
alongside the global host for the same tool before/after hooks and event
dispatch. If it reports unhealthy while a tool call is in flight, the whole
turn is cancelled rather than silently continuing without the sidecar's hooks
(see [Activation-hook health gate](#activation-hook-health-gate)).

## Goal Mode

Goal mode lives in [`completion.rs`](../../crates/hya-core/src/completion.rs).
It uses three pieces:

- `IterationDriver`: generic loop runner with safety caps.
- `LeadTurnExecutor`: admits the next directive into the lead session and runs a
  turn.
- `GoalGate`: asks a `GoalEvaluator` whether the transcript satisfies the goal.

`ModelGoalEvaluator` issues a tool-free `CompletionRequest` at
`temperature: 0.0` and `max_output_tokens: 256`, asking the model for a strict
JSON verdict:

```json
{"met": true, "reason": "..."}
```

This is a **separate** provider call per gate evaluation and therefore bills
against the same route as the evaluator's configured model (not free).
Malformed evaluator output is treated as not met, so it counts toward caps
rather than causing an unbounded loop.

## Loop Mode

Loop mode lives in [`loop_mode.rs`](../../crates/hya-core/src/loop_mode.rs).
It is a lower-level planner/verifier loop:

- `LoopVerifier` grades transcript evidence.
- `LoopPlanner` proposes the next directive.
- `LoopGate` is the only component allowed to stop for success.
- `cost_preflight` rejects budgets outside the hard ceiling before workers run.

Current guards include:

- explicit budget
- satisfaction threshold
- evidence-quality requirement
- no-progress detection
- repeated-directive detection unless the planner marks a strategy change

The current CLI exposes goal mode directly; loop mode is available as core
runtime API.

## Teams, Members, and Workspaces

Team-related code is split across:

- [`subagent.rs`](../../crates/hya-core/src/subagent.rs)
- [`workspace.rs`](../../crates/hya-core/src/workspace.rs)
- [`category.rs`](../../crates/hya-core/src/category.rs)

There is no `team.rs` and no in-memory `TeamControlPlane`. Inter-agent mail and
channels are event-sourced (`MailSent`, `ChannelJoined`, `ChannelLeft`) and
folded by `hya-proto::Projection` (see
[ADR 0001](../adr/0001-event-sourced-mailbox-and-channels.md)).

`run_team` runs member specs in child sessions and returns bounded evidence
summaries. It intentionally does not project full child transcripts into the
lead session.

`WorktreeManager` allocates git worktrees under `.hya/worktrees` and only cleans
up paths it recorded as owned.

These primitives are present in `hya-core`; the shipped CLI exposes the main
TUI, single-turn/run aliases, goal, server, replay, sessions, catalog/auth, and
JSONL RPC surfaces.

## Errors

`CoreError` variants
([`error.rs`](../../crates/hya-core/src/error.rs)):

| Variant | Meaning |
| --- | --- |
| `Bundle` | Bundle catalog / prepare failure |
| `Provider` | Provider stream or compact failure |
| `Tool` | Tool execution failure |
| `Store` | SQLite / session store failure |
| `RuntimeRefresh` | Runtime candidate publication rejected |
| `Cancelled` | Turn cancelled (including activation-hook health loss) |
| `AgentDefinitionMissing { agent_id }` | Fixed system agent not in catalog |
| `Invalid(String)` | Other invalid runtime state |

`hya-server` maps every `CoreError` to HTTP 500 via `ApiError::internal`
except where a Compat route translates the error itself. `Cancelled` and
`AgentDefinitionMissing` are therefore **not** distinguishable over the native
HTTP mapping today.
