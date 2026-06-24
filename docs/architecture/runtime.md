# Runtime

The runtime lives in [`../../crates/yaca-core`](../../crates/yaca-core). Its
central type is `SessionEngine` in
[`engine.rs`](../../crates/yaca-core/src/engine.rs).

## `SessionEngine`

`SessionEngine` owns:

- `SessionStore` for persistence.
- `ProviderRouter` for model streaming.
- `ToolRegistry` for model-requested tool execution.
- `PermissionPlane` for allow/ask/deny decisions.
- `InteractionPlane`, `SpawnerPlane`, `TodoPlane`, `SkillPlane`, `WebSearchPlane`,
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
OpenCode-compatible v2 prompt admission can attach file and agent metadata that
is replayed through the projection and provider request builder.

## Assistant Turn Loop

`run_turn` starts one assistant message and repeatedly:

1. Reads the current projection from the store.
2. Builds a provider request from projection messages and tool schemas.
3. Streams provider events.
4. Appends text, reasoning, and tool-input events.
5. Collects `ToolCallRequested` events.
6. Executes requested tools through the registry with permission checks and
   plugin/MCP bridges.
7. Runs formatter/LSP post-edit work for file mutations when configured.
8. Appends `ToolResult` or `ToolError`.

If a provider round produces tool calls, the engine starts another round with
the updated projection. `MAX_TOOL_ROUNDS` is currently `25`; hitting it emits a
text notice and finishes the message with `FinishReason::Error`.

## Cancellation

`run_turn` receives a `CancellationToken`. If cancellation is observed before a
provider round starts, the engine emits `MessageFinished` with
`FinishReason::Cancelled`.

The shell tool also checks the token before spawning a command and kills the
spawned Unix process group on cancellation.

## Compaction and Summaries

Compaction lives in [`compaction.rs`](../../crates/yaca-core/src/compaction.rs)
and [`engine/summary.rs`](../../crates/yaca-core/src/engine/summary.rs).
`ModelSummarizer` asks the configured provider for a summary when token
thresholds are exceeded. `compact_context` records a yaca-native system summary
and prunes older provider context for future requests. The CLI exposes this via
`/compact`; legacy OpenCode summarize routes persist the same native summary
shape.

## Hooks

[`hooks.rs`](../../crates/yaca-core/src/hooks.rs) defines the runtime hook
boundary used by `yaca-plugin`. Hookable surfaces include events, command/user
message admission, chat params/messages, text completion, permission asks, and
tool before/after hooks. The CLI installs a `PluginHost` when `plugins:` are
configured.

## Goal Mode

Goal mode lives in [`completion.rs`](../../crates/yaca-core/src/completion.rs).
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

Loop mode lives in [`loop_mode.rs`](../../crates/yaca-core/src/loop_mode.rs).
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

- [`subagent.rs`](../../crates/yaca-core/src/subagent.rs)
- [`team.rs`](../../crates/yaca-core/src/team.rs)
- [`workspace.rs`](../../crates/yaca-core/src/workspace.rs)
- [`category.rs`](../../crates/yaca-core/src/category.rs)

`run_team` runs member specs in child sessions and returns bounded evidence
summaries. It intentionally does not project full child transcripts into the
lead session.

`TeamControlPlane` models lifecycle transitions, mailbox messages, and task
board state. `WorktreeManager` allocates git worktrees under `.yaca/worktrees`
and only cleans up paths it recorded as owned.

These primitives are present in `yaca-core`; the shipped CLI exposes the main
TUI, single-turn/run aliases, goal, server, replay, sessions, catalog/auth, and
JSONL RPC surfaces.
