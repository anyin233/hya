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

All runtime events pass through `SessionEngine::emit`, which appends to the
store and publishes the same envelope to the bus.

## Session Creation

`create` mints a `SessionId` and emits `Event::SessionCreated` with:

- optional parent session
- agent name
- model reference
- workdir

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
Compat-compatible v2 prompt admission can attach file and agent metadata that
is replayed through the projection and provider request builder.

## Assistant Turn Loop

After prompt admission succeeds, `run_turn` resolves the session workdir,
refreshes its skill candidate if the logical view changed, and captures one
`TurnBinding`. It then records `MessageStarted` and
`TurnBindingRecorded { generation }`. The binding retains an `Arc` to the
complete immutable runtime snapshot for the entire assistant turn.

The turn then repeatedly:

1. Reads the current projection from the store.
2. Builds a provider request from projection messages plus the bound prompt
   skills and tool schemas.
3. Streams provider events.
4. Appends text, reasoning, and tool-input events.
5. Collects `ToolCallRequested` events.
6. Resolves and executes requested tools through the same binding, with
   permission checks and plugin/MCP bridges.
7. Runs formatter/LSP post-edit work for file mutations when configured.
8. Appends `ToolResult` or `ToolError`.

If a provider round produces tool calls, the engine starts another round with
the updated projection. The turn continues until the provider finishes,
cancellation is observed, or execution returns an error.

Refresh builds and validates a complete candidate while the active snapshot
remains readable, then replaces the active `Arc` once. Only a changed,
successful candidate advances `ConfigGeneration`; failure and logical no-op
leave both generation and effective view unchanged. New turns bind the
published snapshot, while in-flight turns continue on their retained snapshot
without a dispatch-path registry lock. Direct shell turns use the same binding
and audit event.

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

Plugin reconciliation in `0.34.6` covers startup tool exports and their RPC
binding. Plugin hook/command/permission callback lifecycle remains owned by the
existing `PluginHost` and `PermissionPlane`; there is no dynamic hook plane,
plugin watcher, or plugin hot-reload API. A respawn must reproduce the complete
canonical initialize declaration or the new child is closed and calls fail
closed.

`RuntimeSnapshot` owns exactly one `BundleCatalog`. For installed bundles,
`hya-app` reads the bundle registry generation before binding each new root turn
and before TUI/catalog refresh, builds one complete built-ins-plus-installed
public-static candidate, and publishes it atomically. An unchanged generation
is a no-op; validation or load failure preserves the old snapshot. In-flight
turns and child turns retain their pinned snapshot. There is no bundle watcher,
per-provider-round or per-tool-call database check, or second catalog authority.

Core preserves that child pinning with a typed `BoundSpawnRequest` carrying the
parent `TurnBinding` through the application supervisor into both transient and
resident execution. The child path does not query the bundle registry database
for a generation or bind whichever runtime is current when a queued request
runs.

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
durable start marker is aborted and never automatically retried. Provider/tool
events, mailbox writes, spawn admissions, and child transitions use the same
claim-aware store commit seam and publish only after SQLite commit. A stale
claim returns `StaleActorClaim` without appending or waking successful work.
Full-tuple claim release atomically aborts any still-bound admission before the
claim becomes reusable; only the first logical release refunds a live governor.

The claim is TTL-free and local to one harness process incarnation. There is no
heartbeat, wall-clock expiry, lease daemon, distributed coordination, HA, or
active-active behavior. Canonical-state fencing cannot promise exactly-once
filesystem/network/API side effects, and this release does not certify the
future 100/256 workload envelope.

## Cancellation

`run_turn` receives a `CancellationToken`. If cancellation is observed before a
provider round starts, the engine emits `MessageFinished` with
`FinishReason::Cancelled`.

The shell tool also checks the token before spawning a command and kills the
spawned Unix process group on cancellation.

## Compaction and Summaries

Compaction lives in [`compaction.rs`](../../crates/hya-core/src/compaction.rs)
and [`engine/summary.rs`](../../crates/hya-core/src/engine/summary.rs).
`ModelSummarizer` asks the configured provider for a summary when token
thresholds are exceeded. `compact_context` records a hya-native system summary
and prunes older provider context for future requests. The CLI exposes this via
`/compact`; legacy Compat summarize routes persist the same native summary
shape.

## Hooks

[`hooks.rs`](../../crates/hya-core/src/hooks.rs) defines the runtime hook
boundary used by `hya-plugin`. Hookable surfaces include events, command/user
message admission, chat params/messages, text completion, permission asks, and
tool before/after hooks. The CLI installs a `PluginHost` when `plugins:` are
configured.

## Goal Mode

Goal mode lives in [`completion.rs`](../../crates/hya-core/src/completion.rs).
It uses three pieces:

- `IterationDriver`: generic loop runner with safety caps.
- `LeadTurnExecutor`: admits the next directive into the lead session and runs a
  turn.
- `GoalGate`: asks a `GoalEvaluator` whether the transcript satisfies the goal.

`ModelGoalEvaluator` calls a provider with no tools and requests strict JSON:

```json
{"met": true, "reason": "..."}
```

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
- [`team.rs`](../../crates/hya-core/src/team.rs)
- [`workspace.rs`](../../crates/hya-core/src/workspace.rs)
- [`category.rs`](../../crates/hya-core/src/category.rs)

`run_team` runs member specs in child sessions and returns bounded evidence
summaries. It intentionally does not project full child transcripts into the
lead session.

`TeamControlPlane` models lifecycle transitions, mailbox messages, and task
board state. `WorktreeManager` allocates git worktrees under `.hya/worktrees`
and only cleans up paths it recorded as owned.

These primitives are present in `hya-core`; the shipped CLI exposes the main
TUI, single-turn/run aliases, goal, server, replay, sessions, catalog/auth, and
JSONL RPC surfaces.
