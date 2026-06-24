# Context Brief — Wire `hya` ↔ `yaca` via native in-process Rust SDK (no HTTP)

> Shared grounding for parallel planners. All file paths are in the worktree
> `/chivier-disk/yanweiye/Projects/yaca-hya-native` (branch `feat/hya-yaca-native-sdk`, off origin/main `7089c62`).
> Read the cited files directly to verify; do not re-discover the whole tree.

## Goal & Accept Gate

- **Goal:** Replace `hya-sdk`'s HTTP (reqwest) + SSE (`/global/event`) transport to the yaca backend with a
  **native in-process Rust binding**. hya links the yaca backend as a library and drives it directly.
- **Accept gate (hard):** `hya` completes a full session turn against `yaca` with **ZERO HTTP calls** — no TCP
  listener bound, no reqwest request, no localhost port. Verified by running the binary AND proving no socket is opened.

## Current architecture (verified by reading source)

### hya frontend (crates `hya`, `hya-sdk`, `hya-tui`)
- **Transport seam** — `crates/hya-sdk/src/client.rs:14`:
  ```rust
  #[async_trait] pub trait Transport: Send + Sync {
      fn base_url(&self) -> &str;
      fn directory(&self) -> &str;
      async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value>;
  }
  ```
  `Client` (the full TUI-facing API: config_get, session_list, session_messages, agents, find_files,
  commands, models, mcp_status, lsp_status, formatter_status, plugins, session_create, session_prompt,
  session_shell, session_command, permission_reply, question_reply/reject, session_rename/delete/compact/
  revert/unrevert/abort) is implemented ONCE over any `Transport` via `ApiClient<T>`. `ApiClient::with_transport(T)` is public.
- **Existing transports:** `HttpTransport` (reqwest, client.rs:178) and `NativeTransport` (native.rs — spawns a **Bun** process running opencode TS `native-bridge.ts`, framed JSON over stdio). Neither is native-Rust-to-yaca.
- **Events:** `crates/hya-sdk/src/events.rs` `stream_global_events()` consumes SSE `GET /global/event` via reqwest+eventsource-stream → `GlobalEvent`. `crates/hya/src/main.rs` forwards `GlobalEvent` → `AppEvent::Sse`.
- **hya binary** (`crates/hya/src/main.rs`): default mode spawns `yaca serve` (`ServerHandle`, `server.rs`) then connects via `HttpClient` + SSE task. Flags: `--server <url>`, `--http`, `--opencode`, `--yaca-bin`.
- `hya` depends on `hya-sdk` + `hya-tui` only (Cargo.toml). hya-sdk deps: serde, reqwest, eventsource-stream, tokio, async-trait, libc.

### yaca backend (crates `yaca-server`, `yaca-core`, `yaca-cli`, …)
- **Router entry** — `crates/yaca-server/src/lib.rs:31`: `pub fn router(state: AppState) -> axum::Router`.
  It `.merge(opencode::router())` — `crates/yaca-server/src/opencode.rs` serves EXACTLY the opencode-compatible
  routes hya's `Client` calls (`/config`, `/session`, `/session/{id}/message`, `/agent`, `/global/event` SSE,
  `/find/file`, `/command`, `/config/providers`, `/mcp`, `/lsp`, `/formatter`, `/permission/{id}/reply`,
  `/question/...`, `/session/{id}/{shell,command,summarize,revert,unrevert,abort}`, PATCH/DELETE `/session/{id}`).
- **AppState** — `crates/yaca-server/src/state.rs:11`: `{ engine: Arc<SessionEngine>, agent: Arc<AgentSpec>,
  permission_requests, question_requests, mcp_manager, workspace_adapters, formatter_status, default_agent,
  include_global_agents }`. Builder: `AppState::new(engine, agent).with_question_requests(rx)
  .with_mcp_manager(m).with_workspace_adapters(v).with_default_agent(a).with_global_agents(b)
  .with_permission_requests(rx)`. `ServerState::new(app)` adds runs/global/pty/tui/project/mcp_http state.
- **Event bus** — `crates/yaca-core/src/bus.rs`: `EventBus(broadcast::Sender<Envelope>)`; `engine.bus().subscribe()
  -> broadcast::Receiver<Envelope>`. The `/global/event` SSE handler (`opencode/event.rs`, 796 LOC) PROJECTS
  raw `Envelope`s into opencode-shaped `GlobalEvent` frames (e.g. `server.connected`) — hya consumes the projected shape.
- **Bootstrap lives in the `yaca-cli` BINARY** (`crates/yaca-cli/src/main.rs`) — NOT a library:
  - `resolve_runtime(model_override) -> RuntimeConfig{ router: ProviderRouter, model, models, mcp, plugins, default_agent }` via `config::load()` (falls back to offline DevProvider).
  - `build_session_engine(store, router, model, mcp, plugins) -> (Arc<SessionEngine>, asks rx, questions rx, McpManager, Arc<PluginHost>)`.
  - `agent_with_model(model) -> AgentSpec`; `open_store(db) -> SessionStore`.
  - `serve.rs::cmd_serve` / `cmd_tui_hya` assemble `AppState` then `TcpListener::bind` + `axum::serve(listener, router(state))`.
  - Bootstrap depends on: yaca-core, yaca-provider, yaca-store, yaca-tool, yaca-mcp, yaca-plugin + cli-local `config`/`plugins`/`formatter_config`/`permission` modules.
- Only `yaca-cli` consumes `yaca_server::{AppState, router}` today.

## The native seam (decisions to make in the plan)
1. **Request transport:** A new `Transport` impl that calls the in-process `axum::Router` via
   `tower::ServiceExt::oneshot` — build `http::Request` (method, path, `x-opencode-directory` header, JSON body),
   read `http::Response` body via `http_body_util::BodyExt`. NO TCP, NO reqwest. (This is the Rust analogue of
   opencode's in-process `app.fetch`.)
2. **Event bridge:** EITHER oneshot `GET /global/event` and read the SSE body stream in-process (reuses the 796-LOC
   projection), OR `engine.bus().subscribe()` directly and re-project. Trade-off: reuse vs. coupling.
3. **Bootstrap extraction:** Move the engine/AppState bootstrap from the `yaca-cli` binary into a LIBRARY so
   `hya-sdk` (or a new crate) can build `AppState` in-process. New `yaca-app` lib? bootstrap module in `yaca-server`?
4. **Crate boundary for the native transport:** new `hya-yaca` crate (depends on hya-sdk + yaca-server + bootstrap lib)
   vs. a feature-gated `native-yaca` module inside `hya-sdk`. Keep the HTTP path buildable/lean either way.
5. **hya `main.rs` wiring:** native as default vs. `--native` flag; how the lifecycle guard (engine, mcp, plugin host) is owned + torn down; how the event task is spawned.

## Constraints
- Workspace: edition 2024, rust 1.91, `members = ["crates/*", "xtask"]`. hya* crates ARE members → one `cargo build` builds both.
- **No HTTP** anywhere on the native path (the whole point).
- **Atomic commits**; conventional-commit style (see `git log`).
- **Do NOT touch the `/Projects/yaca` main checkout** — another agent is actively editing it. Work only in this worktree.
- **Periodically rebase/merge `origin/main`** — the yaca backend moves fast; adopt new backend changes.
- No `as any`-style escapes; typed errors; keep modules within the repo's ~250 LOC norm.

## Required deliverable from each planner
A concrete, decision-complete plan covering: (a) crate/module layout decision + rationale; (b) exact steps to extract
the bootstrap into a library (what moves, what stays, dependency graph, cycle risks); (c) `YacaNativeTransport` design
(oneshot mechanics, body read, error mapping, directory header); (d) event-bridge design (which option, backpressure,
shutdown, lag/resync); (e) `hya/main.rs` wiring + flags + lifecycle/teardown; (f) accept-gate verification method
(how to PROVE zero HTTP); (g) test strategy (unit + integration + manual tmux QA); (h) risks/failure modes + mitigations;
(i) implementation order decomposed into atomic units suitable for PARALLEL subagents (with file-level boundaries).
