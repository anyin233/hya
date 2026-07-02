# Project Structure

hya is organized as a Rust workspace of small crates. The central idea is that
every runtime surface shares one canonical event model:

```text
CLI / TUI / HTTP
      |
      v
hya-core::SessionEngine
      |
      +--> hya-provider routes model streams into hya-proto::Event
      +--> hya-tool executes builtin, MCP, and plugin tools behind PermissionPlane
      +--> hya-store appends and replays events
      +--> hya-proto folds events into projections
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
| `hya-proto` | [`../crates/hya-proto/src/lib.rs`](../crates/hya-proto/src/lib.rs) | Shared ids, messages, events, API DTOs, and projection reducer. |
| `hya-provider` | [`../crates/hya-provider/src/lib.rs`](../crates/hya-provider/src/lib.rs) | Provider trait, router, protocol encoders/decoders, HTTP SSE client, fake/dev providers. |
| `hya-tool` | [`../crates/hya-tool/src/lib.rs`](../crates/hya-tool/src/lib.rs) | Tool trait, builtin tools, permission rules, ask/decision channel. |
| `hya-mcp` | [`../crates/hya-mcp/src/lib.rs`](../crates/hya-mcp/src/lib.rs) | MCP stdio client/manager, resource discovery, and tool bridge. |
| `hya-plugin` | [`../crates/hya-plugin/src/lib.rs`](../crates/hya-plugin/src/lib.rs) | Stdio JSON-RPC plugin host, manifest/config merge, hook dispatch, tool and permission bridge. |
| `hya-plugin-compat` | [`../crates/hya-plugin-compat`](../crates/hya-plugin-compat) | Bundled Bun adapter for Compat plugin SDK compatibility. |
| `hya-plugin-example` | [`../crates/hya-plugin-example/src/main.rs`](../crates/hya-plugin-example/src/main.rs) | Minimal fixture/example plugin binary. |
| `hya-store` | [`../crates/hya-store/src/lib.rs`](../crates/hya-store/src/lib.rs) | SQLite event log, replay, projection reads, token ledger. |
| `hya-core` | [`../crates/hya-core/src/lib.rs`](../crates/hya-core/src/lib.rs) | Session engine, event bus, turn loop, compaction, hooks, goal/loop drivers, teams, worktrees. |
| `hya-server` | [`../crates/hya-server/src/lib.rs`](../crates/hya-server/src/lib.rs) | Native HTTP/SSE API and Compat-compatible routes over `SessionEngine`. |
| `hya-client` | [`../crates/hya-client/src/lib.rs`](../crates/hya-client/src/lib.rs) | Typed reqwest client for the server API. |
| `hya-legacy-tui` | [`../crates/hya-legacy-tui/src/lib.rs`](../crates/hya-legacy-tui/src/lib.rs) | Pure ratatui view state, layout, theme, view-model conversion, and widgets. |
| `hya-backend` | [`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs) | Umbrella binary: TUI, `run`/`exec`, goal mode, server, tail-session, config/auth, MCP/plugin setup. |

## `hya-proto`

`hya-proto` is deliberately dependency-light so other crates can share wire
types without pulling in async runtimes, SQL, or HTTP.

Important modules:

| Module | Purpose |
| --- | --- |
| [`api.rs`](../crates/hya-proto/src/api.rs) | HTTP request/response DTOs for create session, prompt, events query. |
| [`event.rs`](../crates/hya-proto/src/event.rs) | Canonical `Event` enum and ordered `Envelope`. |
| [`ids.rs`](../crates/hya-proto/src/ids.rs) | Strongly typed ids: new sessions use `hysec_...`; messages, parts, and tool calls keep UUID-backed display prefixes such as `msg_` and `tc_`. |
| [`message.rs`](../crates/hya-proto/src/message.rs) | `Message`, `Part`, role, finish reason, token and cost structs. |
| [`model.rs`](../crates/hya-proto/src/model.rs) | String newtypes for agents, models, tools, and model-facing tool schemas. |
| [`projection.rs`](../crates/hya-proto/src/projection.rs) | Idempotent reducer from envelopes to `Projection`. |

The reducer ignores duplicate or older envelopes by comparing `Envelope.seq` to
`Projection.last_seq`, which makes replay and SSE reconnect logic use the same
state transition rules.

## `hya-provider`

`hya-provider` normalizes upstream model protocols into `hya_proto::Event`.

Important modules:

| Module | Purpose |
| --- | --- |
| [`lib.rs`](../crates/hya-provider/src/lib.rs) | Provider, protocol, decoder traits, capabilities, request type, preflight. |
| [`router.rs`](../crates/hya-provider/src/router.rs) | Selects a provider by model id and runs capability preflight. |
| [`http.rs`](../crates/hya-provider/src/http.rs) | Shared HTTP/SSE driver with redirect-disabled reqwest client. |
| [`openai.rs`](../crates/hya-provider/src/openai.rs) | OpenAI Chat Completions compatible encoder/decoder. |
| [`anthropic.rs`](../crates/hya-provider/src/anthropic.rs) | Anthropic Messages encoder/decoder. |
| [`google.rs`](../crates/hya-provider/src/google.rs) | Gemini encoder/decoder, including canonical media part support. |
| [`dev.rs`](../crates/hya-provider/src/dev.rs) | Offline provider used when no live config is available. |
| [`fake.rs`](../crates/hya-provider/src/fake.rs) | Scripted provider for tests. |
| [`wire.rs`](../crates/hya-provider/src/wire.rs) | Shared helpers for encoding stored tool parts back to provider wire format. |

Providers do not execute tools. They stream text, reasoning, tool-call requests,
and finish reasons; the engine executes requested tools and appends results.

## `hya-tool`

`hya-tool` defines the model-facing tool surface and the permission plane.

Important modules:

| Module | Purpose |
| --- | --- |
| [`permission.rs`](../crates/hya-tool/src/permission.rs) | Action/resource rules, `Allow`/`Ask`/`Deny`, ask requests, persistent allow-always decisions. |
| [`tool.rs`](../crates/hya-tool/src/tool.rs) | Tool trait, registry, aliases, shared context, path/search helpers. |
| [`read.rs`](../crates/hya-tool/src/read.rs), [`write.rs`](../crates/hya-tool/src/write.rs), [`edit.rs`](../crates/hya-tool/src/edit.rs), [`apply_patch`](../crates/hya-tool/src/apply_patch) | File read/write/edit/patch tools. |
| [`shell.rs`](../crates/hya-tool/src/shell.rs) | Shell execution tool and `bash` alias. |
| [`webfetch`](../crates/hya-tool/src/webfetch), [`websearch.rs`](../crates/hya-tool/src/websearch.rs) | Web fetch/search tools. |
| [`lsp.rs`](../crates/hya-tool/src/lsp.rs), [`formatter.rs`](../crates/hya-tool/src/formatter.rs) | LSP and formatter planes. |
| [`skill.rs`](../crates/hya-tool/src/skill.rs), [`task.rs`](../crates/hya-tool/src/task.rs), [`todo.rs`](../crates/hya-tool/src/todo.rs), [`question.rs`](../crates/hya-tool/src/question.rs) | Skill, subtask, todo, and human-question tools. |

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

## `hya-store`

`hya-store` persists the canonical event log in SQLite.

Important files:

| File | Purpose |
| --- | --- |
| [`src/lib.rs`](../crates/hya-store/src/lib.rs) | Store connections, migrations, append/replay/projection/usage APIs. |
| [`src/error.rs`](../crates/hya-store/src/error.rs) | Store error wrapper. |
| [`migrations/0001_init.sql`](../crates/hya-store/migrations/0001_init.sql) | Initial schema. |

Current read path:

1. `append_event` inserts serialized `Event` JSON into `event_log`.
2. `replay` returns ordered `Envelope`s for one session.
3. `read_projection` folds replayed envelopes through `hya_proto::Projection`.

The migration also creates tables for sessions, messages, parts, teams, mail,
tasks, and goals. Those tables reserve schema for broader runtime features; the
current projection read path is still event-log based.

## `hya-core`

`hya-core` owns the runtime behavior.

Important modules:

| Module | Purpose |
| --- | --- |
| [`engine.rs`](../crates/hya-core/src/engine.rs) | `SessionEngine` composition and event emission. |
| [`engine/admission.rs`](../crates/hya-core/src/engine/admission.rs) | User, command, and system-message admission. |
| [`engine/stream_round.rs`](../crates/hya-core/src/engine/stream_round.rs), [`engine/turn.rs`](../crates/hya-core/src/engine/turn.rs) | Provider rounds, tool execution, turn completion. |
| [`engine/shell.rs`](../crates/hya-core/src/engine/shell.rs) | Direct shell turns. |
| [`engine/session_state.rs`](../crates/hya-core/src/engine/session_state.rs) | Agent/model/session metadata updates. |
| [`engine/summary.rs`](../crates/hya-core/src/engine/summary.rs), [`compaction.rs`](../crates/hya-core/src/compaction.rs) | Summarization and provider-context compaction. |
| [`hooks.rs`](../crates/hya-core/src/hooks.rs) | Runtime hook bridge used by plugins. |
| [`bus.rs`](../crates/hya-core/src/bus.rs) | Broadcast event bus for live subscribers. |
| [`completion.rs`](../crates/hya-core/src/completion.rs) | Generic iteration driver, goal mode, model-backed evaluator, transcript rendering. |
| [`loop_mode.rs`](../crates/hya-core/src/loop_mode.rs) | Planner/verifier loop mode with budget, no-progress, and repeated-directive gates. |
| [`subagent.rs`](../crates/hya-core/src/subagent.rs) | Supervised child-session member runs and bounded team evidence projection. |
| [`team.rs`](../crates/hya-core/src/team.rs) | Team lifecycle state machine, mailbox, and task board primitives. |
| [`category.rs`](../crates/hya-core/src/category.rs) | Category-to-model routing helpers and skill prompt injection. |
| [`workspace.rs`](../crates/hya-core/src/workspace.rs) | Git worktree allocation/cleanup and tmux pane helper. |
| [`error.rs`](../crates/hya-core/src/error.rs) | Runtime error wrapper. |

`SessionEngine` is the central write path. It appends every event through the
store and immediately publishes the same envelope on the `EventBus`.

## `hya-server` and `hya-client`

`hya-server` exposes the engine over HTTP. The native hya routes are:

| Route | Behavior |
| --- | --- |
| `POST /sessions` | Create a session. |
| `POST /sessions/:id/prompt` | Admit a user prompt and run one turn. |
| `GET /sessions/:id/events` | Replay envelopes, optionally after `since_seq`. |
| `GET /sessions/:id/stream` | Stream live envelopes as SSE. |

It also mounts Compat-compatible route groups for legacy/v2 sessions, event
SSE, files/search/symbols, providers/models, permission/question queues, MCP,
PTY, VCS, project/worktree, TUI control, sync, global/config, and metadata
catalogs. Those routes translate between hya's event log/projection and
Compat-shaped HTTP bodies; exact parity is tracked in
[`compat-parity.md`](compat-parity.md).

`hya-client` is a small typed wrapper around create session, prompt, and events.
The current interactive TUI runs in-process through `hya-backend`; it does not need
the HTTP client to render local conversations.

## `hya-legacy-tui` and `hya-backend`

`hya-legacy-tui` is pure rendering. It owns:

- `AppState`
- `PermissionPrompt`
- projection application
- scroll state
- app layout calculation
- dark theme tokens
- projection-to-timeline view-model conversion
- ratatui widgets

`hya-backend/src/tui.rs` owns terminal side effects:

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
| [`lib.rs`](../crates/hya-legacy-tui/src/lib.rs) | Public `AppState`, prompt/view structs, and top-level `draw`. |
| [`layout.rs`](../crates/hya-legacy-tui/src/layout.rs) | Responsive terminal regions: status, timeline, optional sidebar, prompt, footer. |
| [`theme.rs`](../crates/hya-legacy-tui/src/theme.rs) | Named color/style tokens for the dark terminal theme. |
| [`view_model.rs`](../crates/hya-legacy-tui/src/view_model.rs) | Converts projections into timeline items, including text, reasoning, and tools. |
| [`widgets.rs`](../crates/hya-legacy-tui/src/widgets.rs) | Renders status, timeline, sidebar, prompt, footer, permission panel, and cursor. |

This split keeps rendering testable in [`../crates/hya-legacy-tui/tests`](../crates/hya-legacy-tui/tests).

## Tests

Tests are crate-local and map closely to runtime boundaries:

| Path | Coverage |
| --- | --- |
| [`../crates/hya-core/tests`](../crates/hya-core/tests) | Turn loop, goal/loop gates, teams, subagents, categories, worktrees. |
| [`../crates/hya-provider/tests`](../crates/hya-provider/tests) | OpenAI/Anthropic conformance, provider preflight, canonical event shape. |
| [`../crates/hya-store/tests`](../crates/hya-store/tests) | Migration, projection, session scoping, persistence, token ledger. |
| [`../crates/hya-tool/tests`](../crates/hya-tool/tests) | Permission evaluation and builtin tools. |
| [`../crates/hya-server/tests`](../crates/hya-server/tests) | Native API and Compat-compatible route behavior. |
| [`../crates/hya-plugin/tests`](../crates/hya-plugin/tests) | Plugin host protocol, hooks, and tool bridge behavior. |
| [`../crates/hya-plugin-compat/adapter/test`](../crates/hya-plugin-compat/adapter/test) | Compat adapter discovery, hooks, SDK shims, tools, events, lifecycle. |
| [`../crates/hya-legacy-tui/tests`](../crates/hya-legacy-tui/tests) | Rendering snapshots, permission panel, scroll behavior, tool lines. |

## Dependency Direction

The intended dependency direction is:

```text
hya-proto
  ^  ^  ^  ^  ^  ^
  |  |  |  |  |  |
provider tool store server/client/tui plugin
        ^      ^       ^
        |      |       |
       mcp  hya-core  plugin-compat adapter
                ^
                |
             hya-backend
```

The binary crate composes everything. Lower crates should avoid depending on the
binary or on UI-specific behavior.
