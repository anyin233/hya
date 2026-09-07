# Plan C — Bottom-Up / Build-Graph / Risk Framing

**Framing**: Start from Cargo's dependency DAG. Every decision is anchored to a verified
edge (existing or new), an API confirmed to exist in the workspace, and a fallback for
the case where the verified API misbehaves at runtime. Implementation is decomposed into
the smallest compilable units a parallel-subagent fan-out can consume.

All citations are file paths and line numbers in
`/chivier-disk/yanweiye/Projects/yaca-hya-native/`.

---

## 0. Build-graph snapshot (verified)

Current relevant crates and direct deps (from Cargo.toml inspection):

```
hya-sdk             → serde, serde_json, thiserror=2, async-trait, tokio, reqwest,
                      eventsource-stream, futures-util, libc          (crates/hya-sdk/Cargo.toml:8-15)
hya                 → hya-sdk, hya-tui, reqwest, tokio                (crates/hya/Cargo.toml:12-16)
yaca-server         → yaca-core, yaca-mcp, yaca-proto, yaca-store,
                      yaca-tool, axum, base64, tokio, tokio-stream,
                      tokio-tungstenite, tokio-util, futures, regex,
                      serde*, tower-http, tracing, uuid               (crates/yaca-server/Cargo.toml:11-31)
                      dev-deps: tower, http-body-util=0.1, …           (lines 33-40)
yaca-cli            → yaca-{core,provider,tool,store,server,proto,
                      tui,mcp,plugin}, tokio, axum, clap, anyhow, …   (crates/yaca-cli/Cargo.toml:15-37)
```

Workspace deps that matter (root `Cargo.toml`):
- `tower = { version = "0.5", features = ["util"] }`  (line 34) — `ServiceExt::oneshot` always on.
- `axum = { version = "0.7", features = ["macros", "ws"] }` (line 33).
- `tower-http = "0.6"` (line 35).

No yaca-* crate depends on any hya-* crate today (grep confirms). **No cycle exists; we
are adding pure forward edges**.

### Proposed new edge set

```
hya-yaca   (NEW)   → hya-sdk, yaca-server, yaca-app (NEW), axum, tower, http-body-util,
                    eventsource-stream, futures-util, tokio, async-trait, serde_json,
                    bytes, thiserror
yaca-app   (NEW)   → yaca-{core,provider,tool,store,server,proto,mcp,plugin},
                    serde*, serde_norway, anyhow, tokio, tokio-util, time, uuid
hya                → + hya-yaca, + yaca-app   (in addition to existing)
yaca-cli           → + yaca-app               (and remove the moved local modules)
```

Cycle check (mental walk):
- `hya → hya-yaca → yaca-server → {yaca-core, yaca-mcp, yaca-proto, yaca-store, yaca-tool}`. None of those depend on hya-anything.
- `hya → hya-yaca → yaca-app → {yaca-server, yaca-provider, yaca-mcp, yaca-plugin, yaca-store, yaca-tool, yaca-core, yaca-proto}`. Pure DAG.
- `yaca-cli → yaca-app` adds a sibling-of-server edge; yaca-app does NOT depend on yaca-cli. Clean.
- Verdict: **no cycle, no feature-gate juggling required**.

### Crate-layout decision

**Option A — new crate `hya-yaca`** (chosen). **Option B — feature-gated `native-yaca`
inside hya-sdk** rejected because:
1. Adds optional deps on every `cargo metadata` resolve even when off → slower lockfile churn.
2. Couples the "pure client" SDK to yaca backend crates conceptually, harming the
   `HttpClient` and existing `NativeClient` (Bun bridge) story (`crates/hya-sdk/src/native.rs:1`).
3. Forward-compat: a future `hya-compat-native` mirrors the same shape symmetrically.

### Bootstrap-extraction decision

**New library crate `yaca-app`** (chosen). Alternative — putting bootstrap in
`yaca-server` — rejected because yaca-server currently has no `yaca-provider`,
`yaca-mcp`, or `yaca-plugin` deps and pulling them in would force every transitive
consumer (including pure HTTP clients) to compile providers + MCP. The split is the
existing dep boundary; we respect it.

---

## 1. Bootstrap extraction — exact steps

### What moves into `yaca-app`

From `crates/yaca-cli/src/main.rs`:
- `today()`            (lines 52-60)
- `discover_context_files()` (lines 62-85)
- `skill_dirs()`       (lines 87-93)
- `agent_with_model()` (lines 95-117)
- `compaction_config()`(lines 119-133)
- `spawn_team_supervisor()` (lines 135-238)
- `build_session_engine()` (lines 240-295)
- `host_info()`        (lines 297-302)
- `headless_policy()`  (lines 304-312)
- `offline_router()`   (lines 314-320)
- `RuntimeConfig` + `resolve_runtime()` (lines 322-382)
- `open_store()`       (lines 384-394)

Whole files that move:
- `crates/yaca-cli/src/config.rs`           → `crates/yaca-app/src/config.rs`
- `crates/yaca-cli/src/formatter_config.rs` → `crates/yaca-app/src/formatter_config.rs`
- `crates/yaca-cli/src/plugins.rs`          → `crates/yaca-app/src/plugins.rs`
- `crates/yaca-cli/src/skills.rs`           → `crates/yaca-app/src/skills.rs`
- `crates/yaca-cli/src/permission.rs`       → `crates/yaca-app/src/permission.rs`
  (PermissionPolicy enum + `spawn_auto_responder`)

### What stays in yaca-cli

- `cli_args.rs`, `auth*.rs`, `agent_cmd.rs`, `models_cmd.rs`, `rpc.rs`, `tui.rs`
- The thin `cmd_*` functions in `main.rs`, now calling `yaca_app::YacaRuntime::start(...)`.
- `serve::cmd_serve` and `serve::cmd_tui_hya`: rewritten to use `YacaRuntime` (the
  HTTP-spawn behaviour they implement today).

### New `yaca-app` public API

```rust
// crates/yaca-app/src/lib.rs
pub mod config;
pub mod formatter_config;
pub mod plugins;
pub mod skills;
pub mod permission;
mod runtime;

pub use runtime::{YacaRuntime, RuntimeOptions};

// crates/yaca-app/src/runtime.rs
pub struct RuntimeOptions {
    pub model: Option<String>,
    pub db: String,        // "" = in-memory
    pub yolo: bool,
    pub default_agent: Option<String>,
    pub include_global_agents: bool,
}

pub struct YacaRuntime {
    router: axum::Router,
    engine: std::sync::Arc<yaca_core::SessionEngine>,
    // Order of fields = drop order. Keep router/engine first, plugin_host LAST so it
    // outlives every Arc<SessionEngine> reference.
    _permission_responder: Option<tokio::task::JoinHandle<()>>,
    _mcp_manager: yaca_mcp::McpManager,
    _plugin_host: std::sync::Arc<yaca_plugin::PluginHost>,
}

impl YacaRuntime {
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self> { /* ... */ }
    pub fn router(&self) -> &axum::Router { &self.router }
    pub fn engine(&self) -> std::sync::Arc<yaca_core::SessionEngine> { self.engine.clone() }
}
```

`YacaRuntime::start` literally inlines the body of `cmd_tui_hya`
(`crates/yaca-cli/src/serve.rs:53-110`) minus the `TcpListener::bind` and the
`launch_hya` call, returning the assembled `AppState`-built `Router` instead. The
permission responder (yolo path) is stored on the runtime.

### Cargo edges for yaca-app

```toml
# crates/yaca-app/Cargo.toml
[dependencies]
yaca-core = { workspace = true }
yaca-provider = { workspace = true }
yaca-tool = { workspace = true }
yaca-store = { workspace = true }
yaca-server = { workspace = true }
yaca-proto = { workspace = true }
yaca-mcp = { workspace = true }
yaca-plugin = { workspace = true }
axum = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
serde_norway = { workspace = true }
anyhow = { workspace = true }
thiserror = { workspace = true }
time = { workspace = true }
uuid = { workspace = true }
```

(Plus `secrecy` etc. as `config.rs`/`auth.rs` currently use.)

Register in workspace: edit root `Cargo.toml` (`members = ["crates/*", ...]` already
covers it — line 5).

### Compilation gates within Phase 1

After each move, `cargo check -p yaca-cli` must still pass. Re-export from
`yaca-cli/src/lib.rs` (or rebind imports in `main.rs`) so the existing call sites
keep working with no behaviour change.

---

## 2. `YacaNativeTransport` design (API-verified)

### Mechanics

```rust
// crates/hya-yaca/src/transport.rs
use async_trait::async_trait;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;        // .collect()
use serde_json::Value;
use tower::ServiceExt;              // .oneshot() — requires `tower` "util" feature.

use hya_sdk::{DIRECTORY_HEADER, SdkError, Transport};

pub struct YacaNativeTransport {
    router: axum::Router,
    directory: String,
    base_url: String,                // "yaca-native://in-process" — synthetic, never dialled.
}

impl YacaNativeTransport {
    pub fn new(router: axum::Router, directory: impl Into<String>) -> Self {
        Self {
            router,
            directory: directory.into(),
            base_url: "yaca-native://in-process".to_owned(),
        }
    }
}

#[async_trait]
impl Transport for YacaNativeTransport {
    fn base_url(&self) -> &str { &self.base_url }
    fn directory(&self) -> &str { &self.directory }

    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> hya_sdk::error::Result<Value> {
        let body_bytes = match body {
            Some(v) => Body::from(serde_json::to_vec(v)?),
            None    => Body::empty(),
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(DIRECTORY_HEADER, &self.directory);
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        let req = builder
            .body(body_bytes)
            .map_err(|e| SdkError::Http(e.to_string()))?;

        // `Router` implements `tower::Service<Request<Body>, Response=Response, Error=Infallible>`
        // after `with_state(...)`, so oneshot is infallible at the transport layer.
        let resp = self.router.clone().oneshot(req)
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?;

        let status = resp.status();
        let bytes = resp.into_body().collect()
            .await
            .map_err(|e| SdkError::Http(e.to_string()))?
            .to_bytes();

        if !status.is_success() {
            // Mirror HttpTransport's error mapping (crates/hya-sdk/src/client.rs:209-216).
            return Err(SdkError::Http(format!(
                "status {} for {method} {path}", status.as_u16()
            )));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| SdkError::Http(e.to_string()))
    }
}
```

### API-availability verification

- `tower::ServiceExt::oneshot` — workspace `tower = "0.5"` + `features = ["util"]`
  (root `Cargo.toml:34`). `Router` after `with_state` implements `Service` (axum 0.7
  guarantee). **Already exercised in `crates/yaca-server/Cargo.toml:36-38` dev-deps**.
- `http_body_util::BodyExt::collect` — present in yaca-server dev-deps (line 38). We
  promote it to a workspace dep so hya-yaca pulls the same `0.1` line.
- `axum::http::Request::builder()` — standard. `axum::body::Body::empty()` / `Body::from(...)`
  — standard for axum 0.7.
- `hya_sdk::DIRECTORY_HEADER` — currently `pub(crate)` (`crates/hya-sdk/src/lib.rs:6`).
  **Required change: promote to `pub`** (one-line, no behaviour delta).

### Why the directory header path is identical to HTTP

`HttpTransport::request` (`crates/hya-sdk/src/client.rs:194-220`) sets
`crate::DIRECTORY_HEADER` for every request; `NativeTransport::request`
(`crates/hya-sdk/src/native.rs:280-333`) injects the same name into the bridge headers.
We do the exact same thing — the backend (`yaca-server`) reads it via the
`location::LocationRef::from_request(&query, &headers)` helper
(`crates/yaca-server/src/compat/event.rs:64`).

### Status / error mapping

Mirror `HttpTransport` semantics (`crates/hya-sdk/src/client.rs:207-219`):
- non-2xx → `SdkError::Http("status {} for {method} {path}")`
- empty body → `Ok(Value::Null)` (covers DELETE/PATCH)
- JSON parse error → `SdkError::Http(...)`

---

## 3. Event-bridge design

### Primary plan — SSE over `oneshot`

Reuses the entire 796-LOC projection in `crates/yaca-server/src/compat/event.rs`,
including `subscribe_global` (line 102) and its connected/heartbeat framing, the same
"resync" semantics on broadcast lag (line 120), and the **same `GlobalEvent` decoder**
hya-sdk already ships (`crates/hya-sdk/src/events.rs:42`). Zero new code in any
projection or parser.

```rust
// crates/hya-yaca/src/events.rs
use axum::body::Body;
use axum::http::Request;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use http_body_util::BodyExt;
use tower::ServiceExt;

use hya_sdk::{DIRECTORY_HEADER, GlobalEvent};

pub fn spawn_event_bridge(
    router: axum::Router,
    directory: String,
    tx: tokio::sync::mpsc::UnboundedSender<GlobalEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Reconnect-on-drop loop, mirroring crates/hya/src/main.rs:270-285.
        loop {
            let Ok(req) = Request::builder()
                .method("GET")
                .uri("/global/event")
                .header(DIRECTORY_HEADER, &directory)
                .body(Body::empty())
            else { break };

            let Ok(resp) = router.clone().oneshot(req).await else { break };
            let body = resp.into_body();
            // BodyExt::into_data_stream gives Stream<Result<Bytes, Error>> — what
            // eventsource-stream wants. (http-body-util 0.1.)
            let mut stream = body.into_data_stream().eventsource();

            while let Some(Ok(event)) = stream.next().await {
                if event.data.is_empty() { continue; }
                let Ok(global) = serde_json::from_str::<GlobalEvent>(&event.data) else { continue; };
                if tx.send(global).is_err() { return; }
            }
            // Body ended unexpectedly; small backoff and try again. The streaming
            // handler in event.rs:102 returns an effectively-infinite Sse body
            // (`stream::select(live, heartbeat)`), so a clean end only happens on
            // shutdown or our channel close — in which case the `return` above fires.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
}
```

### Why `oneshot` works on an infinite SSE response

`Router::oneshot(req)` resolves once the handler returns the `Response<Body>` object.
`subscribe_global` (`crates/yaca-server/src/compat/event.rs:102-128`) builds the Sse
synchronously and returns immediately — the body is the streaming part. We get the
`Response` back, then drain the body lazily; the heartbeat task (line 126
`event_heartbeat::stream(global_heartbeat_event)`) ticks for as long as the body is
being polled.

### Backpressure / shutdown / lag

- **Backpressure**: `tx` is an unbounded sender (matches the existing hya channel —
  `crates/hya/src/main.rs:32`). If the consumer falls behind we accumulate memory; same
  as today.
- **Shutdown**: parent aborts the JoinHandle on TUI quit. Drop closes the Body, the
  router cancels the broadcast `Receiver`.
- **Lag / resync**: handled inside `subscribe_global` (line 120) — a lag event becomes
  `event(\"resync\")`, which hya-sdk's existing code path ignores cleanly (`events.rs:42`
  filters `serde_json::from_str::<GlobalEvent>(...)`, unmatched frames are skipped, **and
  the receiver was just re-subscribed by `subscribe_global` so we don't miss anything
  newly published** — same guarantee as HTTP).

### Fallback — direct bus subscribe

If the SSE-via-oneshot path proves flaky in CI (most likely failure mode: future axum
0.8 changes Body model or eventsource-stream chokes on a partial frame at shutdown),
drop to:

```rust
// In yaca-server (single new public symbol):
pub async fn project_envelope_to_global_event(
    state: &AppState,
    envelope: yaca_proto::Envelope,
) -> serde_json::Value { /* call the existing envelope_payload */ }
```

Plus a `pub(crate) ServerState::from_app(app)` accessor (or just take `&AppState`
and rebuild a `ServerState` for projection — cheap, no I/O).

Then `spawn_event_bridge_via_bus(engine.bus().subscribe(), state, tx)` calls the
projection per envelope, wraps in `{"payload": ...}`, and forwards. We pay one
yaca-server API expansion (one `pub` symbol) for a fully decoupled path. Both
paths produce the **same wire shape** (the projected GlobalEvent), so hya doesn't
notice the swap.

---

## 4. `hya/main.rs` wiring + flags + teardown

### Cargo edges for `hya`

Add to `crates/hya/Cargo.toml`:
```toml
hya-yaca = { path = "../hya-yaca" }
yaca-app = { workspace = true }
```

`reqwest` and the `hya-sdk` HTTP+SSE path stay — the `--http` and `--server <url>`
flags continue to work via them.

### New transport variant

```rust
// Add to crates/hya/src/main.rs Transport enum (line 122-129):
enum Transport {
    Yaca { runtime: std::sync::Arc<yaca_app::YacaRuntime>,
           bridge: tokio::task::JoinHandle<()> },
    Native(NativeBridge),                      // existing compat Bun bridge
    Http { server: ServerMode, sse: tokio::task::JoinHandle<()>,
           keep_streaming: std::sync::Arc<AtomicBool> },
}
```

### Connect logic

In `Transport::connect` (currently `crates/hya/src/main.rs:137-173`), branch first on
the *new* default — native yaca:

```rust
async fn connect(args: &Args, directory: &str, tx: &mpsc::UnboundedSender<AppEvent>)
    -> Result<(Arc<dyn Client>, Transport), Box<dyn Error + Send + Sync>>
{
    if args.server.is_none() && !args.http && !args.opencode {
        let runtime = std::sync::Arc::new(
            yaca_app::YacaRuntime::start(yaca_app::RuntimeOptions {
                model: None, db: String::new(), yolo: false,
                default_agent: None, include_global_agents: true,
            }).await?
        );
        let transport = hya_yaca::YacaNativeTransport::new(
            runtime.router().clone(), directory);
        let client: Arc<dyn Client> = Arc::new(
            hya_sdk::ApiClient::with_transport(transport));
        let (event_tx, event_rx) = mpsc::unbounded_channel::<GlobalEvent>();
        forward_events(event_rx, tx.clone());           // existing fn line 193
        let bridge = hya_yaca::spawn_event_bridge(
            runtime.router().clone(), directory.to_owned(), event_tx);
        return Ok((client, Transport::Yaca { runtime, bridge }));
    }
    // existing compat-native, http, and --server branches unchanged
    // …
}
```

### Flags (post-change)

- *no flags*  → **native yaca in-process** (NEW DEFAULT)
- `--http`    → spawn `yaca serve` as subprocess and connect over HTTP/SSE (the
                current default behaviour, preserved)
- `--compat`→ compat native Bun bridge (unchanged)
- `--server`  → attach to a running compat-compatible server (unchanged)
- `--yaca-bin`→ only consumed by the `--http` path (unchanged)

### Teardown

```rust
fn shutdown(self) {
    match self {
        Transport::Yaca { runtime, bridge } => {
            bridge.abort();
            // Dropping the Arc<YacaRuntime> drops engine → mcp_manager → plugin_host
            // in declared field order (Rust drops fields top-down). PluginHost holds
            // child processes; it is the LAST drop so any tool calls still in flight
            // when we tear down can release cleanly.
            drop(runtime);
        }
        Transport::Native(b) => drop(b),
        Transport::Http { server, sse, keep_streaming } => {
            keep_streaming.store(false, Ordering::SeqCst);
            sse.abort();
            drop(server);
        }
    }
}
```

The connect-task abort flow (`crates/hya/src/main.rs:54-58`) keeps working — the
`Option<Transport>` returned via JoinHandle propagates the YacaRuntime guard.

---

## 5. How to PROVE zero HTTP — accept gate

### Layered evidence (any one is sufficient; we ship all three)

1. **Runtime socket trace (CI test)**:
   `tests/zero_http_check.rs` spawns the `hya` binary under
   `strace -f -e trace=socket,bind,connect,listen,accept -ofifo` and grep:
   - Zero `listen(` calls.
   - Zero `connect(...)` to any `127.0.0.1`/`::1` port.
   - `connect(...)` to public Anthropic/OpenAI IPs is allowed (outbound provider).
   Linux-only; gated `#[cfg(target_os = "linux")]`.

2. **`/proc/self/net/tcp` check (in-process test)**:
   `crates/hya/tests/native_round_trip.rs` boots `YacaRuntime` in-process, completes a
   full turn against the offline `DevProvider` (`yaca-provider` already has one —
   referenced at `crates/yaca-cli/src/main.rs:315`), then reads `/proc/self/net/tcp`
   and asserts no row with `st = 0x0A` (LISTEN) on a port owned by us.

3. **Static-dep audit** (informational only — hyper is unavoidable as a *library*):
   `cargo tree -p hya --no-default-features --duplicates` — record that we still pull
   hyper transitively via axum, but document that the accept-gate is a **runtime**
   property, not a "no hyper code present" property.

### Cheapest CI signal

The proc-tcp test (#2) catches the regression class we care about (a stray
`TcpListener::bind` somewhere) without needing strace privileges. It's the gate. The
strace test (#1) is belt-and-suspenders.

---

## 6. Test strategy

### Unit (in `hya-yaca` crate)

- `transport_get_config_returns_value` — boot YacaRuntime, `request("GET","/config",None)`,
  assert object.
- `transport_post_session_create_returns_session` — POST /session, assert returned
  Session shape.
- `transport_empty_body_returns_null` — `request("DELETE","/session/{fresh_id}",None)`.
- `transport_non_2xx_is_typed_error` — request a route that returns 4xx, assert
  `SdkError::Http`.
- `transport_directory_header_propagated` — pick an compat endpoint that uses
  `LocationRef::from_request` and verify the response reflects the directory we set.
- `event_bridge_emits_connected_then_event` — bridge subscribe, immediately publish a
  `MessageStarted` envelope via `engine.bus().publish(...)`, assert the
  `server.connected` frame arrives first and the projected event next.
- `event_bridge_propagates_resync_on_lag` — saturate the broadcast channel beyond its
  1024-cap (`crates/yaca-core/src/bus.rs:28`), assert the bridge keeps running and
  produces the next valid GlobalEvent after lag.

### Integration (in `hya` crate)

- `tests/native_round_trip.rs` — boot YacaRuntime (offline provider), wire
  YacaNativeTransport + bridge, call `client.config_get`, `session_create`,
  `session_prompt("hi")`, collect events until `MessageFinished`, assert ordered.
- `tests/zero_http.rs` — see §5.

### Manual tmux QA

- `cargo run -p hya` (no flags) → expect TUI, `/help`, fire a prompt, see streaming text
  with the offline provider, exit cleanly with Ctrl-C, no stale port held (`ss -tlnp |
  grep $$` shows nothing).

### CI gating

Hook the proc-tcp test (`tests/native_round_trip.rs::no_listening_sockets`) into the
existing `cargo test --workspace` invocation. No new infrastructure required.

---

## 7. Ranked risks + mitigations + fallbacks

| # | Risk | Likelihood × Impact | Mitigation | Fallback |
|---|------|---------------------|-----------|----------|
| 1 | Bootstrap extraction silently changes runtime behaviour (e.g. drop order of plugin_host vs engine) | High × High | Move in atomic per-file commits; `cargo test -p yaca-cli` after every move; declare YacaRuntime fields in explicit drop order with comment | Roll back the offending move; keep bootstrap inline in yaca-cli and inline-export the public surface via `pub use` only |
| 2 | `tower::ServiceExt::oneshot` unavailable to a downstream because someone disables default features | Low × High | hya-yaca Cargo.toml uses `tower = { workspace = true }` (workspace already includes `features = ["util"]`) | Pin `tower = { version = "0.5", features = ["util"] }` directly on hya-yaca, not via workspace |
| 3 | SSE-over-oneshot streams nothing because heartbeat task can't run inside the oneshot context | Medium × Medium | Stress-test in `event_bridge_propagates_resync_on_lag`; hold the bridge open for >30s and assert ≥1 heartbeat frame | Switch to the **§3 fallback** — direct `engine.bus().subscribe()` + new `pub fn project_envelope_to_global_event` in yaca-server (single public symbol added) |
| 4 | `envelope_payload` projection in `event.rs:171` reads session state via `load_session` (an async store call); under in-process oneshot the load might race the in-flight `run_turn` | Medium × Medium | Same race exists on HTTP today (same handler) — verify by replaying the existing HTTP test path against the bridge in `event_bridge_emits_connected_then_event` | Add a small spin loop in the projection (already tolerant; missing parts → `fallback_payload`, line 717) |
| 5 | `http-body-util` not promoted to workspace deps → version drift between yaca-server dev-deps and hya-yaca prod-deps | Low × Medium | Add `http-body-util = "0.1"` to workspace deps; both crates use `{ workspace = true }` | Pin identical literal `"0.1"` in both Cargo.tomls |
| 6 | `DIRECTORY_HEADER` is `pub(crate)`; hya-yaca cannot reference it | High × Low | Change to `pub` (`crates/hya-sdk/src/lib.rs:6`); no behaviour delta | Inline-redefine `"x-opencode-directory"` in hya-yaca with `static_assertions::const_assert_eq` against the SDK side |
| 7 | `--http` flag's new meaning surprises existing users invoking `yaca serve` then `hya` | Low × Low | Update `print_usage()` (`crates/hya/src/main.rs:412`); document in CHANGELOG | Add a `--native` flag that is the default when not present (purely cosmetic) |
| 8 | Workspace-resolver `resolver = "3"` (`Cargo.toml:4`) edge cases with new feature unions across hya-yaca's chain | Low × Medium | `cargo build --workspace --all-targets` after the new crate appears; spot-check `cargo tree -d` for duplicates | Add explicit features to silence resolver-3 unification surprises |
| 9 | Hyper compiled into the hya binary makes the "no HTTP" claim look weak | High × Low | Document: accept-gate is **runtime** (no socket bound, no connect to loopback) | None needed; the static dep is unavoidable while we depend on axum |
| 10 | Event channel back-pressure regresses now that production goes via in-process | Low × Medium | Unbounded channel maintained; bus capacity stays at 1024 (yaca-core/src/bus.rs:28) | Bound the channel + drop-oldest policy if profiling shows accumulation |
| 11 | Aborting `bridge` JoinHandle leaves a partially-parsed SSE frame in `eventsource-stream` | Low × Low | The handle is dropped before the Body; cancellation drops state | n/a |
| 12 | `yaca-app::YacaRuntime::start` panics or hangs (slow MCP connect) during TUI startup | Medium × Medium | `YacaRuntime::start` is awaited inside the existing `spawn_connect` task (`crates/hya/src/main.rs:67-119`), so the TUI renders the "Starting backend…" toast meanwhile (line 82) | Add a startup timeout option to `RuntimeOptions` |

---

## 8. Topologically-sorted atomic implementation units

`[||]` marks units that can run in parallel within the same phase. Each unit fits a
single compiled-and-tested subagent task.

### Phase 0 — workspace prep
- **0a** [||]  Add `http-body-util = "0.1"` to root `Cargo.toml` workspace.dependencies.
- **0b** [||]  Promote `crates/hya-sdk/src/lib.rs:6` `DIRECTORY_HEADER` from `pub(crate)` to `pub`.
- **0c** [||]  Create empty `crates/yaca-app/{Cargo.toml,src/lib.rs}` skeleton (lib only,
              compiles to nothing yet). Workspace already globs `crates/*` so no root edit.
- **0d** [||]  Create empty `crates/hya-yaca/{Cargo.toml,src/lib.rs}` skeleton.

### Phase 1 — bootstrap extraction (depends on 0a–0d)
- **1a**       Fill in `crates/yaca-app/Cargo.toml` deps (§1 list). [serial — defines all deps]
- **1b** [||]  Move `crates/yaca-cli/src/skills.rs` to `crates/yaca-app/src/skills.rs`; mark
              `pub fn discover_skills`, `pub fn skills_section`; update yaca-cli `mod skills;`
              to `use yaca_app::skills;`.
- **1c** [||]  Move `crates/yaca-cli/src/config.rs` to `crates/yaca-app/src/config.rs`;
              mark public surface.
- **1d** [||]  Move `crates/yaca-cli/src/formatter_config.rs` to
              `crates/yaca-app/src/formatter_config.rs`.
- **1e** [||]  Move `crates/yaca-cli/src/plugins.rs` to `crates/yaca-app/src/plugins.rs`.
- **1f** [||]  Move `crates/yaca-cli/src/permission.rs` to
              `crates/yaca-app/src/permission.rs`; mark `PermissionPolicy` + `spawn_auto_responder`
              `pub`.
- **1g**       Write `crates/yaca-app/src/runtime.rs` with `RuntimeOptions`, `YacaRuntime`, and
              all bootstrap helpers from `crates/yaca-cli/src/main.rs:52-394` moved verbatim,
              then assembled into `YacaRuntime::start`. [serial — uses 1a-1f]
- **1h**       Rewrite `cmd_serve` / `cmd_tui_hya` / `cmd_exec` / `cmd_rpc` / `cmd_goal` / `cmd_tui`
              in `yaca-cli` to call `yaca_app::YacaRuntime::start(...)`; delete the moved fns from
              `main.rs`. `cargo check -p yaca-cli` and `cargo test -p yaca-cli` must pass. [serial]

### Phase 2 — `hya-yaca` crate (depends on Phase 1)
- **2a**       Fill in `crates/hya-yaca/Cargo.toml` deps: `hya-sdk = { path = "../hya-sdk" }`,
              `yaca-server = { workspace = true }`, `yaca-app = { path = "../yaca-app" }`,
              `axum = { workspace = true }`, `tower = { workspace = true }`,
              `http-body-util = { workspace = true }`,
              `eventsource-stream = "0.2"`, `futures-util = "0.3"`, `tokio = { workspace = true }`,
              `serde_json = { workspace = true }`, `async-trait = "0.1"`, `bytes = "1"`,
              `thiserror = "2"`. [serial]
- **2b** [||]  Write `crates/hya-yaca/src/transport.rs` per §2. Unit tests inline.
- **2c** [||]  Write `crates/hya-yaca/src/events.rs` per §3 (primary plan only).
- **2d** [||]  Write `crates/hya-yaca/src/lib.rs` re-exports + module docs.
- **2e**       `cargo test -p hya-yaca` must pass with both unit suites. [serial after 2b–2d]

### Phase 3 — wire `hya/main.rs` (depends on Phase 2)
- **3a**       Add `hya-yaca` and `yaca-app` to `crates/hya/Cargo.toml` deps. [serial]
- **3b**       Add `Transport::Yaca` variant in `crates/hya/src/main.rs`; rewrite
              `Transport::connect` per §4; add `Transport::shutdown` arm. [serial]
- **3c**       Update `print_usage` and the flag-parser comment to reflect the new default. [serial]
- **3d**       `cargo build -p hya` and `cargo clippy -p hya -- -D warnings` must pass. [serial]

### Phase 4 — accept-gate + tests (parallel after Phase 3)
- **4a** [||]  `crates/hya/tests/native_round_trip.rs` — full turn against `DevProvider`.
- **4b** [||]  `crates/hya/tests/zero_http.rs` — proc-tcp + optional strace check.
- **4c** [||]  Optional `xtask` task `verify-no-http` wrapping 4b for CI badges.

### Phase 5 — final verification
- **5a**       `cargo build --workspace`, `cargo test --workspace`,
              `cargo clippy --workspace --all-targets -- -D warnings`. [serial]
- **5b**       Manual tmux QA: `cargo run -p hya`, exercise a turn, observe streaming,
              confirm `ss -tlnp` reports no hya-owned port. [serial]
- **5c**       Atomic commits along the way (`feat(yaca-app): …`, `feat(hya-yaca): …`,
              `feat(hya): default to in-process YacaNativeTransport`,
              `test(hya): verify zero HTTP at runtime`).

---

## Atomic-unit map for parallel dispatch

```
Phase 0:  {0a, 0b, 0c, 0d}             — 4 parallel agents
Phase 1:  1a → {1b, 1c, 1d, 1e, 1f}    — 1 then 5 parallel
          → 1g → 1h                     — 2 serial
Phase 2:  2a → {2b, 2c, 2d} → 2e        — 1 then 3 parallel then 1
Phase 3:  3a → 3b → 3c → 3d             — 4 serial (tiny)
Phase 4:  {4a, 4b, 4c}                  — 3 parallel
Phase 5:  5a → 5b → 5c                  — 3 serial
```

Max sustained parallelism: 5 (Phase 1b–1f) and 4 (Phase 0). Each unit corresponds to
one file-level deliverable with verification commands listed; subagents can run with
zero coordination beyond the listed dependency.
