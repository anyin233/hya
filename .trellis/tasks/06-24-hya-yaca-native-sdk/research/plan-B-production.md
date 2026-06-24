# Hya ↔ Yaca Native SDK Implementation Plan — Planner B Production/Lifecycle

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. This plan is written for the `feat/hya-yaca-native-sdk` worktree only; do not touch the separate `/chivier-disk/yanweiye/Projects/yaca` checkout.

**Goal:** Make `hya` complete a full session turn against `yaca` through a native in-process Rust SDK path with no TCP listener, no `reqwest` request, and no localhost port.

**Architecture:** Extract yaca's current binary-only bootstrap into a reusable `yaca-app` library, add a thin `hya-yaca` adapter crate that owns the in-process yaca runtime and implements `hya_sdk::Transport` by calling `yaca_server::router(AppState)` with `tower::ServiceExt::oneshot`, then make `hya` use that native guard by default. Event streaming deliberately reuses the existing `/global/event` SSE projection in-process instead of reimplementing the 787-line projector, but it adds explicit bounded backpressure, cancellation, lag/resync handling, and teardown around the stream.

**Tech stack:** Rust workspace edition 2024/rust 1.91 for yaca crates, existing hya crates edition 2021 unless separately migrated, `axum 0.7`, `tower 0.5`, `http-body-util`, `tokio`, `tokio-util::CancellationToken`, `thiserror`, existing `hya-sdk::ApiClient<T: Transport>`, existing `yaca-server::router`.

## Source grounding

- `hya-sdk` already has the exact request seam: `Transport` is `Send + Sync` and exposes `base_url`, `directory`, and `request(method, path, body)` (`crates/hya-sdk/src/client.rs:14-19`). `ApiClient::with_transport` is public and the whole TUI API is implemented once over `Transport` (`client.rs:154-175`, `247-590`).
- The current HTTP transport is where native must not go: `HttpTransport` owns `reqwest::Client`, formats URLs, injects `x-opencode-directory`, sends, `error_for_status`, and decodes JSON (`client.rs:177-220`).
- Existing events are still HTTP/SSE-only: `stream_global_events` does a `reqwest.get({base_url}/global/event)`, reads `bytes_stream().eventsource()`, tolerates unknown frames, and deserializes `GlobalEvent` (`crates/hya-sdk/src/events.rs:19-49`).
- `hya` currently gives the TUI a `PendingClient`, spawns backend connection in the background, then aborts the connector and drops the transport guard on exit (`crates/hya/src/main.rs:35-58`). The default yaca path currently goes through `ServerMode::new` -> `ServerHandle::spawn_yaca` -> HTTP/SSE (`main.rs:150-172`, `337-354`, `412-420`).
- `NativeBridge` today is a Bun stdio bridge for opencode only. Its `Drop` kills the child process group (`crates/hya-sdk/src/native.rs:66-72`, `159-182`) and must remain an explicit `--opencode` path, not the yaca-native design.
- `yaca-server::router(AppState)` is already the in-process HTTP-compatible entry point and merges `opencode::router()` (`crates/yaca-server/src/lib.rs:31-43`). `opencode::router()` includes the hya-facing routes: `/global/event`, `/session`, `/session/:id/message`, `/agent`, `/mcp`, `/permission`, `/question`, etc. (`crates/yaca-server/src/opencode.rs:95-126`).
- `AppState` contains the long-lived engine, agent, pending permission/question managers, `McpManager`, workspace adapters, formatter status, default agent, and global-agent flag (`crates/yaca-server/src/state.rs:11-22`). `ServerState::new` derives per-router state, including `RunRegistry`, `GlobalState`, `McpHttpState`, `PtyState`, and `TuiState` (`state.rs:88-126`).
- The event bus is `tokio::sync::broadcast` with default capacity 1024 (`crates/yaca-core/src/bus.rs:4-30`). `/global/event` already maps raw envelopes into opencode-shaped `GlobalEvent` SSE frames and converts broadcast lag into an SSE `resync` event (`crates/yaca-server/src/opencode/event.rs:102-128`).
- yaca bootstrap is currently trapped in `yaca-cli`: `agent_with_model` builds the prompt and skill context (`crates/yaca-cli/src/main.rs:95-117`), `build_session_engine` connects MCP, connects plugins, registers tools, creates permission/interaction/spawner planes, builds `SessionEngine`, and spawns the team supervisor (`main.rs:240-295`), while `resolve_runtime` loads config or falls back to offline (`main.rs:322-382`). `serve::cmd_serve` and `cmd_tui_hya` assemble `AppState` and then bind TCP listeners (`crates/yaca-cli/src/serve.rs:10-47`, `50-109`).
- MCP and plugin child lifecycle is Drop-driven today: `McpManager` owns `McpServer { _client, _guard, ... }` (`crates/yaca-mcp/src/manager.rs:35-47`) where `ChildGuard::drop` terminates the child (`crates/yaca-mcp/src/client.rs:52-89`); `PluginHost` owns plugin connections (`crates/yaca-plugin/src/host.rs:40-63`) whose child guard first sends plugin shutdown then terminates (`crates/yaca-plugin/src/client.rs:38-77`).
- Run cancellation already exists at server level: `RunRegistry::start` stores a `CancellationToken`, `cancel` cancels it, and `RunGuard::drop` clears busy state (`crates/yaca-server/src/runs.rs:38-113`); `/session/:id/abort` calls `st.runs.cancel(session)` (`crates/yaca-server/src/opencode/session_legacy.rs:361-368`).

## Key decisions

1. **Create `yaca-app`, do not put bootstrap in `yaca-server`.** `yaca-server` should remain router/projection state over `AppState`; moving config/provider/MCP/plugin/prompt bootstrap into it would make the server crate depend on provider/plugin/auth/config policy and risk a fat bidirectional application layer. `yaca-app` depends downward on `yaca-core`, `yaca-server`, `yaca-provider`, `yaca-store`, `yaca-tool`, `yaca-mcp`, `yaca-plugin`, and config helpers. `yaca-cli` and `hya-yaca` both depend on `yaca-app`; `yaca-app` depends on neither CLI nor hya.
2. **Create `hya-yaca`, do not feature-gate yaca into `hya-sdk`.** `hya-sdk` should stay the lean client/types/reducer crate. Pulling yaca's backend graph into a `native-yaca` feature would make the normal SDK build transitively include MCP/plugin/provider/sqlx and increase cycle risk. `hya-yaca` is the glue crate: `hya-sdk` + `yaca-app` + `yaca-server` + `tower`/`http-body-util`.
3. **Native is the new `hya` default; HTTP is opt-in.** Preserve `--server <url>` for attaching to external opencode-compatible servers and preserve `--opencode` / `--opencode --http`. Add `--http` as “use yaca over spawned `yaca serve`” for compatibility/debugging. Remove `--yaca-bin` from the default path semantics; keep it meaningful only for `--http` yaca.
4. **Events use in-process oneshot `/global/event`, not direct `engine.bus().subscribe()` projection.** Direct bus subscription is tempting but would require re-exporting or duplicating the private 787-line `opencode/event.rs` projection (`event.rs:171-787`) and would immediately fork behavior from HTTP clients. The production path should reuse the exact projected wire shape hya already accepts (`hya-sdk/src/types.rs:22-58`) by calling the SSE route in-process, then harden stream consumption around backpressure/lag/resync.

## Crate and module layout

### New crate: `crates/yaca-app`

Purpose: reusable yaca application bootstrap and runtime guard. No network binding.

Files:

- `crates/yaca-app/Cargo.toml`
  - Depends on workspace `yaca-core`, `yaca-provider`, `yaca-tool`, `yaca-store`, `yaca-server`, `yaca-proto`, `yaca-mcp`, `yaca-plugin`, plus `tokio`, `tokio-util`, `thiserror`, `serde`, `serde_json`, `serde_norway`, `time`, `anyhow` only if kept internal and converted at boundary.
- `crates/yaca-app/src/lib.rs`
  - `pub mod config; pub mod formatter_config; pub mod plugins; pub mod permission; pub mod skills; mod runtime; mod error;`
  - Exports `YacaApp`, `YacaAppOptions`, `YacaRuntimeConfig`, `YacaAppError`, `PermissionMode`.
- `crates/yaca-app/src/runtime.rs`
  - Moves/adapts from `yaca-cli/src/main.rs`: `today`, `discover_context_files`, `skill_dirs`, `agent_with_model`, `compaction_config`, `spawn_team_supervisor`, `build_session_engine`, `host_info`, `offline_router`, `resolve_runtime`, `open_store`.
  - Adds runtime ownership and shutdown.
- `crates/yaca-app/src/config.rs`
  - Move `yaca-cli/src/config.rs` nearly verbatim, but replace `crate::auth::load_token` with an internal `auth` module or explicit `AuthTokenSource` trait.
- `crates/yaca-app/src/auth.rs`
  - Move only token loading/saving helpers needed by config/control. CLI commands can keep command parsing in `yaca-cli`.
- `crates/yaca-app/src/plugins.rs`, `formatter_config.rs`, `skills.rs`, `permission.rs`
  - Move from `yaca-cli` with exported functions where `yaca-cli` still needs them.
- `crates/yaca-app/src/error.rs`
  - Typed errors; no `Box<dyn Error>` at the SDK boundary.

Core interfaces:

```rust
pub enum PermissionMode {
    FrontendRequests,
    AutoApproveScoped { workdir: std::path::PathBuf },
    Yolo,
}

pub struct YacaAppOptions {
    pub db: String,
    pub model_override: Option<String>,
    pub workdir: std::path::PathBuf,
    pub permission_mode: PermissionMode,
    pub include_global_agents: bool,
    pub readiness_timeout: std::time::Duration,
}

pub struct YacaApp {
    pub state: yaca_server::AppState,
    pub engine: std::sync::Arc<yaca_core::SessionEngine>,
    pub agent: std::sync::Arc<yaca_core::AgentSpec>,
    pub runtime: YacaRuntimeConfig,
    shutdown: tokio_util::sync::CancellationToken,
    background: Vec<tokio::task::JoinHandle<()>>,
    plugin_host: std::sync::Arc<yaca_plugin::PluginHost>,
}

impl YacaApp {
    pub async fn build(options: YacaAppOptions) -> Result<Self, YacaAppError>;
    pub fn router(&self) -> axum::Router;
    pub async fn shutdown(mut self) -> Result<(), YacaAppError>;
}

impl Drop for YacaApp { /* best-effort cancellation + abort background tasks */ }
```

Important: the `McpManager` is moved into `AppState::with_mcp_manager(mcp_manager)`, where `AppState` wraps it in `Arc<McpManager>` (`state.rs:69-73`); therefore `YacaApp` owns MCP lifetime by owning `AppState`/router clones, not by storing a duplicate manager. `plugin_host` must be retained by `YacaApp` because hooks are installed as `Arc<dyn HookDispatcher>` in `SessionEngine` (`yaca-core/src/engine.rs:99-103`) and `AppState` only receives workspace adapters (`yaca-cli/src/serve.rs:26-31`).

### New crate: `crates/hya-yaca`

Purpose: hya-facing native yaca client + event bridge + lifecycle guard. No CLI parsing.

Files:

- `crates/hya-yaca/Cargo.toml`
  - Depends on `hya-sdk`, `yaca-app`, `yaca-server`, `tokio`, `tokio-util`, `axum`, `tower = { features = ["util"] }`, `http-body-util`, `bytes`, `serde_json`, `thiserror`, `eventsource-stream`, `futures-util`.
- `crates/hya-yaca/src/lib.rs`
  - Exports `connect`, `YacaNative`, `YacaNativeOptions`, `YacaNativeTransport`, `YacaNativeError`.
- `crates/hya-yaca/src/transport.rs`
  - `Transport` implementation using `Router::oneshot`.
- `crates/hya-yaca/src/events.rs`
  - In-process `/global/event` stream bridge, bounded channel, cancellation, lag/resync handling.
- `crates/hya-yaca/src/error.rs`
  - Error mapping to `SdkError` where appropriate and native typed errors where lifecycle-specific.

Core interfaces:

```rust
pub struct YacaNativeOptions {
    pub db: String,
    pub model_override: Option<String>,
    pub workdir: std::path::PathBuf,
    pub readiness_timeout: std::time::Duration,
    pub event_buffer: usize,
    pub permission_mode: yaca_app::PermissionMode,
}

pub struct YacaNative {
    app: Option<yaca_app::YacaApp>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    cancel: tokio_util::sync::CancellationToken,
}

pub async fn connect(
    directory: impl Into<String>,
    options: YacaNativeOptions,
    events: tokio::sync::mpsc::Sender<EventBridgeItem>,
) -> Result<(std::sync::Arc<dyn hya_sdk::Client>, YacaNative), YacaNativeError>;
```

`EventBridgeItem` lives in `hya-yaca` and contains `Event(hya_sdk::GlobalEvent)` plus `Resync`; `hya-yaca` must not depend on `hya-tui`. `hya/src/main.rs` owns conversion into `AppEvent::Sse`, exactly as it currently forwards `NativeBridge` events (`hya/src/main.rs:191-201`).

## Bootstrap extraction steps and dependency risks

### What moves from `yaca-cli` to `yaca-app`

Move because both CLI and native hya need it:

- `config.rs` provider/MCP/plugin config loading (`crates/yaca-cli/src/config.rs:18-236`).
- `plugins.rs` plugin resolution and bundled opencode adapter lookup (`crates/yaca-cli/src/plugins.rs:8-114`).
- `formatter_config.rs` formatter plane loading (`crates/yaca-cli/src/formatter_config.rs:66-104`).
- `skills.rs` skill discovery/prompt section (`crates/yaca-cli/src/skills.rs:29-61`).
- `permission.rs` policy and `spawn_auto_responder` (`crates/yaca-cli/src/permission.rs:7-96`).
- Runtime functions from `main.rs`: `agent_with_model`, `build_session_engine`, `spawn_team_supervisor`, `resolve_runtime`, `open_store`, `compaction_config`, `host_info`, `offline_router` (`crates/yaca-cli/src/main.rs:95-117`, `135-295`, `314-394`).

### What stays in `yaca-cli`

- Clap parsing and subcommand dispatch (`crates/yaca-cli/src/main.rs:658-708`).
- CLI-only commands: auth login command wrappers, agent/model listing command formatting, `cmd_exec`, `cmd_goal`, `cmd_rpc`, `cmd_tail_session`, `cmd_sessions`, legacy TUI command dispatch.
- TCP binding in `serve.rs` remains CLI-only. `cmd_serve` should become: build `YacaApp`, bind listener, `axum::serve(listener, app.router())` (`serve.rs:39-46`). `cmd_tui_hya` should eventually launch hya native directly or simply shell out to `hya` without pre-binding a backend; the current in-process HTTP listener at `serve.rs:95-104` must be removed from default yaca UX.

### Cycle risks and how to avoid them

- **Do not make `yaca-server` depend on `yaca-app`.** Current dependency is CLI -> server (`crates/yaca-cli/Cargo.toml:16-24`, `serve.rs:4`); the new graph is CLI -> app -> server. Server remains below app.
- **Do not make `hya-sdk` depend on yaca crates.** `hya-sdk` currently depends only on serde/reqwest/eventsource/tokio/libc (`crates/hya-sdk/Cargo.toml:7-16`); preserve that for lean SDK use and external-server mode.
- **Do not put `hya_tui::AppEvent` into `hya-yaca`.** Keep the event bridge returning `GlobalEvent` to avoid a UI dependency cycle. `hya` owns forwarding.
- **Watch `auth` direction.** `yaca-cli/src/config.rs` calls `crate::auth::load_token` (`config.rs:208`). Move the token loader into `yaca-app::auth`; keep CLI auth commands as wrappers over `yaca_app::auth::{load_token, save_token, remove_token}`.

## `YacaNativeTransport` design

`YacaNativeTransport` is a `Clone + Send + Sync` transport over an `axum::Router` built from `YacaApp::router()`.

Fields:

```rust
#[derive(Clone)]
pub struct YacaNativeTransport {
    router: axum::Router,
    directory: std::sync::Arc<str>,
}
```

`Transport` implementation:

- `base_url()` returns a sentinel like `"native://yaca"`, not `http://...`, to make accidental URL use visible. This string is display-only because `ApiClient` never parses it.
- `directory()` returns the scoped directory.
- `request(method, path, body)`:
  1. Parse `method` into `http::Method`; unsupported method -> `SdkError::Http("unsupported method ...")` or a new `SdkError::Protocol` variant if expanding hya errors.
  2. Build a `http::Request<axum::body::Body>` with URI `path`, not a full URL.
  3. Insert `hya_sdk`'s directory header value `x-opencode-directory` (`crates/hya-sdk/src/lib.rs:3-6`). Because the const is currently `pub(crate)`, either make it `pub const DIRECTORY_HEADER: &str` or expose `hya_sdk::directory_header()`; do not duplicate the string in `hya-yaca`.
  4. If `body.is_some()`, serialize to bytes, set `content-type: application/json`, and use `Body::from(bytes)`. If none, use `Body::empty()`.
  5. Call `self.router.clone().oneshot(request).await` with `tower::ServiceExt`.
  6. Read response body with `http_body_util::BodyExt::collect().await?.to_bytes()`.
  7. If status is not 2xx, decode body as UTF-8 lossy and return `SdkError::Http(format!("status {status} for {method} {path}: {body}"))` so engine errors mid-turn surface to hya toasts/logging instead of silent `Null`.
  8. Empty body -> `Value::Null`; otherwise `serde_json::from_slice`.

Panic isolation:

- Wrap each oneshot in `tokio::spawn` only if real panics are observed. Prefer not spawning every request because it changes cancellation semantics. Instead add `AssertUnwindSafe(router.clone().oneshot(req)).catch_unwind()` only if adding `futures-util` catch-unwind is acceptable. If a handler panics, map to `YacaNativeError::RouterPanic` / `SdkError::Bridge("native router panicked")` and send a user-visible toast. Axum itself does not catch panics by default.
- Add a `tower::timeout::TimeoutLayer` only for short metadata endpoints if needed; do **not** timeout `/session/:id/message` because turns can legitimately run for minutes. Cancellation is through `/session/:id/abort` and `RunRegistry`, not request timeout.

## Event bridge design

### Chosen option: in-process `/global/event` SSE body

Use `router.clone().oneshot(GET /global/event)` and parse the response body stream in-process. Reasoning:

- Reuses yaca's existing global event projection, including `server.connected`, `server.heartbeat`, `session.error`, `message.updated`, `message.part.delta`, `message.part.updated`, `session.status`, and fallback behavior (`crates/yaca-server/src/opencode/event.rs:102-128`, `171-787`).
- Reuses the exact `GlobalEvent` shape hya's decoder expects (`hya-sdk/src/types.rs:22-58`).
- Avoids exporting private `ServerState` projection internals or duplicating `envelope_payload`, which would drift under backend changes.
- Still has no network: `oneshot` over `Router` constructs HTTP request/response values in memory, not sockets.

### Backpressure and ordering

Current yaca bus is a broadcast channel with capacity 1024 (`yaca-core/src/bus.rs:26-29`). A slow consumer will eventually receive `Lagged`; yaca's SSE handler converts that into an SSE event named `resync` (`yaca-server/src/opencode/event.rs:112-121`). The native bridge must make this explicit instead of dropping silently.

Implement `hya-yaca/src/events.rs` as a two-stage bridge:

1. **SSE reader task:** owns the response body stream and parses frames. It sends decoded items into a bounded `mpsc::channel<EventBridgeItem>` of default size 2048. It must `send().await`, not `try_send`, so a slow hya consumer applies backpressure to the SSE body. This preserves stream order until the upstream broadcast itself reports lag.
2. **UI forwarder task in `hya/src/main.rs`:** receives `GlobalEvent` from the bounded channel and sends `AppEvent::Sse` into the existing TUI event channel. If the TUI channel is still unbounded, keep the bounded native channel in front so memory pressure is limited to native bridge + TUI backlog; a later hardening task can migrate `hya_tui` to bounded channels.

SSE parsing rules:

- Parse `data: ...` events the same way `hya-sdk::stream_global_events` does (`events.rs:35-47`). Do not treat unknown JSON as fatal.
- Recognize the SSE event name `resync`. `eventsource-stream` exposes event type; if using it over in-memory bytes, map `event.event == "resync"` to `EventBridgeItem::Resync`.
- Heartbeats remain normal `GlobalEvent`s; hya already ignores `server.heartbeat` (`hya-sdk/src/types.rs:53-57`, `hya-tui/src/app.rs:162-168`).

Resync path:

- On `resync`, the bridge sends `EventBridgeItem::Resync { reason: Lagged }` and then continues reading.
- Add `hya_tui::app::AppEvent::BackendResync` or keep it in `hya/main.rs` as: when a resync arrives, inspect the current route and call `client.session_messages(active_session)` plus `client.session_get(active_session)` to hydrate durable state. The existing `Client` already exposes these methods (`hya-sdk/src/client.rs:39-49`, `269-300`).
- If plumbing active route into main is too invasive, first implementation sends a visible toast “Event stream lagged; reload current session” and calls existing `AppEvent::LoadSession(active_session)` from runtime. Do not ignore resync; a detected drop without a snapshot is still a data-loss bug.
- Track last seen event ids only for diagnostics. `GlobalEvent.payload.id` is event/projection id (`hya-sdk/src/types.rs:35-44`), not guaranteed replay cursor. Durable session replay is by session store, not event id.

Shutdown:

- The bridge owns a `CancellationToken`. `Drop` on `YacaNative` cancels it and aborts the task if it does not exit promptly.
- The SSE reader selects between body `next()` and `cancel.cancelled()`. On cancel, it drops the response body so the in-process stream unsubscribes from `BroadcastStream`.
- The forwarder exits when the bounded channel closes or when hya sends `Quit`.

Failure handling:

- If `/global/event` oneshot returns non-2xx, fail native startup before `BackendReady` and show `Backend failed to start: ...` using existing `spawn_connect` error path (`hya/src/main.rs:83-88`).
- If stream parsing fails after readiness, send `AppEvent::Toast("Native event stream failed; reconnecting...")` and rebuild a fresh in-process SSE request with exponential backoff capped at 5s. Because this is in-process, reconnect should be fast; repeated failure indicates a router/projection bug and should not spin.
- On reconnect, send `BackendResync` before consuming live events to close any gap.

## Lifecycle guard semantics

`hya` holds exactly one guard for native yaca:

```rust
enum Transport {
    YacaNative(hya_yaca::YacaNative),
    OpencodeNative(hya_sdk::NativeBridge),
    Http { server: ServerMode, sse: JoinHandle<()>, keep_streaming: Arc<AtomicBool> },
}
```

`YacaNative::Drop` semantics:

1. Set a `closing` atomic/cancellation token so request/event bridge paths return `SdkError::Bridge("native runtime closed")` rather than hanging.
2. Cancel the event bridge token, await `shutdown()` when called explicitly, and abort the bridge task in Drop if the caller forgot explicit shutdown. Drop cannot `.await`, so `shutdown(self)` is the clean path and Drop is best-effort.
3. Cancel yaca background tasks owned by `YacaApp`: team supervisor loop, optional auto-responder, event bridge, any watchdogs.
4. Drop `AppState`/router clones, then drop `PluginHost`, then `McpManager`. This order matters: tools/hooks may still hold plugin/MCP clients while requests are winding down. The guard should stop accepting new requests before dropping those resources.
5. Dropping `McpManager` drops MCP `ChildGuard`s (`yaca-mcp/src/manager.rs:41-47`, `yaca-mcp/src/client.rs:56-89`); dropping `PluginHost` drops plugin child guards and sends shutdown (`yaca-plugin/src/client.rs:43-77`).
6. No TCP cleanup is needed in native mode. The old `ServerHandle::drop` port-kill logic (`hya-sdk/src/server.rs:129-169`) must never run for native yaca.

Add explicit method:

```rust
impl YacaNative {
    pub async fn shutdown(mut self) -> Result<(), YacaNativeError>;
}
```

Update `hya/src/main.rs` shutdown to prefer explicit async shutdown:

```rust
if let Ok(Some(transport)) = connect.await {
    transport.shutdown().await?;
}
```

The HTTP and Bun guard variants can keep synchronous best-effort shutdown, but native yaca should use async shutdown to let plugin/MCP subprocesses exit cleanly.

## Engine errors mid-turn and cancellation

Problem: legacy `/session/:id/message` awaits `run_turn_with_external_dirs` and propagates errors (`session_prompt_legacy.rs:47-58`); v2 `/api/session/:id/prompt` spawns the run and currently discards the result (`session_prompt.rs:110-120`). `SessionEngine::run_turn_with_external_dirs` starts an assistant message then can return `Err(CoreError)` before emitting `MessageFinished` for provider/store errors (`yaca-core/src/engine/turn.rs:37-111`, `261-267`).

Plan:

- Add a core hardening task in `yaca-core`: after `MessageStarted`, wrap the inner turn loop so any `CoreError` emits both `Event::Error { session: Some(session), code, message }` and `Event::MessageFinished { finish: FinishReason::Error }` before returning `Err`. `event.rs` already projects `Event::Error` to `session.error` (`yaca-server/src/opencode/event.rs:171-178`, `735-763`). This fixes HTTP and native paths together.
- In `yaca-server/src/opencode/session_prompt.rs`, replace `let _ = engine.run_turn...await` with logging and error-event reliance; keep the `RunGuard` in the task so busy state clears on exit (`session_prompt.rs:115-120`, `runs.rs:106-113`).
- `/session/:id/abort` should continue to call `RunRegistry::cancel` (`session_legacy.rs:361-368`). Native request transport must support that route exactly as HTTP.
- `YacaNative::shutdown()` should cancel the global runtime token and, if possible, call all active run tokens via a new `RunRegistry::cancel_all()` only if exposed by server state. If not exposed, rely on dropping request/router state and active `RunGuard`s. Do not leave running turns alive after hya exits.

## `hya/src/main.rs` wiring and flags

Current flags: `--server`, `--http`, `--opencode`, `--yaca-bin`, `--version`, `--help` (`hya/src/main.rs:364-421`). Change semantics:

- Default: native yaca in-process via `hya-yaca`.
- `--http`: spawn `yaca serve` and use existing `HttpClient` + `stream_global_events`. This keeps a fallback and preserves `--yaca-bin`.
- `--server <url>`: attach to external server over HTTP and SSE, no process ownership.
- `--opencode`: use existing Bun `NativeBridge` by default, unless combined with `--http` to spawn `opencode serve`.
- Add `--db <path>` and `--model <provider/model>` if hya should expose yaca-native runtime knobs directly; otherwise default to yaca config and store path already used by CLI.
- Add `--yolo` only if yaca CLI default behavior needs parity. Otherwise use `PermissionMode::FrontendRequests` so permission/question requests flow to hya via existing `/permission` and `/question` endpoints.

`Transport::connect` decision tree:

1. `args.server.is_some()` -> HTTP attach.
2. `args.opencode && !args.http` -> existing `NativeBridge`.
3. `args.opencode && args.http` -> existing `ServerHandle::spawn` opencode HTTP.
4. `args.http` -> existing `ServerHandle::spawn_yaca` yaca HTTP.
5. else -> `hya_yaca::connect(directory, options)` native yaca.

Background fetches (`commands`, `models`, `mcp_status`, `lsp_status`, `formatter_status`, `plugins`) can remain unchanged because the `Client` trait is unchanged (`hya/src/main.rs:203-261`).

## How to prove zero HTTP

Automated proof must combine code-path tests and runtime socket observation:

1. **Compile-time/code-path proof:** Add a native integration test that constructs `hya_yaca::connect` and drives `Client::session_create` + `Client::session_prompt` without constructing `reqwest::Client`. The native transport crate should not depend on `reqwest`.
2. **Runtime no-listener proof:** In a manual tmux QA, run native `hya` under `strace -f -e trace=network` and assert no `bind(`, `listen(`, or `connect(` to `127.0.0.1`/localhost occurs during a full turn. Some provider calls may legitimately `connect` to external model APIs; for a deterministic offline provider run, set no provider config so `resolve_runtime` falls back to `DevProvider` (`yaca-cli/src/main.rs:331-382`), then there should be no network syscalls at all.
3. **Socket table proof:** While hya is running native, capture `ss -ltnp` before/after and prove no new listener for the hya PID tree. The old HTTP path prints/listens via `TcpListener::bind` (`yaca-cli/src/serve.rs:39-46`, `95-104`); native must not call these functions.
4. **Negative regression test:** Keep an HTTP-mode test that confirms `--http` still binds so the QA can distinguish the two modes.

Manual tmux QA script outline:

```bash
tmux new-session -d -s hya-native-qa 'cd /chivier-disk/yanweiye/Projects/yaca-hya-native && env -u YACA_MODEL XDG_CONFIG_HOME=$(mktemp -d) strace -f -o /tmp/hya-native.net -e trace=network target/debug/hya'
# In the TUI: create a session, send "say hello", wait for assistant response, quit.
ss -ltnp > /tmp/hya-native.ss
grep -E 'bind\(|listen\(|connect\(' /tmp/hya-native.net
# Expected for offline run: no lines. If provider config is present: no bind/listen and no localhost connect.
```

## Test strategy

### Unit tests

- `crates/hya-yaca/src/transport.rs`
  - Unsupported method returns typed SDK error.
  - Directory header is injected by a test router that echoes headers.
  - JSON body is sent and empty body returns `Value::Null`.
  - Non-2xx response maps status + body to error.
- `crates/hya-yaca/src/events.rs`
  - Parse `data: {"payload":...}` into `GlobalEvent` using the same verified shape as `hya-sdk/src/types.rs:236-250`.
  - Parse SSE event `resync` into `EventBridgeItem::Resync`.
  - Bounded channel blocks instead of dropping: use `tokio::time::pause` or a small buffer and assert the reader task is pending until receiver drains.
- `crates/yaca-app/src/runtime.rs`
  - Missing config yields offline runtime and no MCP/plugin children.
  - Disabled MCP/plugins are skipped, preserving existing behavior from `config.rs` and `plugins.rs` tests.
  - Permission modes produce pending requests vs auto-responder as expected.

### In-process integration tests

- `crates/hya-yaca/tests/native_turn.rs`
  - Build `YacaNative` with `XDG_CONFIG_HOME` pointing at an empty temp dir and `SessionStore::connect_memory()` or temp sqlite path.
  - Get `Arc<dyn hya_sdk::Client>`.
  - Call `config_get`, `session_create`, then legacy `session_prompt(session.id, json!({"text":"hello"}))`. The legacy route awaits the full run (`session_prompt_legacy.rs:47-58`), so this asserts a full turn completes.
  - Drain events from native event receiver and assert at least `server.connected`, `message.updated`, and final assistant `message.updated`/`message.part.updated` arrive.
  - Assert no `ServerHandle` is constructed and no `TcpListener::bind` is reachable. If possible, wrap socket syscalls with a test-only `NoNetworkGuard`; otherwise rely on process-level manual QA for syscall proof.
- `crates/yaca-cli/tests/native_bootstrap_parity.rs`
  - After extraction, test `cmd_serve` and `YacaApp::build` share the same `AppState` options: default agent, global agents true for serve/native, formatter status, workspace adapters.
- `crates/yaca-core` hardening test
  - Fake provider returns error after `MessageStarted`; replay must include `Event::Error` and `MessageFinished { finish: Error }`.

### Existing related tests to keep passing

- `hya-sdk` client parsing tests (`client.rs:636-682`, `types.rs:231-304`).
- MCP manager partial-failure tests (`yaca-mcp/src/manager.rs:157-225`).
- Plugin restart/backpressure behavior should remain unchanged (`yaca-plugin/src/host.rs:160-167`).

### Verification commands

Run after each task group:

```bash
cargo test -p yaca-app
cargo test -p hya-yaca
cargo test -p hya-sdk
cargo test -p hya
cargo test -p yaca-server -p yaca-core
cargo build -p hya -p yaca-cli
```

Final accept gate:

```bash
cargo build -p hya
env XDG_CONFIG_HOME=$(mktemp -d) strace -f -o /tmp/hya-native.net -e trace=network target/debug/hya
grep -E 'bind\(|listen\(|connect\([^)]*(127\.0\.0\.1|localhost)' /tmp/hya-native.net
# Expected: no matches for native/offline run.
```

## Production risks and mitigations

1. **Silent event loss under load.** The source bus is broadcast(1024), and `/global/event` already reports lag as `resync` (`yaca-core/src/bus.rs:26-29`, `yaca-server/src/opencode/event.rs:112-121`). Native must never drop with `try_send`; use bounded `send().await`, surface resync to hya, and hydrate current session from durable `Client::session_messages`.
2. **Shutdown leaks background work or children.** Current MCP/plugin children rely on Drop guards (`yaca-mcp/src/client.rs:56-89`, `yaca-plugin/src/client.rs:43-77`). Native guard must stop accepting requests, cancel event bridge, cancel/abort background tasks, then drop plugin/MCP owners in order. Drop is best-effort; explicit `shutdown().await` is mandatory in `hya/main.rs`.
3. **Bootstrap extraction creates dependency cycles.** Keep `yaca-app` above server/core and below CLI/hya. Do not move CLI command parsing into app. Do not make `hya-sdk` depend on yaca. Do not make `yaca-server` know about runtime config.
4. **Engine error leaves UI permanently “working”.** Harden `SessionEngine::run_turn_with_external_dirs` to emit `session.error` + `MessageFinished(Error)` on provider/store errors after assistant message start. This protects both HTTP and native.
5. **MCP readiness blocks perceived startup.** `McpManager::connect_all` waits on all configs (`yaca-mcp/src/manager.rs:50-81`), and current hya allows 180s for yaca HTTP readiness (`hya-sdk/src/server.rs:18-20`). Native should preserve a `readiness_timeout` around `YacaApp::build`; hya continues rendering with `PendingClient` while bootstrap runs.
6. **Panic in in-process router kills hya.** Add panic boundary around request execution or at least around native connector task; convert panic to typed error/toast. Longer term, add `tower` catch-panic layer if acceptable.
7. **Permission/question responders mismatch.** In non-yolo native, do not auto-approve. Wire `asks` and `questions` into `AppState::with_permission_requests` / `with_question_requests` exactly as `serve.rs` does (`crates/yaca-cli/src/serve.rs:26-38`) so hya can reply through existing routes (`hya-sdk/src/client.rs:477-522`).
8. **HTTP sneaks back through helper code.** Keep native transport in `hya-yaca` free of `reqwest`; gate `reqwest::Client::new()` behind HTTP branches in `hya/main.rs` (`main.rs:150-164`). Add manual syscall QA and avoid any `TcpListener::bind` in native path.

## Atomic implementation order for parallel subagents

### Task 1: Extract reusable app bootstrap

**Files:**
- Create: `crates/yaca-app/Cargo.toml`
- Create: `crates/yaca-app/src/{lib.rs,error.rs,runtime.rs,config.rs,auth.rs,plugins.rs,formatter_config.rs,permission.rs,skills.rs}`
- Modify: `Cargo.toml`
- Modify: `crates/yaca-cli/Cargo.toml`
- Modify: `crates/yaca-cli/src/main.rs`
- Modify: `crates/yaca-cli/src/serve.rs`

**Produces:** `yaca_app::{YacaApp, YacaAppOptions, PermissionMode, YacaAppError}` and CLI builds using them.

- [ ] Move config/plugin/formatter/permission/skills helpers into `yaca-app` without changing behavior.
- [ ] Move runtime bootstrap functions into `yaca_app::runtime` and return typed `YacaAppError` at public boundaries.
- [ ] Update `yaca-cli` to import from `yaca_app` and keep command behavior unchanged.
- [ ] Run `cargo test -p yaca-app -p yaca-cli`.

### Task 2: Harden engine turn errors

**Files:**
- Modify: `crates/yaca-core/src/engine/turn.rs`
- Modify/Add tests near `crates/yaca-core/src/engine/turn.rs` or existing test module
- Modify: `crates/yaca-server/src/opencode/session_prompt.rs` only for spawned-run error logging if needed

**Produces:** provider/store errors after `MessageStarted` become durable `Event::Error` and `MessageFinished(Error)`.

- [ ] Add failing test with fake provider error mid-turn.
- [ ] Refactor `run_turn_with_external_dirs` into outer message/error wrapper + inner loop.
- [ ] Preserve cancellation behavior: cancellation still emits `FinishReason::Cancelled` (`turn.rs:50-63`).
- [ ] Run `cargo test -p yaca-core -p yaca-server`.

### Task 3: Build `hya-yaca` native transport

**Files:**
- Create: `crates/hya-yaca/Cargo.toml`
- Create: `crates/hya-yaca/src/{lib.rs,error.rs,transport.rs}`
- Modify: `Cargo.toml`
- Modify: `crates/hya-sdk/src/lib.rs` only to expose the directory header safely

**Produces:** `YacaNativeTransport: hya_sdk::Transport` using `Router::oneshot`.

- [ ] Unit-test method parsing, directory header injection, JSON request/response, non-2xx error mapping.
- [ ] Ensure no `reqwest` dependency in `hya-yaca/Cargo.toml`.
- [ ] Run `cargo test -p hya-yaca -p hya-sdk`.

### Task 4: Build native event bridge

**Files:**
- Create: `crates/hya-yaca/src/events.rs`
- Modify: `crates/hya-yaca/src/lib.rs`
- Possibly modify: `crates/hya-tui/src/app.rs` and `crates/hya-tui/src/app/runtime.rs` for explicit `BackendResync`

**Produces:** bounded in-process `/global/event` stream with cancellation, reconnect, lag/resync signal.

- [ ] Unit-test `GlobalEvent` parsing and `resync` parsing.
- [ ] Unit-test bounded-channel backpressure with buffer size 1.
- [ ] Add `EventBridgeItem::{Event(GlobalEvent), Resync}` and expose receiver to `hya`.
- [ ] If adding `BackendResync`, handle it in runtime by hydrating active session; otherwise document visible toast + reload behavior and add a follow-up task before production default.
- [ ] Run `cargo test -p hya-yaca -p hya-tui`.

### Task 5: Implement native lifecycle guard

**Files:**
- Modify: `crates/hya-yaca/src/lib.rs`
- Modify: `crates/hya-yaca/src/error.rs`
- Modify: `crates/yaca-app/src/runtime.rs`

**Produces:** `YacaNative` guard with explicit `shutdown().await` and Drop fallback.

- [ ] Add cancellation token and closing state.
- [ ] On `shutdown`, stop event bridge, stop accepting requests, drop router/app/plugin/MCP owners in order.
- [ ] Add a test that dropping the guard closes the event stream and future transport requests fail.
- [ ] Run `cargo test -p hya-yaca -p yaca-app`.

### Task 6: Wire hya default path and flags

**Files:**
- Modify: `crates/hya/Cargo.toml`
- Modify: `crates/hya/src/main.rs`
- Possibly modify: `crates/hya-sdk/src/server.rs` docs/help only if HTTP fallback wording changes

**Produces:** `hya` defaults to native yaca; HTTP/opencode paths remain explicit.

- [ ] Add `hya-yaca` dependency.
- [ ] Change `Transport` enum to include `YacaNative` and rename current `Native` to `OpencodeNative` for clarity.
- [ ] Move `reqwest::Client::new()` into HTTP-only branch.
- [ ] Update help text: default native yaca, `--http` yaca HTTP fallback, `--server` external, `--opencode` existing Bun bridge.
- [ ] Await native shutdown explicitly after TUI exits.
- [ ] Run `cargo test -p hya && cargo build -p hya`.

### Task 7: In-process full-turn integration test

**Files:**
- Create: `crates/hya-yaca/tests/native_turn.rs`
- Possibly create fixtures under `crates/hya-yaca/tests/fixtures/`

**Produces:** automated proof that a full hya client session turn works in-process.

- [ ] Set `XDG_CONFIG_HOME` to an empty temp dir so yaca uses offline provider.
- [ ] Build native runtime with temp DB/memory store.
- [ ] Call `session_create`, `session_prompt`, `session_messages` through `Arc<dyn hya_sdk::Client>`.
- [ ] Drain event receiver and assert connected + assistant completion/error-free finish.
- [ ] Run `cargo test -p hya-yaca --test native_turn`.

### Task 8: Zero-HTTP QA harness/documentation

**Files:**
- Create: `.trellis/tasks/06-24-hya-yaca-native-sdk/research/native-zero-http-qa.md` or repo docs path chosen by maintainer
- Optionally create: `scripts/qa/hya-native-no-http.sh`

**Produces:** repeatable tmux/strace/socket-table QA for accept gate.

- [ ] Script/build instructions for offline native run.
- [ ] `strace -f -e trace=network` command with expected no `bind/listen/localhost connect` output.
- [ ] `ss -ltnp` before/after capture instructions.
- [ ] Negative comparison for `hya --http` showing listener exists.

### Task 9: Final full verification

**Files:** no code files unless fixing failures.

**Produces:** acceptance evidence.

- [ ] Run related tests: `cargo test -p yaca-app -p hya-yaca -p hya-sdk -p hya -p yaca-server -p yaca-core`.
- [ ] Run build: `cargo build -p hya -p yaca-cli`.
- [ ] Run manual tmux QA and attach `/tmp/hya-native.net` + `/tmp/hya-native.ss` excerpts proving no native socket.
- [ ] Record any provider-network exception separately; offline accept gate must have zero network syscalls.

## Planner B recommendation

Do not ship this as “native by default” until Tasks 2, 4, 5, 7, and 8 are complete. The simple transport is not the hard part; the production risks are dropped event streams, background child leaks, and turns that error without terminal UI state. The safest implementation sequence is bootstrap extraction first, then turn-error hardening, then native transport/events/lifecycle, then hya default flip only after the in-process full-turn test and no-socket QA pass.
