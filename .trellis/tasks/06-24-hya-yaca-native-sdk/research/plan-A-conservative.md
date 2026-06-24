# Plan A — CONSERVATIVE / MINIMAL framing

**Wire `hya` ↔ `yaca` via a native in-process Rust SDK (no HTTP).**

Framing: smallest correct change. Re-use the `Transport` seam, the existing
`yaca_server::router(state)`, the existing `/global/event` projection, and the
`tower::oneshot` + `http_body_util::BodyExt` patterns already used in 30+ tests
(`crates/yaca-server/tests/api.rs:53`, `opencode_event_api.rs:105`,
`opencode_prompt_async_api.rs:211`). The only genuinely new code is a small
in-process `Transport` impl and a small SSE-body-in-process event bridge,
landing behind a Cargo feature so the HTTP path is untouched.

---

## 0. Bottom line + non-goals

**Bottom line.** Add (1) one new tiny crate `yaca-bootstrap` (move the bootstrap
glue out of the `yaca-cli` binary into a library — nothing else), (2) one new
feature-gated module `hya_sdk::yaca_native` (~150 LOC: `YacaNativeTransport`
over `tower::oneshot`, plus `spawn_yaca_event_bridge` that oneshot-fetches
`/global/event` and reuses `eventsource_stream`), (3) a new `--native` flag in
`hya/main.rs` that owns an `EngineRuntime` guard. No new traits, no new SDK
public surface, no new wire protocol.

**Explicit non-goals (what NOT to build).**
- **No new `Transport` trait.** The existing one in `crates/hya-sdk/src/client.rs:14`
  is exactly the right seam — adding a parallel trait doubles the surface for zero
  benefit. The dynamic `Arc<dyn Client>` storage in `hya/main.rs:122` works as-is.
- **No re-projection of the bus.** The 796-line `/global/event` projection in
  `crates/yaca-server/src/opencode/event.rs` already produces exactly the
  `GlobalEvent` shape hya's `events.rs` consumes. Re-projecting from
  `engine.bus().subscribe()` duplicates that logic and binds hya to internal
  envelope shapes — exactly the coupling the projection was designed to hide.
- **No new `hya-yaca` crate.** A feature on `hya-sdk` is one Cargo.toml change vs.
  a whole new crate to maintain. The feature also documents the dep boundary in
  source.
- **No re-implementing engine calls.** `tower::ServiceExt::oneshot` on
  `axum::Router` IS the in-process call mechanism. It's already the test idiom.
- **No moving `auth`, `agent_cmd`, `models_cmd`, `tui*`, `rpc`, `cli_args`** out
  of `yaca-cli`. Only the bootstrap functions the engine actually needs.
- **No changes to `crates/hya-sdk/src/{client,server,events,native}.rs`** beyond
  one `#[cfg(feature = "http")]` if we choose to gate (Optional — see §2.5).
- **No new error variants.** `SdkError::Http(String)` already covers non-2xx and
  serialization failures; that's what `HttpTransport` does at
  `client.rs:201,210-215`. Reuse it.

---

## 1. Crate / module layout (decision + rationale)

### Decision

```
crates/
├── yaca-bootstrap/                 # NEW, tiny (~600 LOC moved verbatim)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                  # re-exports: build_engine, EngineRuntime,
│       │                           # agent_with_model, open_store, resolve_runtime,
│       │                           # PermissionPolicy, spawn_auto_responder
│       ├── runtime.rs              # build_engine + EngineRuntime + RuntimeConfig
│       ├── agent.rs                # agent_with_model + discover_context_files
│       │                           # + today + skill_dirs + skills helpers (verbatim
│       │                           # move of yaca-cli/src/skills.rs)
│       ├── config.rs               # verbatim move of yaca-cli/src/config.rs
│       ├── plugins.rs              # verbatim move of yaca-cli/src/plugins.rs
│       ├── formatter_config.rs    # verbatim move of yaca-cli/src/formatter_config.rs
│       ├── permission.rs           # verbatim move of yaca-cli/src/permission.rs
│       └── team.rs                 # spawn_team_supervisor (moved from main.rs)
│
├── hya-sdk/                        # ONE new module + ONE Cargo feature
│   ├── Cargo.toml                  # add `native-yaca` feature (opt-in)
│   └── src/
│       ├── lib.rs                  # add `#[cfg(feature="native-yaca")] pub mod yaca_native;`
│       └── yaca_native.rs          # NEW (~150 LOC): YacaNativeTransport
│                                   # + spawn_yaca_event_bridge
│                                   # + YacaNativeClient (= ApiClient<YacaNativeTransport>)
│
├── hya/                            # binary wiring only
│   ├── Cargo.toml                  # enable hya-sdk/native-yaca
│   └── src/main.rs                 # add --native flag + Transport::Yaca variant
│
├── yaca-cli/                       # use yaca-bootstrap; delete duplicates
│   ├── Cargo.toml                  # +yaca-bootstrap dep
│   └── src/
│       ├── main.rs                 # `use yaca_bootstrap::*;` — remove moved fns
│       ├── serve.rs                # `use yaca_bootstrap::*;` — unchanged otherwise
│       ├── tui.rs                  # unchanged (already uses internal helpers)
│       └── …                       # auth_cmd, agent_cmd, models_cmd, rpc: stay
```

### Rationale

- **Why a new crate, not a `yaca_server::bootstrap` module?** `yaca-server`
  currently depends on `yaca-core`, `yaca-mcp`, `yaca-proto`, `yaca-store`,
  `yaca-tool` only (see `crates/yaca-server/Cargo.toml`). Bootstrap needs
  `yaca-provider` and `yaca-plugin`. Folding bootstrap into `yaca-server` would
  expand `yaca-server`'s role from "HTTP routing over engine" to "router + config
  + provider loader + plugin host", and force every downstream consumer of
  `yaca-server` (and there will be more as the project grows) to drag in
  provider/plugin code they don't need. A focused 600-LOC `yaca-bootstrap` crate
  is the smaller intrusion.
- **Why feature-gate `yaca_native` instead of always-on?** A `hya-sdk` consumer
  that only needs the HTTP transport shouldn't be forced to compile
  `yaca-server` + `yaca-bootstrap` + `axum` + `http-body-util`. Gating also
  documents the boundary and makes the proof-of-zero-HTTP simpler (see §6).
- **Why not also gate `HttpTransport` behind `feature = "http"`?** It would
  shrink the native binary further, but it changes the existing
  `crates/hya-sdk/src/{client,events,server}.rs` files (they'd need `#[cfg]`
  attributes), which violates the "smallest change" framing. Land this PR
  without that change; mark it as Optional future consideration (§Edge cases).

### Dependency graph (no cycles)

```
yaca-proto, yaca-provider, yaca-tool, yaca-store, yaca-mcp, yaca-plugin
   └── yaca-core
          └── yaca-bootstrap (NEW)
                 ├── yaca-cli         (was direct: yaca-cli → all-of-above)
                 └── hya-sdk[native-yaca]  ──┐
                       └── hya-tui          │
                              └── hya       │
yaca-server ── yaca-cli                     │
       └── hya-sdk[native-yaca] ────────────┘
```

`yaca-cli` ALREADY depends on `yaca-server` (the `serve` command); adding
`yaca-bootstrap` adds no cycle. `hya-sdk[native-yaca]` adds a new edge into
`yaca-server` + `yaca-bootstrap` — both leaves, no cycle.

---

## 2. Bootstrap extraction — exact steps

### 2.1 What moves (file → file, verbatim)

| from `crates/yaca-cli/src/…`                | to `crates/yaca-bootstrap/src/…` |
|---------------------------------------------|----------------------------------|
| `config.rs` (400 LOC, public types already) | `config.rs`                      |
| `plugins.rs` (246 LOC)                      | `plugins.rs`                     |
| `formatter_config.rs` (161 LOC)             | `formatter_config.rs`            |
| `permission.rs` (189 LOC)                   | `permission.rs`                  |
| `skills.rs` (110 LOC)                       | `agent.rs` (folded in)           |
| `main.rs`: `today`, `discover_context_files`, `skill_dirs`, `agent_with_model`, `compaction_config`, `spawn_team_supervisor`, `host_info`, `headless_policy`, `offline_router`, `RuntimeConfig`, `resolve_runtime`, `open_store`, `build_session_engine` | split into `agent.rs` + `runtime.rs` + `team.rs` |

### 2.2 What stays in `yaca-cli`

- `main.rs` retains: `cmd_exec`, `cmd_rpc`, `cmd_goal`, `cmd_tui`,
  `cmd_tail_session`, `cmd_sessions`, the `main()` entry point and `clap` parse.
- `serve.rs`, `tui.rs`, `tui/*`, `auth.rs`, `auth_cmd.rs`, `agent_cmd.rs`,
  `models_cmd.rs`, `rpc.rs`, `cli_args.rs`: untouched.

### 2.3 Public surface of `yaca-bootstrap`

```rust
// crates/yaca-bootstrap/src/lib.rs
pub mod agent;
pub mod config;
pub mod formatter_config;
pub mod permission;
pub mod plugins;
pub mod runtime;
pub mod team;

pub use agent::{agent_with_model, discover_context_files, skill_dirs, today};
pub use config::{ModelEntry, ResolvedConfig};
pub use permission::{PermissionPolicy, spawn_auto_responder, path_in_workdir};
pub use runtime::{
    EngineRuntime, RuntimeConfig, build_engine, compaction_config, host_info,
    offline_router, open_store, resolve_runtime,
};
```

```rust
// crates/yaca-bootstrap/src/runtime.rs (key types)

/// Identical to today's `RuntimeConfig` in yaca-cli/src/main.rs:322.
pub struct RuntimeConfig {
    pub router: yaca_provider::ProviderRouter,
    pub model: String,
    pub models: Vec<crate::config::ModelEntry>,
    pub mcp: std::collections::BTreeMap<String, yaca_mcp::McpServerConfig>,
    pub plugins: Vec<yaca_plugin::config::PluginSpec>,
    pub default_agent: Option<String>,
}

/// Everything `hya` (or `yaca-cli serve`) needs to assemble an `AppState`,
/// PLUS the lifecycle guards (mcp_manager + plugin_host) the caller must keep
/// alive until shutdown.
pub struct EngineRuntime {
    pub engine: std::sync::Arc<yaca_core::SessionEngine>,
    pub agent: std::sync::Arc<yaca_core::AgentSpec>,
    pub asks: tokio::sync::mpsc::UnboundedReceiver<yaca_tool::AskRequest>,
    pub questions: tokio::sync::mpsc::UnboundedReceiver<yaca_tool::QuestionRequest>,
    pub mcp_manager: yaca_mcp::McpManager,
    pub plugin_host: std::sync::Arc<yaca_plugin::PluginHost>,
    pub default_agent: Option<String>,
    pub model: String,
    pub models: Vec<crate::config::ModelEntry>,
}

/// Top-level: resolve config + open store + build engine in one call.
/// `db = ""` means in-memory.
pub async fn build_engine(
    model_override: Option<String>,
    db: &str,
) -> anyhow::Result<EngineRuntime>;
```

### 2.4 `yaca-bootstrap/Cargo.toml`

```toml
[package]
name = "yaca-bootstrap"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
yaca-core = { workspace = true }
yaca-mcp = { workspace = true }
yaca-plugin = { workspace = true }
yaca-proto = { workspace = true }
yaca-provider = { workspace = true }
yaca-store = { workspace = true }
yaca-tool = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
serde = { workspace = true }
serde_norway = { workspace = true }
time = { workspace = true }
```

Notice: NO `axum`, NO `yaca-server`, NO `reqwest`, NO `clap`. Bootstrap is pure
engine construction.

### 2.5 Yaca-cli call-site changes

Two short diffs (a few lines each):

```rust
// crates/yaca-cli/src/main.rs — top of file
use yaca_bootstrap::{
    agent_with_model, build_engine, compaction_config, host_info, open_store,
    resolve_runtime, spawn_team_supervisor, EngineRuntime, RuntimeConfig,
};
use yaca_bootstrap::permission::{PermissionPolicy, spawn_auto_responder};

// delete: today, discover_context_files, skill_dirs, agent_with_model,
//         compaction_config, spawn_team_supervisor, build_session_engine,
//         host_info, headless_policy, offline_router, RuntimeConfig,
//         resolve_runtime, open_store
// delete modules: config, formatter_config, permission, plugins, skills
```

```rust
// crates/yaca-cli/src/serve.rs
use yaca_bootstrap::{agent_with_model, build_engine, host_info, open_store,
                     resolve_runtime, EngineRuntime};
use yaca_bootstrap::permission::{PermissionPolicy, spawn_auto_responder};
```

Replace the existing `let (engine, asks, questions, mcp_manager, plugin_host) =
build_session_engine(...).await;` (in `cmd_serve` and `cmd_tui_hya`) with:

```rust
let rt = build_engine(model_override, &db).await?;
let mut state = AppState::new(rt.engine, rt.agent)
    .with_question_requests(rt.questions)
    .with_mcp_manager(rt.mcp_manager)
    .with_workspace_adapters(rt.plugin_host.workspace_adapters())
    .with_default_agent(rt.default_agent.clone())
    .with_global_agents(true);
let _responder = if yolo {
    Some(spawn_auto_responder(rt.asks, PermissionPolicy::Yolo))
} else {
    state = state.with_permission_requests(rt.asks);
    None
};
// hold rt.plugin_host until end of serve loop (already done today via _plugin_host)
let _plugin_host = rt.plugin_host;
```

### 2.6 Cycle risks — none

The full dep DAG was verified above (§1). The only crate added to `hya-sdk`'s
deps (and only when `native-yaca` is enabled) is `yaca-server` +
`yaca-bootstrap`; neither depends on `hya-sdk`. `yaca-cli` already depends on
both `yaca-server` and (after this change) `yaca-bootstrap`. No cycles.

---

## 3. `YacaNativeTransport` design

File: `crates/hya-sdk/src/yaca_native.rs` (NEW, ~120 LOC).

### 3.1 Cargo wiring

`crates/hya-sdk/Cargo.toml`:

```toml
[features]
default = []                   # keep HTTP path on by default for landing
http = []                      # currently a no-op label; mark for future gating
native-yaca = ["dep:yaca-server", "dep:yaca-bootstrap", "dep:tower",
               "dep:http-body-util", "dep:axum"]

[dependencies]
# existing always-on:
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
async-trait = "0.1"
tokio = { version = "1", features = ["rt-multi-thread","macros","process","io-util","time","sync"] }
reqwest = { version = "0.12", default-features = false, features = ["json","stream","charset","http2"] }
eventsource-stream = "0.2"
futures-util = "0.3"
libc = "0.2"

# new, optional:
yaca-server = { workspace = true, optional = true }
yaca-bootstrap = { workspace = true, optional = true }
tower = { workspace = true, features = ["util"], optional = true }
http-body-util = { version = "0.1", optional = true }
axum = { workspace = true, optional = true }
```

`crates/hya/Cargo.toml`:

```toml
[dependencies]
hya-sdk = { path = "../hya-sdk", features = ["native-yaca"] }
hya-tui = { path = "../hya-tui" }
reqwest = { ... }                  # keep for now (HTTP path still works)
tokio = { ... }
yaca-bootstrap = { workspace = true }  # NEW — main.rs constructs EngineRuntime
yaca-server = { workspace = true }     # NEW — main.rs calls server::router(state)
```

### 3.2 The transport (exact code shape)

```rust
//! crates/hya-sdk/src/yaca_native.rs
//! In-process Transport that calls `yaca_server::router(state)` via `tower::oneshot`.
//! No TCP. No reqwest. The router is `Clone` (axum::Router wraps an Arc);
//! every request clones-and-oneshots, exactly like crates/yaca-server/tests/api.rs.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use futures_util::StreamExt;
use http_body_util::BodyExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tower::ServiceExt;

use crate::client::{ApiClient, Transport};
use crate::error::{Result, SdkError};
use crate::types::GlobalEvent;

const BASE_URL: &str = "http://yaca.internal";

pub struct YacaNativeTransport {
    router: axum::Router,
    directory: String,
}

impl YacaNativeTransport {
    pub fn new(router: axum::Router, directory: impl Into<String>) -> Self {
        Self { router, directory: directory.into() }
    }
}

#[async_trait]
impl Transport for YacaNativeTransport {
    fn base_url(&self) -> &str { BASE_URL }
    fn directory(&self) -> &str { &self.directory }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value> {
        let m = match method {
            "GET" => Method::GET, "POST" => Method::POST,
            "PATCH" => Method::PATCH, "DELETE" => Method::DELETE,
            other => return Err(SdkError::Http(format!("unsupported method {other}"))),
        };
        let mut builder = Request::builder()
            .method(m.clone())
            .uri(path)
            .header(crate::DIRECTORY_HEADER, &self.directory);
        let req = if let Some(value) = body {
            let bytes = serde_json::to_vec(value)?;
            builder = builder.header("content-type", "application/json");
            builder.body(Body::from(bytes))
        } else {
            builder.body(Body::empty())
        }
        .map_err(|e| SdkError::Http(e.to_string()))?;

        let resp = self.router.clone().oneshot(req).await
            .map_err(|e| SdkError::Http(e.to_string()))?;
        let status = resp.status();
        let bytes = resp.into_body().collect().await
            .map_err(|e| SdkError::Http(e.to_string()))?
            .to_bytes();

        if !status.is_success() {
            // Match HttpTransport's non-2xx behavior (client.rs:211): error_for_status.
            let body = String::from_utf8_lossy(&bytes);
            return Err(SdkError::Http(format!(
                "status {} for {method} {path}: {body}",
                status.as_u16()
            )));
        }
        if bytes.is_empty() {
            return Ok(Value::Null); // mirrors HttpTransport (client.rs:216).
        }
        serde_json::from_slice(&bytes).map_err(|e| SdkError::Http(e.to_string()))
    }
}

pub type YacaNativeClient = ApiClient<YacaNativeTransport>;
```

### 3.3 Decisions on knobs

| Knob | Decision | Why |
|---|---|---|
| `base_url` | constant `"http://yaca.internal"` | Same trick `NativeTransport` (native.rs:273) already uses. It's never network-resolved; it just keeps `Client::base_url()` non-empty for log/breadcrumb sites. |
| Directory header | injected on every call (line above `if let Some(value) = body`) | Identical contract to `HttpTransport::request` (client.rs:203). The `/global/event` projection (event.rs:60-68) reads it for location scoping. |
| `Router::clone()` | yes, per request | `axum::Router` is `Clone` (cheap Arc-wrap). Same pattern as 30+ tests, e.g. `api.rs:53,69,86`. `tower::ServiceExt::oneshot` consumes the service by value. |
| Non-2xx → `SdkError` | `SdkError::Http(format!("status {} ...", code))` | Identical to `HttpTransport::request` shape (client.rs:211-215). Tests in `crates/hya-sdk/tests/*` assert on `Http(_)`. |
| Empty body | `Ok(Value::Null)` | DELETE/PATCH return empty bodies; `client.rs:216` does the same. |
| Method validation | reject unsupported with `SdkError::Http("unsupported method …")` | Matches `HttpTransport` (client.rs:201). |
| `content-type` header on body | always `application/json` (added before serialization) | Matches `HttpTransport::request().json(body)`. Axum extractors will reject otherwise. |
| Backpressure on the in-process call | none needed | `oneshot` awaits the full response. The router runs to completion in the same task. |

### 3.4 Error mapping cheat sheet

| Source | `SdkError` variant |
|---|---|
| `Router` poll error (`Infallible` in practice) | `Http(e.to_string())` |
| `BodyExt::collect` IO error | `Http(e.to_string())` |
| Non-2xx | `Http(format!("status {N} for {method} {path}: {body}"))` |
| JSON decode of response | `Decode(serde_json::Error)` (auto via `From`) |
| Unsupported method | `Http("unsupported method {m}")` |

No new variants. No `Service`/`Native` variant. Mirrors existing HTTP behavior so
existing hya-tui error-handling paths stay valid.

---

## 4. Event bridge design

**Decision: Option 1 — oneshot `GET /global/event` and read the SSE body in-process.**

### 4.1 Why this option (and not bus.subscribe())

| | Option 1: oneshot `/global/event` | Option 2: `engine.bus().subscribe()` |
|---|---|---|
| Code added in hya-sdk | ~40 LOC | ~400 LOC (re-project envelope → GlobalEvent) |
| Couples hya to | the SSE wire shape (already a frozen contract) | yaca's internal `Envelope`/`Event` enum |
| Reuses the 796-LOC `opencode/event.rs` projection | **yes** | no (must reimplement) |
| Behavior drift risk vs. HTTP path | none — same producer | high — two projections must stay in sync |
| Matches existing hya `events.rs` consumer | drop-in | drop-in but the producer is duplicated |

Conservative pick is obvious.

### 4.2 The bridge (exact code shape)

```rust
//! still in crates/hya-sdk/src/yaca_native.rs
pub fn spawn_yaca_event_bridge(
    router: axum::Router,
    directory: String,
    tx: mpsc::UnboundedSender<GlobalEvent>,
) -> tokio::task::JoinHandle<()> {
    use eventsource_stream::Eventsource;

    tokio::spawn(async move {
        loop {
            let req = match Request::builder()
                .method(Method::GET)
                .uri("/global/event")
                .header(crate::DIRECTORY_HEADER, &directory)
                .body(Body::empty())
            {
                Ok(r) => r,
                Err(_) => return,
            };
            let Ok(resp) = router.clone().oneshot(req).await else {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            };
            // Reuse the same eventsource_stream parser that events.rs uses
            // for the HTTP path — keeps the parsing logic exactly one place.
            let mut stream = resp.into_body().into_data_stream().eventsource();
            while let Some(event) = stream.next().await {
                let Ok(event) = event else { break; };
                if event.data.is_empty() { continue; } // also covers "resync"
                if let Ok(global) = serde_json::from_str::<GlobalEvent>(&event.data) {
                    if tx.send(global).is_err() { return; }   // receiver dropped → quit
                }
            }
            // Stream ended (shouldn't happen with the heartbeat producer); reconnect.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    })
}
```

### 4.3 Backpressure, lag, resync, shutdown

- **Backpressure.** `mpsc::unbounded_channel`, matching the existing HTTP path
  (`hya/src/main.rs:32,143`). The TUI consumer is fast (just folds events into
  `AppState`); unbounded keeps the producer non-blocking.
- **Lag (`broadcast::Receiver::Lagged`).** Already handled inside the router at
  `event.rs:50,93,120` — converted to `SseEvent::default().event("resync")`,
  which carries empty data. Our `event.data.is_empty() { continue; }` clause
  drops it (matching `events.rs:38`). hya's existing reducer treats a `resync`
  as "trigger refetch" via the on-event handler in `hya-tui`; behavior is
  preserved unchanged.
- **Heartbeats.** The router emits `server.heartbeat` every ~10s
  (`event_heartbeat.rs`). The reducer in hya-sdk already drops these via
  `GlobalEvent::is_heartbeat()` (`types.rs:55`). Unchanged.
- **Shutdown.** The `tokio::task::JoinHandle` returned is stored on
  `Transport::Yaca`; `.shutdown()` aborts it (mirrors current
  `hya/src/main.rs:184`). Aborting the task drops the `Body`, which propagates
  cancellation through the router. The Tokio broadcast stream registered to the
  bus is dropped → `Sender` decrements its subscriber count → no leak.
- **Reconnect loop.** Same shape as `spawn_sse_task` in
  `hya/src/main.rs:263-285`: on disconnect, sleep 500ms, retry. In-process
  this almost never trips; if it does, the SSE is just re-attached to the bus.

---

## 5. `hya/src/main.rs` wiring + flags + lifecycle

### 5.1 New flag

Add to `struct Args` (`hya/src/main.rs:364`):

```rust
native: bool,        // --native: in-process yaca, no HTTP
```

Parse it in `Args::parse()` alongside `--http`:

```rust
"--native" => parsed.native = true,
```

Update `print_usage()`:

```
  --native           run yaca in-process (no HTTP, no spawn)
```

### 5.2 New `Transport` variant

```rust
enum Transport {
    Native(NativeBridge),                       // existing (Bun bridge)
    Http { server: ServerMode, sse: ..., keep_streaming: ... },  // existing
    Yaca {                                       // NEW
        plugin_host: Arc<yaca_plugin::PluginHost>,  // explicit lifecycle owner
        event_task: tokio::task::JoinHandle<()>,
    },
}
```

### 5.3 New connect branch (added INSIDE `Transport::connect`, ABOVE the existing branches)

```rust
async fn connect(
    args: &Args,
    directory: &str,
    tx: &mpsc::UnboundedSender<AppEvent>,
) -> Result<(Arc<dyn Client>, Transport), Box<dyn Error + Send + Sync>> {
    if args.native && args.server.is_none() {
        // Build engine in-process. Inherit `--yolo` here later if desired (out of scope).
        let rt = yaca_bootstrap::build_engine(None, "").await?;
        let plugin_host = rt.plugin_host.clone();
        let workspace_adapters = rt.plugin_host.workspace_adapters();
        let state = yaca_server::AppState::new(rt.engine, rt.agent)
            .with_question_requests(rt.questions)
            .with_mcp_manager(rt.mcp_manager)
            .with_workspace_adapters(workspace_adapters)
            .with_default_agent(rt.default_agent.clone())
            .with_global_agents(true)
            .with_permission_requests(rt.asks);
        let router = yaca_server::router(state);

        let (event_tx, event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
        let event_task = hya_sdk::yaca_native::spawn_yaca_event_bridge(
            router.clone(),
            directory.to_owned(),
            event_tx,
        );
        forward_events(event_rx, tx.clone());   // already exists at main.rs:193

        let client: Arc<dyn Client> = Arc::new(
            hya_sdk::ApiClient::with_transport(
                hya_sdk::yaca_native::YacaNativeTransport::new(router, directory),
            )
        );
        return Ok((client, Transport::Yaca { plugin_host, event_task }));
    }
    // …existing Native/Http branches…
}
```

### 5.4 Teardown

```rust
fn shutdown(self) {
    match self {
        Transport::Native(bridge) => drop(bridge),
        Transport::Http { server, sse, keep_streaming } => { /* unchanged */ }
        Transport::Yaca { plugin_host, event_task } => {
            event_task.abort();
            // McpManager was moved into AppState (held by the router, held by
            // YacaNativeTransport, held by the Arc<dyn Client>) — dropping the
            // last client reference drops the router → AppState → McpManager.
            // We explicitly drop the plugin_host Arc to mirror serve.rs's
            // `drop(plugin_host)` step (serve.rs:108).
            drop(plugin_host);
        }
    }
}
```

### 5.5 Resolution of the Arc<dyn Client>

The Arc returned to the TUI is the only strong reference to the
`YacaNativeTransport`, which holds the router, which holds the AppState. When
the TUI exits, `spawn_connect`'s join handle returns the `Transport::Yaca`
guard; `Transport::shutdown` aborts `event_task`. The Arc<dyn Client> goes out
of scope on TUI return → router drops → AppState drops → engine + mcp_manager
drop. The explicit `drop(plugin_host)` here matches the existing
`crates/yaca-cli/src/serve.rs:108` pattern that keeps the host alive until
graceful shutdown.

### 5.6 Flag interaction matrix

| flags | resulting branch |
|---|---|
| `--native` (no `--server`) | new Yaca branch |
| `--native --server <url>` | reject in `Args::parse` (`InvalidInput`) |
| `--http` / `--opencode` / `--server` / default (no `--native`) | UNCHANGED from today |
| `--yolo` (future) | inside Yaca branch, swap `with_permission_requests` for `spawn_auto_responder(asks, PermissionPolicy::Yolo)` — copy the 4-line pattern from `serve.rs:88-93`. Out of scope for this PR. |

---

## 6. Proving zero HTTP (accept gate)

Three layered checks. The integration test is the primary gate; strace is the
secondary acceptance proof; the manual tmux QA is the human verification.

### 6.1 Integration test (PRIMARY, hermetic, runs on every PR)

File: `crates/hya-sdk/tests/zero_http_yaca_native.rs` (NEW).

```rust
#![cfg(feature = "native-yaca")]

use http_body_util::BodyExt;
use hya_sdk::{ApiClient, Client, yaca_native::YacaNativeTransport};
use std::sync::Arc;
use tokio::sync::mpsc;
use yaca_bootstrap::build_engine;
use yaca_server::{AppState, router as build_router};

/// Returns the count of listening + non-loopback-connected IPv4/IPv6 TCP sockets
/// owned by THIS process (snapshot of /proc/self/net/tcp{,6}).
/// Mirrors crates/hya-sdk/src/server.rs:205 logic.
fn nontrivial_tcp_sockets() -> usize { /* … */ }

#[tokio::test]
async fn full_turn_no_tcp_sockets_created() {
    let before = nontrivial_tcp_sockets();

    let rt = build_engine(None, "").await.unwrap();
    let plugin_host = rt.plugin_host.clone();
    let state = AppState::new(rt.engine, rt.agent)
        .with_question_requests(rt.questions)
        .with_mcp_manager(rt.mcp_manager)
        .with_default_agent(rt.default_agent.clone())
        .with_permission_requests(rt.asks);
    let router = build_router(state);

    let client: Arc<dyn Client> = Arc::new(
        ApiClient::with_transport(YacaNativeTransport::new(router.clone(), "."))
    );

    // Drive a full turn through the public Client surface.
    let session = client.session_create().await.unwrap();
    let _ = client.session_prompt(&session.id, serde_json::json!({
        "text": "hi", "agent": "build"
    })).await.unwrap();
    // Hit a representative GET to exercise the SSE-bridge code path.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let bridge = hya_sdk::yaca_native::spawn_yaca_event_bridge(
        router.clone(), ".".into(), tx);
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        rx.recv(),
    ).await; // at least see one server.connected
    bridge.abort();
    drop(plugin_host);

    let after = nontrivial_tcp_sockets();
    assert_eq!(
        before, after,
        "no TCP sockets must be opened on the native path (before={before}, after={after})",
    );
}
```

The `nontrivial_tcp_sockets()` helper is lifted directly from
`crates/hya-sdk/src/server.rs:172-229` (existing). The test fails if any TCP
listener was bound or any non-loopback CONNECT was issued.

### 6.2 Strace gate (SECONDARY, manual + CI)

Add `crates/hya/tests/zero_http_strace.sh` (run from CI Linux runner only):

```bash
#!/usr/bin/env bash
set -euo pipefail
cd "$(mktemp -d)"
strace -f -qq -e trace=connect,bind,socket -o strace.out -- \
    "${HYA_BIN:-./hya}" --native --help || true
# Allow AF_UNIX (Bun/IPC may use them); reject any AF_INET/AF_INET6 syscalls.
if grep -E 'socket\([^,]*AF_INET' strace.out; then
    echo "FAIL: hya --native created an AF_INET socket"
    exit 1
fi
echo "OK: no AF_INET sockets in hya --native"
```

For a full-turn strace, the QA script in §7 also captures strace during a real
session.

### 6.3 `cargo tree` documentation (TERTIARY)

A `crates/yaca-bootstrap/tests/no_reqwest_in_native_path.rs` test (compile-only)
that does `let _: yaca_bootstrap::EngineRuntime;`. yaca-bootstrap has no
reqwest dep (verified in §2.4). If reqwest ever sneaks into the bootstrap deps,
`cargo tree -p yaca-bootstrap | grep reqwest` in CI catches it. Add this as a
1-line CI step.

---

## 7. Test strategy

### 7.1 Unit (hya-sdk)

`crates/hya-sdk/tests/yaca_native_transport.rs` (NEW; `#![cfg(feature="native-yaca")]`):

1. **`request_passes_directory_header`** — Build a minimal `axum::Router` that
   inspects headers; assert the `x-opencode-directory` header equals the
   `directory` arg. Mirrors `client.rs:203`.
2. **`request_get_decodes_json`** — Route returns `Json({"theme":"foo"})`;
   assert `request("GET", "/config", None)` deserializes round-trip.
3. **`request_post_serializes_body_with_content_type`** — Echo route asserts
   `content-type: application/json` and returns the body verbatim.
4. **`request_non_2xx_maps_to_sderror_http`** — Route returns
   `StatusCode::CONFLICT`; assert `Err(SdkError::Http(s))` with `"status 409"`
   in `s`.
5. **`request_empty_body_returns_null`** — DELETE route returns `204 No
   Content`; assert `Ok(Value::Null)` (matches `client.rs:216`).
6. **`request_unsupported_method_errors`** — Pass `"HEAD"`; assert
   `Err(SdkError::Http("unsupported method HEAD"))` (matches `client.rs:201`).

These mirror, one-to-one, the behavioral contracts already exercised by
`HttpTransport` tests at `crates/hya-sdk/src/client.rs:636-682` and ensure the
two transports are interchangeable from any consumer's point of view.

### 7.2 Integration (the accept gate, §6.1)

`crates/hya-sdk/tests/zero_http_yaca_native.rs` (verbatim §6.1).

### 7.3 Existing tests

The `crates/yaca-server/tests/api.rs` and 70+ opencode_*_api.rs tests already
exercise the router via `tower::oneshot`. They give us extremely high
confidence that the in-process router behaves identically to a TCP-served one.
No new server-side tests needed.

### 7.4 Manual tmux QA

Documented script (kept in
`.trellis/tasks/06-24-hya-yaca-native-sdk/qa/native-tmux.md`):

1. `cargo build -p hya --release` in this worktree.
2. `tmux new -d -s hya-native ./target/release/hya --native`.
3. Attach; type `hello`; observe the assistant reply (offline echo provider
   is fine for the smoke test).
4. In a sibling pane: `ss -tnp | grep $(pgrep -f hya)` returns empty.
5. `strace -f -p $(pgrep -f hya) -e trace=connect,bind 2>strace.out & sleep 5;
   kill %1`; assert no AF_INET entries (§6.2).
6. Exit hya cleanly (`Ctrl-C`); confirm `pgrep -f hya` returns empty within 1s
   (lifecycle).

---

## 8. Risks + mitigations

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| 1 | **`yaca-bootstrap` extraction misses a private dep** (e.g., `yaca-cli::tui::history` reaches into `config::ModelEntry`). Build breaks. | Medium | Low | The `ModelEntry`, `RuntimeConfig` types are already `pub` (or used as field types of `tui::RunOptions`). Run `cargo build --workspace` after each move file; the borrow checker forces every miss to surface. Atomic commit per file. |
| 2 | **Engine bootstrap is slow** (MCP `connect_all` can take ~30s). The TUI must remain responsive. | High | Medium | Already solved by hya's `PendingClient` pattern (`hya-sdk/src/pending.rs`). The native branch follows the same `spawn_connect → slot.set` shape (see `main.rs:39,91`). No new work needed. |
| 3 | **`Router::clone` is not free.** | Low | Low | Already validated: 70+ existing tests do this per request without measurable cost. The Router wraps an Arc internally. |
| 4 | **SSE bridge stops reading mid-frame on cancel**, leaving the broadcast `Sender` with one extra subscriber. | Low | Low | Sub drops when the Body stream drops. Tokio's broadcast already collects orphans. Verified by reading `tokio::sync::broadcast` semantics; no leak. |
| 5 | **`/global/event` projection diverges from hya's expected shape over time.** | Low | High | This is the SAME risk the HTTP path runs today — the producer is the same code. Native path takes no new risk; in fact native path closes the risk that hya's HTTP consumer ever sees a different projection than the bus produces. |
| 6 | **`with_permission_requests` consumed by AppState** but hya wants the channel for its own visibility. | Low | Low | hya uses the existing TUI permission prompt machinery via SSE events (`permission.created`, `permission.replied`). The channel inside AppState is only used by the legacy /sessions REST API; hya's opencode-routes path is the same as today. No change. |
| 7 | **`drop(plugin_host)` runs while still pluggable.** | Low | Medium | Ordered teardown: abort `event_task` FIRST (kills the SSE stream → router stops touching state), THEN drop the Arc<dyn Client> (this happens when `spawn_connect`'s task returns and the JoinHandle is dropped), THEN `drop(plugin_host)`. The existing `serve.rs:107-109` (`server.abort(); drop(plugin_host); result`) is our model. |
| 8 | **Cargo feature interaction with `cargo build --workspace`** — `hya` enables `hya-sdk/native-yaca`, dragging `yaca-server` into the build for everyone. | Medium | Low | Acceptable — `yaca-server` is already a workspace member and builds anyway. The feature exists for downstream crates, not the workspace build. |
| 9 | **Periodic merge with origin/main brings new routes/events that need projection updates.** | Medium | Medium | Zero new exposure on the native path — the projection is server-side, so any rebase automatically picks up new routes/events. The contract is wholly inside `yaca-server::opencode`. |
| 10 | **`anyhow::Result` from `build_engine` doesn't match `hya-sdk`'s `SdkError`.** | Low | Low | The native branch in `hya/src/main.rs` returns `Box<dyn Error + Send + Sync>` (existing signature, see `main.rs:141`). `anyhow::Error` boxes cleanly. No new error mapping needed. |

---

## 9. Implementation order — atomic units for parallel subagents

Each unit is sized to a single subagent. File boundaries are explicit so two
subagents on different units do NOT touch overlapping files.

### Sequential prerequisite (one subagent, must finish before others start)

**A1 — extract bootstrap.** Files touched: only inside
`crates/yaca-bootstrap/**` (new) and `crates/yaca-cli/{Cargo.toml,
src/main.rs, src/serve.rs}`.
- Create `crates/yaca-bootstrap/Cargo.toml` per §2.4.
- Move files per §2.1, verbatim (`git mv` where possible — keeps blame).
- Write `crates/yaca-bootstrap/src/lib.rs` per §2.3.
- In `crates/yaca-bootstrap/src/runtime.rs`, expose `EngineRuntime` and
  `build_engine(model_override, db)` that orchestrates `open_store + resolve_runtime
  + build_session_engine`.
- Update yaca-cli per §2.5; delete the moved modules from `yaca-cli/src/`.
- Add `yaca-bootstrap = { path = "crates/yaca-bootstrap" }` to the workspace
  `[workspace.dependencies]` table.
- Gate: `cargo build --workspace` green; `cargo test -p yaca-cli` green;
  `cargo test -p yaca-server` green.

### Parallel after A1 (run as four concurrent subagents)

**A2 — `YacaNativeTransport` only.** Files touched: only
`crates/hya-sdk/Cargo.toml` (add feature + optional deps) and the FIRST half of
`crates/hya-sdk/src/yaca_native.rs` (just the transport, NOT the event bridge).
Update `crates/hya-sdk/src/lib.rs` with `#[cfg(feature="native-yaca")] pub mod
yaca_native; #[cfg(feature="native-yaca")] pub use yaca_native::{YacaNativeTransport,
YacaNativeClient};`.
- Gate: `cargo build -p hya-sdk --features native-yaca` green.

**A3 — SSE event bridge.** Files touched: SECOND half of
`crates/hya-sdk/src/yaca_native.rs` only (`spawn_yaca_event_bridge` function).
Re-export in `lib.rs`. No conflict with A2 if they write disjoint sections of the
same file — coordinate by having A2 land the file with a `// EVENT BRIDGE
BELOW` placeholder, A3 fills below. Or simpler: serialize A3 just after A2.
- Gate: `cargo build -p hya-sdk --features native-yaca` green.

**A4 — unit tests for the transport.** Files touched:
`crates/hya-sdk/tests/yaca_native_transport.rs` ONLY (NEW file). No code
overlap with A2/A3.
- Gate: `cargo test -p hya-sdk --features native-yaca` green for the new tests.

**A5 — accept-gate integration test.** Files touched:
`crates/hya-sdk/tests/zero_http_yaca_native.rs` ONLY (NEW file). Uses the
public surface introduced by A2/A3 — so A5 must merge AFTER A2 and A3.
- Gate: `cargo test -p hya-sdk --features native-yaca --test
  zero_http_yaca_native` green.

### Sequential tail (one subagent each, in this order)

**A6 — hya/main.rs wiring.** Files touched: `crates/hya/Cargo.toml` (add
`yaca-bootstrap`, `yaca-server`, `hya-sdk` feature), `crates/hya/src/main.rs`
(add `--native` flag, `Transport::Yaca` variant, branch in
`Transport::connect`, branch in `Transport::shutdown`). Depends on A2 + A3.
- Gate: `cargo build -p hya` green; `cargo run -p hya -- --help` shows
  `--native`; `cargo run -p hya -- --native` opens the TUI.

**A7 — proof scripts + docs.** Files touched:
- `crates/hya/tests/zero_http_strace.sh` (NEW, executable).
- `.trellis/tasks/06-24-hya-yaca-native-sdk/qa/native-tmux.md` (NEW).
- README note under `crates/hya-sdk/README.md` documenting the
  `native-yaca` feature.
- Gate: `bash crates/hya/tests/zero_http_strace.sh` exits 0 on the Linux CI
  runner.

### Visualization

```
            A1 (bootstrap extraction)
             |
   ┌─────────┼─────────┬─────────┐
   v         v         v         v
  A2        A3        A4         (waits for A2+A3)
(transport) (bridge) (unit)
   |         |         |          A5 (integration test)
   └────┬────┴─────────┴─────────────┐
        v                            v
       A6 (hya wiring)               (already merged)
        |
        v
       A7 (proof + docs)
```

Total wall-time estimate with the parallel fan: **Medium (1–2d)**.

---

## 10. Effort estimate

- A1: **Short (1–4h).** Mostly file moves; thin glue.
- A2: **Quick (<1h).** ~80 LOC.
- A3: **Quick (<1h).** ~40 LOC.
- A4: **Short (1–4h).** Six small unit tests.
- A5: **Short (1–4h).** One test + `/proc/net/tcp` helper.
- A6: **Short (1–4h).** ~50 LOC of binary glue.
- A7: **Quick (<1h).** Shell script + doc.

**Whole change: Medium (1–2d) end-to-end**, ~Short with parallelism.

---

## 11. Watch out for

- **The `directory` argument** must be the canonical, absolute path
  (`std::env::current_dir()?.display().to_string()` as `main.rs:31` already
  does). The `/global/event` `subscribe_api` route (`event.rs:66`) reads it for
  location scoping; a mismatched directory silently filters out events.
- **`with_permission_requests` vs `spawn_auto_responder`** — the AppState
  builder consumes the `asks` channel by `with_permission_requests`. If hya
  ever adds `--yolo`, the channel must instead be passed to
  `spawn_auto_responder`. Do NOT do both — the second consumer would block on
  an empty channel.
- **`yaca-bootstrap` MUST NOT depend on `axum` or `yaca-server`.** Bootstrap is
  pure engine construction. If a future change needs server-aware bootstrap, it
  belongs in a different crate (or in `yaca-server::bootstrap` if the policy
  flips).

---

## 12. Optional future considerations (out of scope, ≤2 items)

1. **Feature-gate the HTTP path** (`HttpTransport`, `events.rs`, `server.rs`)
   behind `feature = "http"`. This makes `cargo build -p hya
   --no-default-features --features hya-sdk/native-yaca` produce a binary with
   zero reqwest. Cleanest possible proof, but touches `events.rs`, `server.rs`,
   `client.rs` with `#[cfg]` and breaks `--server`/`--http`/`--opencode` until
   you flip the feature. Land in a follow-up PR after the native path is
   default. **Short (1–4h).**
2. **Flip `--native` from opt-in to default.** Replace the current default
   (spawn `yaca serve` over HTTP) with the in-process Yaca path; relegate the
   spawn path to `--legacy-http` or remove it entirely. **Quick (<1h).**
