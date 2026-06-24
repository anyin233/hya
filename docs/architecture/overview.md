# Architecture Overview

yaca is built around one invariant: the runtime produces a canonical,
replayable stream of `Event`s, and every surface reads from or writes to that
stream.

```text
user input
   |
   v
yaca-cli / yaca-server
   |
   v
SessionEngine
   |
   +-- append Event to SessionStore
   +-- publish Envelope on EventBus
   +-- stream provider Events from ProviderRouter
   +-- execute requested builtin/MCP/plugin tools through ToolRegistry + PermissionPlane
   +-- dispatch hooks through the plugin host when configured
   v
Projection reducer
   |
   v
TUI / API clients / transcript renderers
```

## Layer Boundaries

| Layer | Crates | Responsibility |
| --- | --- | --- |
| Protocol | [`yaca-proto`](../../crates/yaca-proto) | Shared ids, events, messages, DTOs, and projection reducer. |
| Model I/O | [`yaca-provider`](../../crates/yaca-provider) | Normalize OpenAI-compatible, Anthropic, Google, fake, and dev providers into event streams. |
| Tools | [`yaca-tool`](../../crates/yaca-tool), [`yaca-mcp`](../../crates/yaca-mcp), [`yaca-plugin`](../../crates/yaca-plugin) | Define tool schemas, execute builtin/MCP/plugin tools, and enforce permissions. |
| Persistence | [`yaca-store`](../../crates/yaca-store) | Append and replay events from SQLite; fold projections on read. |
| Runtime | [`yaca-core`](../../crates/yaca-core) | Own sessions, turn execution, event publication, hooks, compaction, goal/loop/team primitives. |
| Surfaces | [`yaca-cli`](../../crates/yaca-cli), [`yaca-server`](../../crates/yaca-server), [`yaca-client`](../../crates/yaca-client), [`yaca-tui`](../../crates/yaca-tui), [`yaca-plugin-opencode`](../../crates/yaca-plugin-opencode) | Expose the runtime through CLI, TUI, native/OpenCode HTTP/SSE, typed client APIs, and the OpenCode plugin adapter. |

## Turn Flow

1. A caller creates a session with an agent name, model, and workdir.
2. A user prompt is admitted as a user message event sequence.
3. `SessionEngine::run_turn` starts an assistant message.
4. The engine reads the current projection, applies compaction if needed, and
   builds a `CompletionRequest`.
5. `ProviderRouter` resolves the request model to a provider and preflights
   capabilities.
6. The provider streams canonical events such as text deltas and tool-call
   requests.
7. The engine appends streamed events, collects requested tool calls, executes
   tools, and appends tool results.
8. If tools were called, the engine runs tool-after hooks and starts another
   provider round with the updated projection. A hard cap stops runaway tool
   loops.
9. When no more tool calls remain, the assistant message is finished.

## Why Event Sourcing

The event log is the source of truth. This gives yaca a few useful properties:

- Replay and live streaming use the same `Envelope` shape.
- TUI state and API state fold through the same projection reducer.
- `tail-session` can debug a session without special introspection hooks.
- Tool results and provider deltas are stored in the same ordered history.

## Current Runtime Surfaces

- The default `yaca` command runs the interactive TUI in-process.
- `yaca exec` runs one turn and prints a transcript.
- `yaca run` is an OpenCode-compatible alias for headless prompt execution.
- `yaca -p` runs goal mode with an independent model-backed evaluator.
- `yaca serve` exposes HTTP and SSE over the same engine.
- `yaca tail-session` replays JSON envelopes from a persisted SQLite event log.
- `yaca models`, `login`, `auth`/`providers`, `agent`, `sessions`, and `rpc`
  expose local catalogs, auth tokens, session listing, and JSONL integration
  modes.
