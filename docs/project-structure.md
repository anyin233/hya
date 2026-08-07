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
| [`../crates/xtask`](../crates/xtask) | Developer tooling: `sync-compat`, `migrate`, `startup-bench`, `matrix-check`. |
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
| `hya-plugin-example` | [`../crates/hya-plugin-example/src/main.rs`](../crates/hya-plugin-example/src/main.rs) | Placeholder stub (`fn main() {}`); does **not** speak the plugin protocol. Future native-plugin QA fixture. Real ABI: [plugin-protocol.md](plugin-protocol.md). |
| `hya-store` | [`../crates/hya-store/src/lib.rs`](../crates/hya-store/src/lib.rs) | SQLite event log, replay, projection reads, token ledger, admission journal, mailbox, resident claims, saved permissions, and installed-bundle registry. |
| `hya-core` | [`../crates/hya-core/src/lib.rs`](../crates/hya-core/src/lib.rs) | Session engine, event bus, turn loop, compaction, hooks, goal/loop drivers, resident teams, orchestrator budgets, worktrees. |
| `hya-server` | [`../crates/hya-server/src/lib.rs`](../crates/hya-server/src/lib.rs) | Native HTTP/SSE API and Compat-compatible routes over `SessionEngine`. |
| `hya-client` | [`../crates/hya-client/src/lib.rs`](../crates/hya-client/src/lib.rs) | Typed reqwest client for the server API. |
| `hya-sdk` | [`../crates/hya-sdk/src/lib.rs`](../crates/hya-sdk/src/lib.rs) | Integration SDK: `Client` + HTTP/native transport, `DIRECTORY_HEADER`, `ServerHandle`, live `MessageStore`, team projection, V2Event reducer. |
| `hya-native` | [`../crates/hya-native/src/transport.rs`](../crates/hya-native/src/transport.rs) | In-process axum `Router` transport via tower `oneshot` (no TCP) and `spawn_event_bridge` for `/global/event`. |
| `hya-updater` | [`../crates/hya-updater`](../crates/hya-updater) | Independent self-update TCB; see [self-update.md](self-update.md). |
| `hya` | [`../crates/hya/src/main.rs`](../crates/hya/src/main.rs) | Canonical Unix entrypoint. Replaces itself with the adjacent `hya-ts` launcher. |
| `hya-ts` | [`../crates/hya-ts/src/main.rs`](../crates/hya-ts/src/main.rs) | TypeScript TUI supervisor: CLI parsing, backend/runtime discovery, process-group handoff, and cleanup. |
| `hya-backend` | [`../crates/hya-backend/src/main.rs`](../crates/hya-backend/src/main.rs) | Backend umbrella binary: `run`/`exec`, goal mode, server, tail-session, config/auth, MCP/plugin setup, session listing, JSONL RPC, and interactive startup that launches the current `hya` frontend. |
| `hya-app` | [`../crates/hya-app/src/lib.rs`](../crates/hya-app/src/lib.rs) | Runtime composition: config load, provider/MCP/plugin wiring, session engine build, installed-bundle refresh. |
| `hya-bundle` | [`../crates/hya-bundle/src/lib.rs`](../crates/hya-bundle/src/lib.rs) | AgentBundle prepare/validate/catalog types and package fixtures used by install CLI and process E2E. Catalog builders: `from_prepared` / `from_verified_catalogs` / `with_verified_catalogs` index agents by stable id and `bundle:<id>/agent/<local_id>`, resources by `(ExportKind, stable id)` plus bundle-local names/aliases; reads include `resolve_agent`, `resolve_resource`, `bundle_resources`, `resolve_spawn`, `spawnable_agents`. |
| `hya-e2e` | [`../crates/hya-e2e`](../crates/hya-e2e) | Process-level agent E2E: real `hya-backend` + FakeLlm (Track P). See [docs/testing](testing/README.md). |

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
| [`projection.rs`](../crates/hya-proto/src/projection.rs) | Idempotent reducer from envelopes to `Projection`, plus the roster-aware address/channel resolution the store gate and engine reader share. |
| [`scope.rs`](../crates/hya-proto/src/scope.rs) | Canonical agent paths, the parent/sibling/report scope rule, and unit-qualified channel keys ([ADR-0011](adr/0011-hierarchy-scoped-mailbox.md)). |

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
| [`shell.rs`](../crates/hya-tool/src/shell.rs) | Shell execution tool; registry also advertises a second canonical name `bash` (same implementation, not a hidden alias). |
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
| `shell`, `bash` | `Bash` | Two **advertised** canonical tool names (`insert_named_builtin` for `bash`) sharing one shell implementation. Not among the five **hidden** aliases (`patch`, `fetch`, `search`, `todo`, `plan`). |
| `webfetch` (`fetch`), `websearch` (`search`) | `WebFetch` / `WebSearch` | Fetch URLs or query a configured web-search provider. |
| `question`, `ask_user` | `Tool` | Ask the human a select or free-text question (interaction plane). |
| `lsp` | `Lsp` | Dispatch workspace-symbol/diagnostic-style LSP operations. |
| `skill` | `Skill` | Load and expose local `SKILL.md` content. |
| `list_agents` | `ReadOnly` | List spawnable agents for the model; allows without prompting under `default`. |
| `task` | `Task` | Start foreground/background subagent member work (spawner plane). |
| `todowrite` (`todo`) | `TodoWrite` | Store the latest session todo snapshot. |
| `plan_exit` (`plan`) | `Tool` | Signal plan-mode completion semantics to the model. |
| `roster`, `channels` | `ReadOnly` | Team roster and channel list; allow without prompting under `default`. |
| `send`, `announce`, `join`, `leave` | `Tool` | Unit-scoped mailbox send, one-way announce to direct reports, and channel join/leave; ask under `default`. |
| `invalid` | `Tool` | Structured response for unknown tool calls. |

Successful tool output is capped at **5000 characters** for model consumption
([`output_cap.rs`](../crates/hya-tool/src/output_cap.rs)); oversized results keep
a trailing window plus a truncation notice. Shell has its own larger buffer
limits. Search-style tools such as `glob` and `grep` also cap returned rows
(for example 100) while preserving count and truncation metadata.

## `hya-store`

`hya-store` persists the canonical event log in SQLite and related journals.

Important modules:

| File | Purpose |
| --- | --- |
| [`src/lib.rs`](../crates/hya-store/src/lib.rs) | Connections, append/replay/`read_projection`, list/delete sessions, token ledger, `decode_session_key`. |
| [`src/admission.rs`](../crates/hya-store/src/admission.rs) | Durable spawn-admission journal (queue, bindings, fairness). |
| [`src/mailbox.rs`](../crates/hya-store/src/mailbox.rs) | Event-sourced mail writes, resident recovery, stop/failure finalization. |
| [`src/resident_claim.rs`](../crates/hya-store/src/resident_claim.rs) | Actor claim fencing primitives. |
| [`src/sync.rs`](../crates/hya-store/src/sync.rs) | Compat sync history/replay helpers. |
| [`src/permission.rs`](../crates/hya-store/src/permission.rs) | Saved permissions. |
| [`src/bundle_registry.rs`](../crates/hya-store/src/bundle_registry.rs) | **Separate** installed-bundle registry SQLite DB (not the session event log). |
| [`src/error.rs`](../crates/hya-store/src/error.rs) | Store error wrapper. |

Session-store migrations (`migrations/`):

| Migration | Role |
| --- | --- |
| `0001_init.sql` | Core schema: event log plus reserved tables (sessions, messages, parts, teams, mail, tasks, goals). Projection remains event-log based. |
| `0002_sync_event.sql` | Compat sync event history. |
| `0003_saved_permission.sql` | Saved permission rows. |
| `0004_admission_journal.sql` | Spawn admission journal. |
| `0005_resident_actor_claim.sql` | Resident actor claims. |
| `0006_admission_queue_states.sql` | Admission queue states. |
| `0007_admission_bindings.sql` | Admission bindings. |
| `0008_admission_fairness.sql` | Admission fairness bookkeeping. |

Bundle registry uses a **separate** migration set under
`bundle_migrations/0001_init.sql` for the installed-bundle database file.

`BundleRegistryRecord` columns (installed bundles): `bundle_id`, `version`,
`publisher`, 32-byte `source_digest`, `prepared_digest`, `prepared_bytes`,
`installed_at`, tracked under a monotonically increasing registry `generation`
that drives catalog reload (see [cli.md](cli.md) bundle section).

Current event-log path:

1. `append_event` inserts serialized `Event` JSON into `event_log`.
2. `replay` returns ordered `Envelope`s for one session.
3. `read_projection` folds replayed envelopes through `hya_proto::Projection`.

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
| [`mailbox.rs`](../crates/hya-core/src/mailbox.rs) | Mailbox service loop draining `MailboxRequest`. |
| [`engine/mailbox.rs`](../crates/hya-core/src/engine/mailbox.rs) | Team-root mail delivery, announce, scope-filtered roster/channel queries, `MAIN_HANDLE`. |
| [`resident.rs`](../crates/hya-core/src/resident.rs) | `ResidentSupervisor`, team state, per-team lock and quiescence. |
| [`orchestrator.rs`](../crates/hya-core/src/orchestrator.rs) | `SubagentLimits`, `SubagentGovernor`, stream permits, per-team budgets. |
| [`runtime_registry.rs`](../crates/hya-core/src/runtime_registry.rs) | `RuntimeRegistry`, `TurnBinding`, config-generation publication. |
| [`sidecar.rs`](../crates/hya-core/src/sidecar.rs) | `SidecarLifecycle` contract for Bundle sidecars. |
| [`prompt.rs`](../crates/hya-core/src/prompt.rs) | Prompt construction helpers. |
| [`title.rs`](../crates/hya-core/src/title.rs) | Session title generation. |
| [`category.rs`](../crates/hya-core/src/category.rs) | Category-to-model routing helpers and skill prompt injection. |
| [`workspace.rs`](../crates/hya-core/src/workspace.rs) | Git worktree allocation/cleanup and tmux pane helper. |
| [`error.rs`](../crates/hya-core/src/error.rs) | Runtime error wrapper. |

(`team.rs` / `TeamControlPlane` were removed; see ADR-0001. Mailbox and resident
modules above are the live replacements.)

`SessionEngine` is the central durable write path: it appends events through the
store for committed work. Live-only envelopes use `publish_live` (seq `0`) and
**do not** go through the durable log. `publish_envelope` dispatches global and
activation/sidecar hooks **before** publishing on the `EventBus`, so the bus is
not the only consumer of an envelope.

The projection reducer applies `seq == 0` as live-only (does not advance
`last_seq`); durable envelopes with `seq <= last_seq` are ignored; otherwise the
event folds and `last_seq` advances.

## `hya-server` and `hya-client`

`hya-server` exposes the engine over HTTP. The native hya routes are:

| Route | Behavior |
| --- | --- |
| `POST /sessions` | Create a session. |
| `POST /sessions/:id/prompt` | Admit a user prompt and run one turn. |
| `POST /sessions/:id/command` | Run a command/template turn. |
| `POST /sessions/:id/shell` | Run a shell tool turn. |
| `GET /sessions/:id/events` | Replay envelopes, optionally after `since_seq`. |
| `GET /sessions/:id/stream` | Stream live envelopes as SSE. |

It also mounts Compat-compatible route groups for legacy/v2 sessions, event
SSE, files/search/symbols, providers/models, permission/question queues, MCP,
PTY, VCS, project/worktree, TUI control, sync, global/config, and metadata
catalogs. Those routes translate between hya's event log/projection and
Compat-shaped HTTP bodies; exact parity is tracked in
[`compat-parity.md`](compat-parity.md).

`hya-client` is a small typed wrapper around create session, prompt, and events.

## `hya-sdk`, `hya-native`, and `hya-updater`

### `hya-sdk`

Integration SDK for TUI and embedders talking to **`hya-server`** (or an
in-process bridge) over the Compat-compatible HTTP/SSE surface.

| Module | Purpose |
| --- | --- |
| [`client.rs`](../crates/hya-sdk/src/client.rs) | Typed `Client` trait and HTTP transport. |
| [`native.rs`](../crates/hya-sdk/src/native.rs) | In-process stdio/native bridge client surface. |
| [`server.rs`](../crates/hya-sdk/src/server.rs) | `ServerHandle` — spawn/supervise `hya-backend serve` and parse the listen URL. |
| [`events.rs`](../crates/hya-sdk/src/events.rs) | Global SSE helpers. |
| [`store.rs`](../crates/hya-sdk/src/store.rs) | Live `MessageStore` projection for UI. |
| [`team.rs`](../crates/hya-sdk/src/team.rs) | Frontend `TeamProjection` mirror. |
| [`reducer.rs`](../crates/hya-sdk/src/reducer.rs) | `session.next.*` V2Event timeline reducer. |
| [`types.rs`](../crates/hya-sdk/src/types.rs) | Shared SDK wire types. |
| [`pending.rs`](../crates/hya-sdk/src/pending.rs) | Pending ask/permission coordination slots. |
| [`error.rs`](../crates/hya-sdk/src/error.rs) | SDK errors and `Result` alias. |

Wire constant: `DIRECTORY_HEADER` = `x-opencode-directory` (working-directory
scope on every request).

### `hya-native`

In-process embedding: [`HyaNativeTransport`](../crates/hya-native/src/transport.rs)
drives the hya axum `Router` with tower `oneshot` — **no TCP, no reqwest** —
injecting the directory header on every request. This is the Rust analogue of
the Compat adapter’s in-process `app.fetch` and the supported way to embed hya
inside another Rust process.

Callers use the exported type alias
[`HyaNativeClient`](../crates/hya-native/src/transport.rs)
(`pub type HyaNativeClient = ApiClient<HyaNativeTransport>`), re-exported from
`hya_native`. That is the same typed `Client` surface as the HTTP SDK client,
backed by the in-process transport instead of reqwest.

[`spawn_event_bridge`](../crates/hya-native/src/events.rs) subscribes to
in-process `GET /global/event` SSE, decodes frames into
`hya_sdk::GlobalEvent`, and forwards them on an `mpsc` channel. Undecodable
frames are **skipped** (not fatal); on stream loss it re-subscribes after a
**50 ms** backoff; the task stops when the receiver is dropped.

### `hya-updater`

Independent self-update TCB (signed metadata, staged generations, smoke,
owner-gated activation). Not part of `hya-backend`. See
[self-update.md](self-update.md).

## Interactive Frontend

The shipped frontend spans four colocated components:

| Component | Purpose |
| --- | --- |
| [`crates/hya/src/main.rs`](../crates/hya/src/main.rs) | Replace the canonical `hya` process with adjacent `hya-ts`. |
| [`crates/hya-ts`](../crates/hya-ts) | Parse launcher/auth/import arguments, start or attach to the backend, and supervise Bun. |
| [`packages/hya-tui-ts`](../packages/hya-tui-ts) | SolidJS/OpenTUI rendering, interaction, routes, and HTTP/SSE synchronization through `@opencode-ai/sdk/v2`. |
| [`crates/hya-backend`](../crates/hya-backend) | Runtime composition, local server ownership, headless commands, and bare interactive startup through `hya`. |

`packages/hya-tui-ts` is the sole interactive frontend implementation. New
interactive behavior belongs there; shared backend behavior belongs below the
SDK boundary.

## Tests

Tests are crate-local and map closely to runtime boundaries. Product-path
process E2E is layered on top (Track P/T); see [Testing](testing/README.md).

| Path | Coverage |
| --- | --- |
| [`../crates/hya-core/tests`](../crates/hya-core/tests) | Turn loop, goal/loop gates, teams, subagents, categories, worktrees. |
| [`../crates/hya-app/tests`](../crates/hya-app/tests) | Nested spawn tree, admission, installed-bundle refresh, runtime composition. |
| [`../crates/hya-provider/tests`](../crates/hya-provider/tests) | OpenAI/Anthropic conformance, provider preflight, canonical event shape. |
| [`../crates/hya-store/tests`](../crates/hya-store/tests) | Migration, projection, session scoping, persistence, token ledger. |
| [`../crates/hya-tool/tests`](../crates/hya-tool/tests) | Permission evaluation and builtin tools. |
| [`../crates/hya-server/tests`](../crates/hya-server/tests) | Native API and Compat-compatible route behavior. |
| [`../crates/hya-plugin/tests`](../crates/hya-plugin/tests) | Plugin host protocol, hooks, and tool bridge behavior. |
| [`../crates/hya-plugin-compat/adapter/test`](../crates/hya-plugin-compat/adapter/test) | Compat adapter discovery, hooks, SDK shims, tools, events, lifecycle. |
| [`../crates/hya-backend/tests`](../crates/hya-backend/tests) | Bundle CLI, backend command integration. |
| [`../crates/hya/tests`](../crates/hya/tests), [`../crates/hya-ts/tests`](../crates/hya-ts/tests) | Canonical launcher delegation, process supervision, argument forwarding, and native transport integration. |
| [`../packages/hya-tui-ts/test`](../packages/hya-tui-ts/test) | TypeScript frontend state, SDK integration, real-backend permission/roster, multi-agent presentation, PTY smoke. |
| [`../crates/hya-e2e`](../crates/hya-e2e) | Track P process agent suite (FakeLlm + real backend). Matrix: [`../crates/hya-e2e/matrix.toml`](../crates/hya-e2e/matrix.toml). |
| [`testing/`](testing/) | Human docs for tracks, oracles, and optional CI snippet. |

## Dependency Direction

The intended dependency direction is:

```text
hya-proto
  ^  ^  ^  ^  ^
  |  |  |  |  |
provider tool store server/sdk
        ^      ^
        |      |
       mcp  hya-core
              ^
              |
            hya-app -- hya-native

hya -> hya-ts -> packages/hya-tui-ts -> hya-backend HTTP/SSE
hya-backend -> hya-app/hya-server and may launch hya for interactive startup
```

The binary crate composes everything. Lower crates should avoid depending on the
binary or on UI-specific behavior.
