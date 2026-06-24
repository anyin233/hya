# Project Structure

yaca is organized as a Rust workspace of small crates. The central idea is that
every runtime surface shares one canonical event model:

```text
CLI / TUI / HTTP
      |
      v
yaca-core::SessionEngine
      |
      +--> yaca-provider routes model streams into yaca-proto::Event
      +--> yaca-tool executes builtin, MCP, and plugin tools behind PermissionPlane
      +--> yaca-store appends and replays events
      +--> yaca-proto folds events into projections
```

## Repository Map

| Path | Purpose |
| --- | --- |
| [`../Cargo.toml`](../Cargo.toml) | Workspace members, shared dependency versions, Rust edition/version, and workspace lints. |
| [`../Cargo.lock`](../Cargo.lock) | Locked dependency graph. |
| [`../clippy.toml`](../clippy.toml) | Workspace clippy configuration. |
| [`../rustfmt.toml`](../rustfmt.toml) | Workspace formatting configuration. |
| [`../README.md`](../README.md) | Short public overview and quick command examples. |
| [`../crates`](../crates) | Production crates. |
| [`../xtask`](../xtask) | Developer tooling crate. Currently a scaffold. |
| [`../docs`](../docs) | Project documentation. |

## Crate Responsibilities

| Crate | Main source | Responsibility |
| --- | --- | --- |
| `yaca-proto` | [`../crates/yaca-proto/src/lib.rs`](../crates/yaca-proto/src/lib.rs) | Shared ids, messages, events, API DTOs, and projection reducer. |
| `yaca-provider` | [`../crates/yaca-provider/src/lib.rs`](../crates/yaca-provider/src/lib.rs) | Provider trait, router, protocol encoders/decoders, HTTP SSE client, fake/dev providers. |
| `yaca-tool` | [`../crates/yaca-tool/src/lib.rs`](../crates/yaca-tool/src/lib.rs) | Tool trait, builtin tools, permission rules, ask/decision channel. |
| `yaca-mcp` | [`../crates/yaca-mcp/src/lib.rs`](../crates/yaca-mcp/src/lib.rs) | MCP stdio client/manager, resource discovery, and tool bridge. |
| `yaca-plugin` | [`../crates/yaca-plugin/src/lib.rs`](../crates/yaca-plugin/src/lib.rs) | Stdio JSON-RPC plugin host, manifest/config merge, hook dispatch, tool and permission bridge. |
| `yaca-plugin-opencode` | [`../crates/yaca-plugin-opencode`](../crates/yaca-plugin-opencode) | Bundled Bun adapter for OpenCode plugin SDK compatibility. |
| `yaca-plugin-example` | [`../crates/yaca-plugin-example/src/main.rs`](../crates/yaca-plugin-example/src/main.rs) | Minimal fixture/example plugin binary. |
| `yaca-store` | [`../crates/yaca-store/src/lib.rs`](../crates/yaca-store/src/lib.rs) | SQLite event log, replay, projection reads, token ledger. |
| `yaca-core` | [`../crates/yaca-core/src/lib.rs`](../crates/yaca-core/src/lib.rs) | Session engine, event bus, turn loop, compaction, hooks, goal/loop drivers, teams, worktrees. |
| `yaca-server` | [`../crates/yaca-server/src/lib.rs`](../crates/yaca-server/src/lib.rs) | Native HTTP/SSE API and OpenCode-compatible routes over `SessionEngine`. |
| `yaca-client` | [`../crates/yaca-client/src/lib.rs`](../crates/yaca-client/src/lib.rs) | Typed reqwest client for the server API. |
| `yaca-tui` | [`../crates/yaca-tui/src/lib.rs`](../crates/yaca-tui/src/lib.rs) | Pure ratatui view state, layout, theme, view-model conversion, and widgets. |
| `yaca-cli` | [`../crates/yaca-cli/src/main.rs`](../crates/yaca-cli/src/main.rs) | Umbrella binary: TUI, `run`/`exec`, goal mode, server, tail-session, config/auth, MCP/plugin setup. |

## `yaca-proto`

`yaca-proto` is deliberately dependency-light so other crates can share wire
types without pulling in async runtimes, SQL, or HTTP.

Important modules:

| Module | Purpose |
| --- | --- |
| [`api.rs`](../crates/yaca-proto/src/api.rs) | HTTP request/response DTOs for create session, prompt, events query. |
| [`event.rs`](../crates/yaca-proto/src/event.rs) | Canonical `Event` enum and ordered `Envelope`. |
| [`ids.rs`](../crates/yaca-proto/src/ids.rs) | Strongly typed UUIDv7 ids with display prefixes such as `ses_` and `msg_`. |
| [`message.rs`](../crates/yaca-proto/src/message.rs) | `Message`, `Part`, role, finish reason, token and cost structs. |
| [`model.rs`](../crates/yaca-proto/src/model.rs) | String newtypes for agents, models, tools, and model-facing tool schemas. |
| [`projection.rs`](../crates/yaca-proto/src/projection.rs) | Idempotent reducer from envelopes to `Projection`. |

The reducer ignores duplicate or older envelopes by comparing `Envelope.seq` to
`Projection.last_seq`, which makes replay and SSE reconnect logic use the same
state transition rules.

## `yaca-provider`

`yaca-provider` normalizes upstream model protocols into `yaca_proto::Event`.

Important modules:

| Module | Purpose |
| --- | --- |
| [`lib.rs`](../crates/yaca-provider/src/lib.rs) | Provider, protocol, decoder traits, capabilities, request type, preflight. |
| [`router.rs`](../crates/yaca-provider/src/router.rs) | Selects a provider by model id and runs capability preflight. |
| [`http.rs`](../crates/yaca-provider/src/http.rs) | Shared HTTP/SSE driver with redirect-disabled reqwest client. |
| [`openai.rs`](../crates/yaca-provider/src/openai.rs) | OpenAI Chat Completions compatible encoder/decoder. |
| [`anthropic.rs`](../crates/yaca-provider/src/anthropic.rs) | Anthropic Messages encoder/decoder. |
| [`google.rs`](../crates/yaca-provider/src/google.rs) | Gemini encoder/decoder, including canonical media part support. |
| [`dev.rs`](../crates/yaca-provider/src/dev.rs) | Offline provider used when no live config is available. |
| [`fake.rs`](../crates/yaca-provider/src/fake.rs) | Scripted provider for tests. |
| [`wire.rs`](../crates/yaca-provider/src/wire.rs) | Shared helpers for encoding stored tool parts back to provider wire format. |

Providers do not execute tools. They stream text, reasoning, tool-call requests,
and finish reasons; the engine executes requested tools and appends results.

## `yaca-tool`

`yaca-tool` defines the model-facing tool surface and the permission plane.

Important modules:

| Module | Purpose |
| --- | --- |
| [`permission.rs`](../crates/yaca-tool/src/permission.rs) | Action/resource rules, `Allow`/`Ask`/`Deny`, ask requests, persistent allow-always decisions. |
| [`tool.rs`](../crates/yaca-tool/src/tool.rs) | Tool trait, registry, aliases, shared context, path/search helpers. |
| [`read.rs`](../crates/yaca-tool/src/read.rs), [`write.rs`](../crates/yaca-tool/src/write.rs), [`edit.rs`](../crates/yaca-tool/src/edit.rs), [`apply_patch`](../crates/yaca-tool/src/apply_patch) | File read/write/edit/patch tools. |
| [`shell.rs`](../crates/yaca-tool/src/shell.rs) | Shell execution tool and `bash` alias. |
| [`webfetch`](../crates/yaca-tool/src/webfetch), [`websearch.rs`](../crates/yaca-tool/src/websearch.rs) | Web fetch/search tools. |
| [`lsp.rs`](../crates/yaca-tool/src/lsp.rs), [`formatter.rs`](../crates/yaca-tool/src/formatter.rs) | LSP and formatter planes. |
| [`skill.rs`](../crates/yaca-tool/src/skill.rs), [`task.rs`](../crates/yaca-tool/src/task.rs), [`todo.rs`](../crates/yaca-tool/src/todo.rs), [`question.rs`](../crates/yaca-tool/src/question.rs) | Skill, subtask, todo, and human-question tools. |

Builtins currently include:

| Tool | Permission action | Behavior |
| --- | --- | --- |
| `read` | `Read` | Read text/media files and directory listings with truncation. |
| `write` | `Edit` | Create parent directories, write content, run formatter/LSP post-edit hooks. |
| `edit` | `Edit` | Replace text with ambiguity checks, formatter/LSP post-edit hooks. |
| `apply_patch` (`patch`) | `Edit` | Apply unified-style patches and return aggregate/per-file diff metadata. |
| `ls` | `Read` | List immediate directory entries. |
| `glob`, `find` | `Glob` | Search path names under a directory. |
| `grep` | `Grep` | Regex-search file contents under a path. |
| `shell`, `bash` | `Bash` | Run a shell command in the agent workdir. |
| `webfetch` (`fetch`), `websearch` (`search`) | Web planes | Fetch URLs or query a configured web-search provider. |
| `question`, `ask_user` | Interaction plane | Ask the human a select or free-text question. |
| `lsp` | LSP plane | Dispatch workspace-symbol/diagnostic-style LSP operations. |
| `skill` | Skill plane | Load and expose local `SKILL.md` content. |
| `task` | Spawner plane | Start foreground/background subagent member work. |
| `todowrite` (`todo`) | Todo plane | Store the latest session todo snapshot. |
| `plan_exit` (`plan`) | Plan tool | Signal plan-mode completion semantics to the model. |
| `invalid` | None | Structured response for unknown tool calls. |

Tool output is capped at 16 KiB for large text fields. Search-style tools such
as `glob` and `grep` cap returned rows at 100 while preserving count and
truncation metadata.

## `yaca-store`

`yaca-store` persists the canonical event log in SQLite.

Important files:

| File | Purpose |
| --- | --- |
| [`src/lib.rs`](../crates/yaca-store/src/lib.rs) | Store connections, migrations, append/replay/projection/usage APIs. |
| [`src/error.rs`](../crates/yaca-store/src/error.rs) | Store error wrapper. |
| [`migrations/0001_init.sql`](../crates/yaca-store/migrations/0001_init.sql) | Initial schema. |

Current read path:

1. `append_event` inserts serialized `Event` JSON into `event_log`.
2. `replay` returns ordered `Envelope`s for one session.
3. `read_projection` folds replayed envelopes through `yaca_proto::Projection`.

The migration also creates tables for sessions, messages, parts, teams, mail,
tasks, and goals. Those tables reserve schema for broader runtime features; the
current projection read path is still event-log based.

## `yaca-core`

`yaca-core` owns the runtime behavior.

Important modules:

| Module | Purpose |
| --- | --- |
| [`engine.rs`](../crates/yaca-core/src/engine.rs) | `SessionEngine` composition and event emission. |
| [`engine/admission.rs`](../crates/yaca-core/src/engine/admission.rs) | User, command, and system-message admission. |
| [`engine/stream_round.rs`](../crates/yaca-core/src/engine/stream_round.rs), [`engine/turn.rs`](../crates/yaca-core/src/engine/turn.rs) | Provider rounds, tool execution, turn completion. |
| [`engine/shell.rs`](../crates/yaca-core/src/engine/shell.rs) | Direct shell turns. |
| [`engine/session_state.rs`](../crates/yaca-core/src/engine/session_state.rs) | Agent/model/session metadata updates. |
| [`engine/summary.rs`](../crates/yaca-core/src/engine/summary.rs), [`compaction.rs`](../crates/yaca-core/src/compaction.rs) | Summarization and provider-context compaction. |
| [`hooks.rs`](../crates/yaca-core/src/hooks.rs) | Runtime hook bridge used by plugins. |
| [`bus.rs`](../crates/yaca-core/src/bus.rs) | Broadcast event bus for live subscribers. |
| [`completion.rs`](../crates/yaca-core/src/completion.rs) | Generic iteration driver, goal mode, model-backed evaluator, transcript rendering. |
| [`loop_mode.rs`](../crates/yaca-core/src/loop_mode.rs) | Planner/verifier loop mode with budget, no-progress, and repeated-directive gates. |
| [`subagent.rs`](../crates/yaca-core/src/subagent.rs) | Supervised child-session member runs and bounded team evidence projection. |
| [`team.rs`](../crates/yaca-core/src/team.rs) | Team lifecycle state machine, mailbox, and task board primitives. |
| [`category.rs`](../crates/yaca-core/src/category.rs) | Category-to-model routing helpers and skill prompt injection. |
| [`workspace.rs`](../crates/yaca-core/src/workspace.rs) | Git worktree allocation/cleanup and tmux pane helper. |
| [`error.rs`](../crates/yaca-core/src/error.rs) | Runtime error wrapper. |

`SessionEngine` is the central write path. It appends every event through the
store and immediately publishes the same envelope on the `EventBus`.

## `yaca-server` and `yaca-client`

`yaca-server` exposes the engine over HTTP. The native yaca routes are:

| Route | Behavior |
| --- | --- |
| `POST /sessions` | Create a session. |
| `POST /sessions/:id/prompt` | Admit a user prompt and run one turn. |
| `GET /sessions/:id/events` | Replay envelopes, optionally after `since_seq`. |
| `GET /sessions/:id/stream` | Stream live envelopes as SSE. |

It also mounts OpenCode-compatible route groups for legacy/v2 sessions, event
SSE, files/search/symbols, providers/models, permission/question queues, MCP,
PTY, VCS, project/worktree, TUI control, sync, global/config, and metadata
catalogs. Those routes translate between yaca's event log/projection and
OpenCode-shaped HTTP bodies; exact parity is tracked in
[`opencode-parity.md`](opencode-parity.md).

`yaca-client` is a small typed wrapper around create session, prompt, and events.
The current interactive TUI runs in-process through `yaca-cli`; it does not need
the HTTP client to render local conversations.

## `yaca-tui` and `yaca-cli`

`yaca-tui` is pure rendering. It owns:

- `AppState`
- `PermissionPrompt`
- projection application
- scroll state
- app layout calculation
- dark theme tokens
- projection-to-timeline view-model conversion
- ratatui widgets

`yaca-cli/src/tui.rs` owns terminal side effects:

- raw mode and alternate-screen setup/teardown
- keyboard handling
- permission prompt key handling
- question prompt key handling
- slash command, custom command, and `@` reference routing
- spawning an async turn
- subscribing to the engine event bus

Important TUI modules:

| Module | Purpose |
| --- | --- |
| [`lib.rs`](../crates/yaca-tui/src/lib.rs) | Public `AppState`, prompt/view structs, and top-level `draw`. |
| [`layout.rs`](../crates/yaca-tui/src/layout.rs) | Responsive terminal regions: status, timeline, optional sidebar, prompt, footer. |
| [`theme.rs`](../crates/yaca-tui/src/theme.rs) | Named color/style tokens for the dark terminal theme. |
| [`view_model.rs`](../crates/yaca-tui/src/view_model.rs) | Converts projections into timeline items, including text, reasoning, and tools. |
| [`widgets.rs`](../crates/yaca-tui/src/widgets.rs) | Renders status, timeline, sidebar, prompt, footer, permission panel, and cursor. |

This split keeps rendering testable in [`../crates/yaca-tui/tests`](../crates/yaca-tui/tests).

## Tests

Tests are crate-local and map closely to runtime boundaries:

| Path | Coverage |
| --- | --- |
| [`../crates/yaca-core/tests`](../crates/yaca-core/tests) | Turn loop, goal/loop gates, teams, subagents, categories, worktrees. |
| [`../crates/yaca-provider/tests`](../crates/yaca-provider/tests) | OpenAI/Anthropic conformance, provider preflight, canonical event shape. |
| [`../crates/yaca-store/tests`](../crates/yaca-store/tests) | Migration, projection, session scoping, persistence, token ledger. |
| [`../crates/yaca-tool/tests`](../crates/yaca-tool/tests) | Permission evaluation and builtin tools. |
| [`../crates/yaca-server/tests`](../crates/yaca-server/tests) | Native API and OpenCode-compatible route behavior. |
| [`../crates/yaca-plugin/tests`](../crates/yaca-plugin/tests) | Plugin host protocol, hooks, and tool bridge behavior. |
| [`../crates/yaca-plugin-opencode/adapter/test`](../crates/yaca-plugin-opencode/adapter/test) | OpenCode adapter discovery, hooks, SDK shims, tools, events, lifecycle. |
| [`../crates/yaca-tui/tests`](../crates/yaca-tui/tests) | Rendering snapshots, permission panel, scroll behavior, tool lines. |

## Dependency Direction

The intended dependency direction is:

```text
yaca-proto
  ^  ^  ^  ^  ^  ^
  |  |  |  |  |  |
provider tool store server/client/tui plugin
        ^      ^       ^
        |      |       |
       mcp  yaca-core  plugin-opencode adapter
                ^
                |
             yaca-cli
```

The binary crate composes everything. Lower crates should avoid depending on the
binary or on UI-specific behavior.
