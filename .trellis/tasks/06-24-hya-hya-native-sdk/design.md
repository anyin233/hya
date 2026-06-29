# Design — Wire `hya` ↔ `hya` via native in-process Rust SDK (no HTTP)

> **MERGED PLAN** (main-agent synthesis of parallel planners A/conservative, B/production, C/build-graph).
> Worktree: `/chivier-disk/yanweiye/Projects/hya-hya-native`, branch `feat/hya-hya-native-sdk` (off origin/main `7089c62`).
> Trellis task: `06-24-hya-hya-native-sdk`. Source drafts: `research/plan-{A,B,C}.md`.

## 1. Goal & Accept Gate

- **Goal:** Replace `hya-sdk`'s HTTP (reqwest) + SSE transport to the hya backend with a **native in-process
  Rust binding**. `hya` links the hya backend as a library and drives it directly; native becomes the **default** path
  (the literal "native rust sdk INSTEAD OF HTTP streaming").
- **Accept gate (hard):** the **hya↔hya transport** uses **ZERO HTTP** — no TCP listener, no reqwest, no loopback connect
  between hya and hya. Proven by completing a full session turn with the **OFFLINE provider** (`force_offline`) and asserting
  the process opens **ZERO sockets** (no `listen`, no `connect` of ANY kind). Scope note: with a REAL model the only network is
  `hya → LLM-provider` outbound HTTPS (inherent to any coding agent, NOT part of hya↔hya) — the offline gate isolates and proves
  the transport claim; the architectural guarantee (§7.3) covers the real-model case (the transport still constructs no reqwest/listener).

## 2. Architecture (the native seam)

The existing `Transport` trait (`crates/hya-sdk/src/client.rs:14`) is the seam. `Client` is implemented once over any
`Transport`. We add a new in-process `Transport` that calls `hya_server::router(AppState)` — an `axum::Router` — via
`tower::ServiceExt::oneshot`, the Rust analogue of opencode's in-process `app.fetch`. No TCP, no reqwest. Events stream
in-process by `oneshot`-ing `GET /global/event` and reading the SSE body, reusing the entire 796-LOC projection.

```
hya (bin)
 └─ hya-hya (NEW)         HyaNativeTransport (oneshot router)  +  spawn_event_bridge (oneshot /global/event)
     ├─ hya-sdk            Transport trait, ApiClient, Client, GlobalEvent  (unchanged except DIRECTORY_HEADER → pub)
     ├─ hya-app (NEW)     HyaRuntime::start(opts) -> {router, engine, mcp, plugin_host, responder}
     │   └─ hya-server    router(AppState), AppState        (bootstrap extracted from hya-cli binary)
     └─ hya-server        (for fallback bus projection + types)
hya-cli  → hya-app       (re-uses the extracted bootstrap; behavior unchanged)
```

## 3. Reconciled decisions (where planners disagreed)

| Topic | A (conservative) | B (production) | C (build-graph) | **DECISION + rationale** |
|---|---|---|---|---|
| Native transport crate | feature-gated module in `hya-sdk` | new `hya-hya` | new `hya-hya` | **NEW `hya-hya` crate** (B+C). hya-sdk's design is "Client over any Transport" — backends belong OUTSIDE it. Feature-gating forces optional `reqwest`/`hya-server` deps + gated `events.rs`/`server.rs` (A itself flagged this as awkward). Cost of a crate = 1 Cargo.toml. |
| Bootstrap lib name | `hya-bootstrap` | `hya-app` | `hya-app` | **`hya-app`** (B+C majority). |
| Native default vs opt-in | `--native` opt-in | default | default | **DEFAULT** (B+C; matches "instead of HTTP streaming"). Keep `--http`/`--server`/`--opencode` as explicit fallbacks. |
| Event delivery | oneshot SSE | oneshot SSE | oneshot SSE (+bus fallback) | **oneshot `/global/event`** primary; **`engine.bus()` fallback** if flaky. Reuses projection + decoder. |
| Channel backpressure | unbounded (match HTTP) | bounded | unbounded | **UNBOUNDED** — match existing HTTP path exactly; bounded redesign is a separate concern. |
| Engine error hardening | — | harden `run_turn` | — | **OUT OF SCOPE** — changing core `SessionEngine` semantics affects both paths; not part of "wire natively". |

## 4. Explicit non-goals (do NOT build)
- No new `Transport` trait; no new `SdkError` variants (reuse `SdkError::Http(String)` for non-2xx + parse errors, exactly like `HttpTransport`).
- No re-projection of the bus on the primary path (reuse the `/global/event` 796-LOC projection in `opencode/event.rs`).
- No changes to `hya-sdk/src/{client,server,events,native}.rs` except promoting `DIRECTORY_HEADER` to `pub`.
- No moving `auth_cmd.rs` (the `hya auth` CLI subcommand), `agent_cmd`, `models_cmd`, `tui*`, `rpc`, `cli_args` out of `hya-cli`. Only the bootstrap glue moves — which DOES include `auth.rs` token helpers (because `config.rs:208` calls `crate::auth::load_token`). The staying files that reference moved items (`models_cmd.rs`, `tui.rs`, `tui/controller.rs`, `auth_cmd.rs`, `tui/harness.rs`, and every `cmd_*`) keep their `crate::…`/`super::…`/unqualified references UNCHANGED — `main.rs` re-exports the moved modules and fns via `pub use hya_app::{…}` (1i2/1i3), so NO call-site edits are needed anywhere in hya-cli.
- No `SessionEngine` behavior change; no bounded-channel redesign; no feature-gating `reqwest` out (HTTP path stays intact).

## 5. Verified build facts (from planner C, grounded in Cargo.toml)
- `tower = { version = "0.5", features = ["util"] }` is a **workspace dep** (root `Cargo.toml:34`) → `ServiceExt::oneshot` available.
- `http-body-util = "0.1"` is currently **ONLY a `hya-server` dev-dependency** — it is NOT yet in root `[workspace.dependencies]` (verify: today's root `Cargo.toml` has no `http-body-util` line). **Phase 0a is the REQUIRED step that adds it** to root `[workspace.dependencies]` so 2a can use `{ workspace = true }`. `BodyExt::collect()` + `into_data_stream()` are exercised by 30+ existing hya-server tests, proving the API works against axum 0.7 here.
- `axum 0.7` Router is `Clone` and implements `Service<Request<Body>>` after `with_state`.
- **No hya-\* crate depends on any hya-\* crate** (grep-verified) → new edges `hya→hya-hya→{hya-app,hya-server}` and `hya-cli→hya-app` form a pure DAG, no cycle.
- `DIRECTORY_HEADER` is `pub(crate)` (`hya-sdk/src/lib.rs:6`) → must become `pub`.

## 6. Component specs

### 6.1 `hya-app` (bootstrap library) — extracted verbatim from `hya-cli`
Move from `hya-cli/src/main.rs`: `today`, `discover_context_files`, `skill_dirs`, `agent_with_model`, `compaction_config`,
`spawn_team_supervisor`, `build_session_engine`, `host_info`, `headless_policy`, `offline_router`, `RuntimeConfig`+`resolve_runtime`, `open_store`.
Move whole files: `auth.rs` (token file helpers — `config.rs:208` calls `crate::auth::load_token`, so auth MUST move with config; `auth_cmd.rs` stays in cli and repoints to `hya_app::auth`), `config.rs`, `formatter_config.rs`, `plugins.rs`, `skills.rs`, `permission.rs` (PermissionPolicy + spawn_auto_responder).
Public API:
```rust
pub struct RuntimeOptions { pub model: Option<String>, pub db: String, pub yolo: bool,
                            pub default_agent: Option<String>, pub include_global_agents: bool,
                            pub force_offline: bool } // force_offline=true → use offline DevProvider
                                                      // regardless of ~/.config/hya/config.yaml (proof gates set this)
pub struct HyaRuntime {            // field order == drop order; plugin_host LAST (outlives engine Arcs)
    router: axum::Router,           // owns AppState -> Arc<McpManager> + Arc<SessionEngine> clones
    engine: Arc<hya_core::SessionEngine>,
    app_state: hya_server::AppState, // a CLONE kept for the §6.3 bus fallback (AppState: Clone); cheap (all-Arc inside)
    _permission_responder: Option<tokio::task::JoinHandle<()>>,
    _plugin_host: Arc<hya_plugin::PluginHost>,
}
// NOTE (D3): the McpManager is NOT a HyaRuntime field. `AppState::with_mcp_manager(manager)`
// (hya-server/src/state.rs:70) takes `McpManager` BY VALUE (it is non-Clone) and wraps it in
// `Arc<McpManager>` inside AppState; that AppState is owned by `router`, so the manager lives as
// long as `router`. Keeping a separate `_mcp_manager` field is impossible (value already moved).
// `_plugin_host` IS kept: `build_session_engine` returns the `Arc<PluginHost>` separately (the
// engine holds only a hook clone), and we need the outer strong ref dropped LAST.
impl HyaRuntime {
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self>;  // inlines cmd_tui_hya minus TcpListener+launch_hya
    pub fn router(&self) -> &axum::Router;
    pub fn engine(&self) -> Arc<hya_core::SessionEngine>;
    pub fn app_state(&self) -> hya_server::AppState;                  // clone; ONLY used by the §6.3 bus fallback
}
```
**`hya-app` exposes TWO layers** (both public — required because headless commands bypass the router):
- **Low-level building blocks** (all `pub`, moved verbatim): `resolve_runtime`, `RuntimeConfig`, `build_session_engine`,
  `agent_with_model`, `open_store`, `headless_policy`, `host_info`, `compaction_config`, `spawn_team_supervisor`, and the
  `permission` module (`PermissionPolicy`, `spawn_auto_responder`). The headless commands (`cmd_exec` main.rs:415, `cmd_goal`,
  `cmd_rpc`, `cmd_tui`) use THESE directly — they call `build_session_engine` + `engine.create/admit_user_prompt/run_turn`
  with a SCOPED auto-responder (`headless_policy`), NOT the router. They do NOT use `HyaRuntime`.
- **High-level convenience**: `HyaRuntime::start` — used ONLY by the server/native entrypoints (`cmd_serve`, `cmd_tui_hya`,
  and hya's native connect) that need the assembled `AppState`→`router`. `serve` binds a listener around `runtime.router()`;
  hya drives `runtime.router()` via `HyaNativeTransport`.

`hya-cli` repoints all call sites to `hya_app::*`. **Behavior unchanged** (the headless path keeps its scoped permission semantics).

### 6.2 `hya-hya` (native adapter) — `HyaNativeTransport`
```rust
// transport.rs — mirrors HttpTransport error mapping (client.rs:194-220)
pub struct HyaNativeTransport { router: axum::Router, directory: String, base_url: String /* "hya-native://in-process" */ }
#[async_trait] impl hya_sdk::Transport for HyaNativeTransport {
    async fn request(&self, method, path, body) -> Result<Value> {
        // build axum::http::Request (method, uri=path, header DIRECTORY_HEADER=dir, json body),
        // router.clone().oneshot(req).await; collect body via http_body_util::BodyExt;
        // non-2xx => SdkError::Http("status {} for {method} {path}"); empty => Value::Null; else serde_json::from_slice
    }
}
```

### 6.3 `hya-hya` — event bridge (primary)
```rust
pub fn spawn_event_bridge(router: axum::Router, directory: String,
                          tx: mpsc::UnboundedSender<GlobalEvent>) -> JoinHandle<()> {
    // loop: oneshot GET /global/event (DIRECTORY_HEADER) -> resp.into_body().into_data_stream().eventsource()
    //       -> serde_json::from_str::<GlobalEvent> -> tx.send; reconnect-on-end with small backoff (mirrors main.rs:270-285)
}
```
**Fallback** — a **dev-time implementation swap** (NOT a runtime auto-switch), triggered ONLY when a concrete Phase-2 gate
fails: (a) unit `event_bridge_holds_30s_heartbeat` yields **0 frames within 30s** of holding the body open, OR (b)
`event_bridge_emits_connected_then_event` fails to deliver the projected event within a **2s** budget, OR (c)
`native_round_trip` flakes on event ordering across **20 consecutive runs**. On any trigger, add to hya-server a PUBLIC projector
that takes the PUBLIC `AppState` and ENCAPSULATES the private `ServerState` internally (both `ServerState` and `ServerState::new`
are `pub(crate)` — state.rs:88,108 — so they must NOT appear in the public signature): define
`pub async fn project_envelope_to_global_event(state: &AppState, env: Envelope) -> Option<Value>` IN hya-server, body
`let ss = ServerState::new(state.clone()); /* call existing private subscribe_global/envelope_payload projection */`
(`opencode/event.rs`). The fallback bridge needs `HyaRuntime` to expose the AppState (`pub fn app_state(&self) -> AppState`,
`AppState: Clone`), then drives `engine.bus().subscribe()` → `project_envelope_to_global_event(&rt.app_state(), env)` →
`{"payload": ...}` → tx. **No `ServerState` in the public API.** Same wire shape; hya cannot tell. Primary (oneshot-SSE) and
fallback (bus) are mutually exclusive at build time; ship whichever passes the Phase-2 gates.

### 6.4 `hya/main.rs` wiring
- Add `Transport::Hya { runtime: Arc<hya_app::HyaRuntime>, bridge: JoinHandle<()> }`.
- `Transport::connect`: when `args.server.is_none() && !args.http && !args.opencode` → build `HyaRuntime::start(RuntimeOptions{
  model:None, db:String::new(), yolo:false, default_agent:None, include_global_agents:true, force_offline:false })`,
  `HyaNativeTransport::new(runtime.router().clone(), directory)`, `ApiClient::with_transport`, spawn bridge → forward to `AppEvent::Sse`.
  (`yolo:false` → permission requests surface to the TUI via the existing `permission_reply` path, exactly like the HTTP path.)
- `shutdown`: `bridge.abort(); drop(runtime)` (drops engine → mcp → plugin_host in order).
- Flags after change: *(none)* → native default; `--http` → spawn `hya serve` + reqwest/SSE (today's default, preserved);
  `--server <url>` → attach; `--opencode` → Bun bridge; `--hya-bin` retained (used by `--http`). Update `print_usage`.
  NOTE: hya's current `Args` (main.rs:364) has NO `yolo` field; native mode uses `yolo:false` (scoped, TUI-mediated permissions).
  A hya `--yolo` flag is **OUT OF SCOPE** for this task (the proof tests set `yolo:true` directly in `RuntimeOptions`, not via a CLI flag).

## 7. Accept-gate proof (ship all three)
1. **In-process per-process socket-ownership test** (`crates/hya/tests/native_round_trip.rs`): boot `HyaRuntime` (offline
   `DevProvider`), run a full turn via `HyaNativeTransport`, then assert OUR PROCESS owns no network socket. `/proc/self/net/tcp{,6}`
   is **namespace-wide** (shows other processes too — incl. the concurrently-running main checkout), so a bare LISTEN/ESTABLISHED
   scan has false positives. Instead: (i) enumerate `/proc/self/fd/*`, `read_link` each, collect the set of `socket:[INODE]` we own;
   (ii) parse `/proc/self/net/tcp{,6}` rows (local, remote, state, inode); (iii) for each row whose inode ∈ our set, assert
   `state != LISTEN(0A)` AND not (`state == ESTABLISHED(01)` with a loopback `127.0.0.1`/`::1` remote) — catches an accidental
   outbound HTTP `connect`, not just an inbound `bind`. This needs a NEW small self-fd helper (reuse the `/proc/net/tcp` row-parsing
   shape from `hya-sdk/src/server.rs:172-229`, but filter by OUR fd inodes, not by port). **SUPPLEMENTARY** — a post-turn snapshot
   catches a LINGERING socket but can MISS a transient loopback `connect` that opened+closed mid-turn; #2 covers that.
2. **strace gate** (linux, **AUTHORITATIVE, mandatory in linux CI**): trace the **headless `native_round_trip` test binary**
   (   `strace -f -e trace=socket,bind,connect,listen,accept <test-bin> native_turn_opens_no_socket --exact`) → zero INET network
   syscalls: zero `socket(AF_INET`/`socket(AF_INET6`, zero `bind(`/`connect(`/`listen(`/`accept(` on inet fds (AF_UNIX allowed;
   offline provider → no provider calls). Tracing the test binary (not the raw-mode `hya` TUI) needs no PTY
   harness yet exercises the identical native path; it is the only gate that observes TRANSIENT loopback connects. Fails (not skips)
   if strace missing on a linux CI runner.
3. **Architectural guarantee** (strongest, design-level): the native `Transport::Hya` path constructs NO `reqwest::Client`
   and binds NO `TcpListener` — the code simply never calls either on this branch. #1/#2 are runtime confirmations of this invariant.
4. **Manual tmux QA** (offline-isolated, deterministic): run `HOME=$(mktemp -d) cargo run -p hya` (no flags) under
   `interactive_bash`/tmux — the empty HOME means no `~/.config/hya/config.yaml`, so `resolve_runtime` falls back to the offline
   DevProvider → socket-free. Expect: TUI renders, send a prompt, see streaming offline text, Ctrl-C → clean exit;
   `ss -tlnp | grep <pid>` shows no hya-owned listening port.

## 8. Test strategy
- **Unit (hya-hya):** transport GET/POST/empty-body/non-2xx/directory-header; event-bridge connected-then-event; resync-on-lag.
- **Integration (hya):** `native_round_trip.rs` (full turn + ordered events + zero-listen assertion).
- **Regression:** `cargo test -p hya-cli` after each bootstrap move (behavior parity).
- **Gates:** `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`.

## 9. Risks (top) + mitigations
1. **Bootstrap move changes runtime behavior / drop order** (H×H): atomic per-file `git mv`, `cargo check -p hya-cli` + `cargo test -p hya-cli` after each; explicit `HyaRuntime` field/drop order with comment; one commit per move (easy revert).
2. **`hya-cli` private types leak across new crate boundary** (e.g. `tui.rs` reads `config::ModelEntry`) (M×H): mark moved public surface `pub`; `cargo check -p hya-cli` gates each move.
3. **SSE-over-oneshot streams nothing / heartbeat lifecycle** (M×M): stress test (>30s, assert heartbeat); fallback to direct bus subscribe (§6.3).
4. **`http-body-util` version drift** (L×M): promote to workspace dep; both crates `{ workspace = true }`.
5. **`DIRECTORY_HEADER` privacy** (H×L): flip to `pub`.
6. **Slow MCP connect at startup** (M×M): `HyaRuntime::start` awaited inside existing `spawn_connect` task → TUI shows "Starting backend…" toast meanwhile.

## 10. Atomic implementation units (topologically sorted; `[||]` = parallel-safe)

**Phase 0 — workspace prep** (0a-0d parallel → 0e serial)
- 0a [||] add `http-body-util = "0.1"` to root `Cargo.toml` `[workspace.dependencies]`. (1 edit + `cargo check`)
- 0b [||] promote `DIRECTORY_HEADER` `pub(crate)`→`pub` (`hya-sdk/src/lib.rs:6`). (1 edit + `cargo check -p hya-sdk`)
- 0c [||] create `crates/hya-app/{Cargo.toml,src/lib.rs}` skeleton (empty lib, deps filled in 1a). (writes)
- 0d [||] create `crates/hya-hya/{Cargo.toml,src/lib.rs}` skeleton (empty lib, deps filled in 2a). (writes)
- 0e [after 0c,0d] register internal path deps in root `[workspace.dependencies]`: `hya-app = { path = "crates/hya-app" }` and `hya-hya = { path = "crates/hya-hya" }` (REQUIRED so `{ workspace = true }` resolves in 1a/2a/3a); `cargo metadata` succeeds. (1 edit + check)

**Phase 1 — bootstrap extraction** (each unit = 1-2 edits + 1 check, i.e. 1-3 tool calls. File moves are SERIAL: `config.rs`
references `crate::auth::load_token` (config.rs:208), and all 6 moves append `pub mod` to the same `hya-app/src/lib.rs`, so
parallel moves would race the lib.rs edit AND could `cargo check` before `auth` lands. Verified the ONLY inter-file dep is
config→auth; the other four are independent — order auth→config first, then the rest.)
> **Deterministic `pub`-flip procedure** (resolves "flip referenced symbols" ambiguity): the EXACT items needing `pub`
> are named by the compiler. The named symbols below (`config::ModelEntry`, `permission::{PermissionPolicy, spawn_auto_responder}`,
> `auth::{load_token,save_token,list_tokens,remove_token}`) are known; for any others, the `cargo check` at units 1h1-1h5
> (hya-app internal references from `runtime.rs`) and 1i1-1i4 (hya-cli cross-crate references) emit `E0603 (private item)`
> / `E0432 (unresolved import)` naming each item — flip exactly those to `pub`, re-run the check, repeat until clean. No guessing.
- 1a fill `hya-app/Cargo.toml` deps (core/provider/tool/store/server/proto/mcp/plugin + axum/tokio/serde/serde_json/anyhow/thiserror/time/uuid/serde_norway, each `{ workspace = true }`); `cargo check -p hya-app`.
- 1b move `auth.rs` (FIRST — config depends on it): `git mv crates/hya-cli/src/auth.rs crates/hya-app/src/auth.rs`, add `pub mod auth;` to `hya-app/src/lib.rs`, ensure `auth::{load_token,save_token,list_tokens,remove_token}` are `pub`; `cargo check -p hya-app`.
- 1c move `config.rs` (after auth — its `crate::auth::load_token` now resolves inside hya-app): `git mv`, `pub mod config;`, flip `config::ModelEntry` + other hya-cli-referenced items to `pub`; `cargo check -p hya-app`.
- 1d move `skills.rs`: `git mv`, `pub mod skills;`, flip referenced symbols `pub`; `cargo check -p hya-app`.
- 1e move `formatter_config.rs`: `git mv`, `pub mod formatter_config;`, flip `pub`; `cargo check -p hya-app`.
- 1f move `plugins.rs`: `git mv`, `pub mod plugins;`, flip `pub`; `cargo check -p hya-app`.
- 1g move `permission.rs`: `git mv`, `pub mod permission;`, flip `permission::{PermissionPolicy, spawn_auto_responder}` `pub`; `cargo check -p hya-app`. (`hya-cli/src/auth_cmd.rs` import is repointed later in 1i3.)
- 1h1 move pure helpers verbatim → `hya-app/src/runtime.rs` and mark ALL `pub` (every one is re-exported by hya-cli in 1i3): `pub fn today`, `pub fn discover_context_files`, `pub fn skill_dirs`, `pub fn host_info`, `pub fn headless_policy`, `pub fn offline_router`, `pub fn compaction_config`; `cargo check -p hya-app`.
- 1h2 move config/engine helpers → runtime.rs: `pub fn agent_with_model`, `pub struct RuntimeConfig` with ALL fields `pub` (`router, model, models, mcp, plugins, default_agent` — hya-cli call sites read `runtime.model`/`.router`/etc. cross-crate), `pub fn resolve_runtime`, `pub fn open_store`; `cargo check -p hya-app`.
- 1h3 move `pub async fn build_session_engine` + `spawn_team_supervisor` → runtime.rs (build_session_engine is `pub` — `cmd_exec`/`cmd_goal`/`cmd_rpc`/`cmd_tui` call it directly); `cargo check -p hya-app`.
- 1h4 add `RuntimeOptions` (incl. `force_offline: bool`) + `HyaRuntime` struct (drop order: router, engine, app_state, _permission_responder, _plugin_host — NO `_mcp_manager`, see §6.1 NOTE) + `router()`/`engine()`/`app_state()` accessors; `cargo check -p hya-app`.
- 1h5 implement `HyaRuntime::start(opts)` = inline `cmd_tui_hya` assembly (`serve.rs:53-110`) minus `TcpListener::bind`/`launch_hya`. Build the `AppState`, store a CLONE in the `app_state` field, pass the original to `hya_server::router(state)`, keep the result in `router`. Honor `opts.force_offline`: when true, use `offline_router()` (DevProvider) instead of `resolve_runtime(..)` so the runtime makes NO outbound provider calls (proof gates rely on this). `cargo check -p hya-app`.
- 1i1 add `hya-app = { workspace = true }` to `hya-cli/Cargo.toml`. GATE `cargo check -p hya-app` green. (hya-cli is INTENTIONALLY red between 1c and 1i2 — its `mod config;` etc. point at moved files; the shim in 1i2 restores green WITHOUT touching the ~5 referencing files.)
- 1i2 **re-export shims** (this is the trick that keeps every existing `crate::{auth,config,permission}::…` reference resolving with ZERO edits to referencing files): in `main.rs`, replace each `mod auth;`/`mod config;`/`mod plugins;`/`mod skills;`/`mod formatter_config;`/`mod permission;` with `pub use hya_app::{auth, config, plugins, skills, formatter_config, permission};`. Now `crate::config::ModelEntry` (tui.rs:37, models_cmd.rs:3, tui/controller.rs:8), `crate::auth::*` (auth_cmd.rs:4, config.rs:208 is internal to hya-app now), and `crate::permission::*` (main.rs:49, serve.rs:6) ALL resolve via the re-exports — no edits to those files. GATE `cargo check -p hya-cli` GREEN (single edit, green again).
- 1i3 in `main.rs`, delete the duplicated bootstrap fn DEFINITIONS and replace them with a SINGLE `pub use` re-export so EVERY call site keeps resolving with ZERO edits (the same shim trick as 1i2, applied to fns). Delete defs of and re-export the COMPLETE set (grep-verified — every fn hya-cli still calls): `pub use hya_app::{RuntimeConfig, resolve_runtime, build_session_engine, agent_with_model, open_store, offline_router, headless_policy, host_info, compaction_config, spawn_team_supervisor, discover_context_files, today, skill_dirs};`. Because these are `pub use`, ALL existing call sites resolve UNCHANGED: unqualified in `main.rs` (`cmd_exec`:405-415, `cmd_goal`:455-465, `cmd_rpc`/`cmd_tui`:515-526/577-587, `cmd_tail_session`:622-625/643/703), `super::…` in `serve.rs:8`, and `crate::…` in `tui/harness.rs:77,85`. **NO call-site edits anywhere** — re-exports cover models_cmd/tui/controller/auth_cmd (via 1i2 module re-exports) and every headless cmd (via these fn re-exports). REQUIRES (from Phase 1h) that all these fns + `RuntimeConfig`'s fields are `pub`. GATE `cargo check -p hya-cli && cargo test -p hya-cli` clean (behavior parity). (1i2/1i3 are each a single-file edit with its own green gate — no mega-unit, no per-call-site churn.)

**Phase 2 — `hya-hya` crate** (2a → {2b,2c} 2-parallel → 2d → 2e). 2d re-exports the `transport`/`events` modules so it is SERIAL after 2b+2c. Each unit lists its own gate command + observable assertion.
- 2a fill `hya-hya/Cargo.toml` deps (`hya-sdk`={path}, `hya-app`/`hya-server`/`axum`/`tower`/`http-body-util`/`tokio`/`serde_json`={workspace=true}, `eventsource-stream`="0.2", `futures-util`="0.3", `async-trait`="0.1", `bytes`="1", `thiserror`={workspace=true}); GATE `cargo check -p hya-hya` (empty lib compiles, deps resolve).
- 2b1 [||] write `hya-hya/src/transport.rs` impl ONLY: `HyaNativeTransport` struct + `#[async_trait] impl hya_sdk::Transport` exactly per §6.2 (oneshot, `BodyExt::collect`, non-2xx→`SdkError::Http`, empty→`Value::Null`). GATE `cargo check -p hya-hya`.
- 2b2 [after 2b1] add the inline `#[tokio::test]`s to transport.rs: `get_config_returns_object` (boot `HyaRuntime::start(RuntimeOptions{db:String::new(),model:None,yolo:true,default_agent:None,include_global_agents:false,force_offline:true})`, `request("GET","/config",None)` → `.is_object()`), `post_session_create_returns_id` (`.id` non-empty), `delete_session_returns_ok` (`client.session_delete(<new id>)`→`Ok(())`; the raw `DELETE /session/{id}` returns `Json(<bool>)` not empty — `session_legacy.rs:146` — do NOT assert `Value::Null`), `non_2xx_is_sdkerror_http` (GET `/session/zzz`→`Err(SdkError::Http(_))`), `directory_header_propagated`. GATE `cargo test -p hya-hya transport::`.
- 2c1 [||] write `hya-hya/src/events.rs` impl ONLY: `spawn_event_bridge` exactly per §6.3. GATE `cargo check -p hya-hya`.
- 2c2 [after 2c1] add inline tests to events.rs: `bridge_emits_connected_first` (first decoded `GlobalEvent` payload type == `"server.connected"`), `bridge_forwards_published_envelope` (publish `MessageStarted` via `engine.bus()`, arrives on mpsc <2s), `bridge_holds_30s_heartbeat` (`#[ignore]`, ≥1 frame/30s). GATE `cargo test -p hya-hya events::` (non-ignored).
- 2d [after 2b2+2c2] write `hya-hya/src/lib.rs`: `mod transport; mod events; pub use transport::HyaNativeTransport; pub use events::spawn_event_bridge;` + module docs. GATE `cargo check -p hya-hya`.
- 2e GATE `cargo test -p hya-hya && cargo clippy -p hya-hya -- -D warnings` both clean.

**Phase 3 — wire `hya`** (serial; each = 1-2 edits + 1 check)
- 3a add `hya-hya = { path = "../hya-hya" }` + `hya-app = { workspace = true }` to `crates/hya/Cargo.toml`; `cargo check -p hya`.
- 3b1 add the `Transport::Hya { runtime: Arc<hya_app::HyaRuntime>, bridge: JoinHandle<()> }` enum variant in `hya/src/main.rs`; `cargo check -p hya`.
- 3b2 add the native default branch in `Transport::connect` (build `HyaRuntime::start`, `HyaNativeTransport`, spawn bridge → `AppEvent::Sse`); `cargo check -p hya`.
- 3b3 add the `Transport::Hya` arm to `shutdown` (`bridge.abort(); drop(runtime)`); `cargo check -p hya`.
- 3c update `print_usage` + flag-parser comments for the new default; `cargo check -p hya`.
- 3d `cargo clippy -p hya -- -D warnings` (note the `--` separator) clean.

**Phase 4 — accept-gate + tests** (4a,4b parallel-author; 4c optional). Exact bodies + assertions + gate commands.
- 4a1 write the turn body of `crates/hya/tests/native_round_trip.rs` `#[tokio::test] async fn native_turn_opens_no_socket()`: `HyaRuntime::start(RuntimeOptions{db:String::new(),model:None,yolo:true,default_agent:None,include_global_agents:false,force_offline:true})` (offline → the ONLY thing that could open a socket is the transport, so the socket assertion in 4a2 is exact); build `HyaNativeTransport::new(rt.router().clone(),"/tmp")` + `ApiClient::with_transport`; spawn `spawn_event_bridge`; `client.session_create()` → assert id non-empty; `client.session_prompt(id, json!({"parts":[{"type":"text","text":"hi"}]}))`; collect bridge events until a text `message.part` frame AND a turn-idle/finished frame (timeout 30s); ASSERT prompt `Ok` + ≥1 text part. GATE `cargo test -p hya --test native_round_trip`.
- 4a2 APPEND the §7.1 self-fd socket check to the END of the SAME `native_turn_opens_no_socket()` body (after the turn, before return): collect our `socket:[inode]` set from `/proc/self/fd/*`, parse `/proc/self/net/tcp{,6}`, assert OUR process owns no LISTEN(`0A`) and no ESTABLISHED(`01`)-to-loopback socket. (REQUIRED to be in the SAME test fn — a standalone `#[test]` would not observe a socket opened during the turn.) GATE re-run `cargo test -p hya --test native_round_trip`.
- 4b [after 4a2 — REQUIRES the `native_round_trip` test binary that 4a builds] **AUTHORITATIVE** automated strace gate — trace the **headless `native_round_trip` TEST binary** (NOT the interactive `hya` TUI, which needs a PTY): `cargo test -p hya --test native_round_trip --no-run` to build it, locate the artifact, then `strace -f -e trace=socket,bind,connect,listen,accept <artifact> native_turn_opens_no_socket --exact --test-threads=1`; parse the trace and ASSERT zero **INET** network syscalls — zero `socket(AF_INET`/`socket(AF_INET6` creations AND zero `bind(`/`connect(`/`listen(`/`accept(` on an inet fd (AF_UNIX/local-IPC sockets are ALLOWED — they are not HTTP/network). With `force_offline:true` the backend makes NO outbound provider calls, so a correct native turn emits NONE of these (matching the "ZERO sockets" accept gate). The test binary drives the EXACT native transport+bridge+full-turn path programmatically with NO raw-mode TUI, so no PTY/scripted-input harness is needed; it is the ONLY gate that catches a TRANSIENT loopback connect (opened+closed mid-turn) that 4a's post-turn fd snapshot would miss. Implement as `scripts/verify-no-http.sh` (exact body, deterministic — no parsing ambiguity):
  ```bash
  #!/usr/bin/env bash
  set -euo pipefail
  command -v strace >/dev/null || { echo "strace required"; exit 1; }   # FAIL, not skip
  # 1) artifact discovery via cargo JSON (no path guessing):
  BIN=$(cargo test -p hya --test native_round_trip --no-run --message-format=json \
        | jq -r 'select(.executable != null and .target.name == "native_round_trip") | .executable' | tail -1)
  TRACE=$(mktemp)
  # 2) isolated empty HOME → no ~/.config/hya config → offline DevProvider (belt-and-suspenders with force_offline):
  HOME=$(mktemp -d) strace -f -e trace=socket,bind,connect,listen,accept -o "$TRACE" \
        "$BIN" native_turn_opens_no_socket --exact --nocapture
  # 3) assert ZERO inet network syscalls (AF_UNIX allowed):
  if grep -E 'socket\(AF_INET6?|^.*\bconnect\(|^.*\bbind\(|^.*\blisten\(|^.*\baccept\(' "$TRACE" \
       | grep -Ev 'AF_UNIX|AF_NETLINK'; then
    echo "FAIL: native turn opened an inet socket"; exit 1
  fi
  echo "OK: zero inet sockets"
  ```
  The optional `xtask` form is 4c. **MANDATORY in linux CI** (4d); skipped only on non-linux.
- 4c [||] optional: also expose 4b via the EXISTING `xtask` crate as `cargo run -p xtask -- verify-no-http`; GATE exits 0. (Belt-and-suspenders; the bash script in 4b is the canonical form.)
- 4d wire the gate into CI — edit `.github/workflows/ci.yml` (the single ubuntu-latest `check` job, lines 11-27): after the `test` step add `- name: install strace\n  run: sudo apt-get update && sudo apt-get install -y strace` then `- name: verify-no-http\n  run: bash scripts/verify-no-http.sh`. Since the runner is `ubuntu-latest` (linux), the strace gate is MANDATORY — the job FAILS if a socket is opened or strace is missing. GATE: the CI `check` job is green with the added steps (validate locally with `bash scripts/verify-no-http.sh` exit 0).

**Phase 5 — final** (serial; each one command/assertion)
- 5a `cargo build --workspace` → exit 0.
- 5b `cargo test --workspace` → all pass (or pre-existing failures explicitly noted vs the baseline).
- 5c `cargo clippy --workspace --all-targets -- -D warnings` → clean.
- 5d `cargo fmt --all --check` → clean (matches ci.yml:21).
- 5e `bash scripts/verify-no-http.sh` → prints `OK: zero inet sockets`, exit 0.
- 5f manual tmux QA per §7.4: `HOME=$(mktemp -d) cargo run -p hya` under interactive_bash → render, prompt, offline stream, Ctrl-C clean exit, `ss -tlnp` shows no hya port.
- 5g confirm one atomic conventional commit per phase exists (`git log --oneline`); none left uncommitted.

**Parallel dispatch map:** P0={0a,0b,0c,0d}(4)→0e → P1=1a→1b→1c→1d→1e→1f→1g→1h1→1h2→1h3→1h4→1h5→1i1→1i2→1i3 (P1 serial — file-move ordering + shared lib.rs; 1i2 module re-exports, 1i3 fn re-exports, each its own green gate) → P2=2a→{2b1,2c1}(2)→{2b2,2c2}(2)→2d→2e → P3=3a→3b1→3b2→3b3→3c→3d → P4=4a1→4a2→4b→4c→4d (4b/4c/4d need 4a's test binary, so P4 is serial) → P5=5a→5b→5c→5d→5e→5f→5g (serial).
Subagent fan-out parallelism lives in P0 (4-wide: 0a-0d) and P2 (2-wide: {2b1,2c1} then {2b2,2c2}); P1, P3, P4, P5 are serial by data dependency.
Commit atomically after each phase; rebase/merge `origin/main` between phases to adopt backend changes.

## 11. Plan Review

### Round 1 — oracle (ran as claude-opus-4-7, same-family — NOT cross-family compliant) — VERDICT: FAIL
- D1 PASS; D2 FAIL (1g/1h too coarse); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fixes applied: split 1g→1g1/1g2/1g3 and 1h→1h1/1h2/1h3 (each 1-3 tool calls); tightened §7.1 proof to also assert no ESTABLISHED loopback row.
- Cross-family gate not satisfied by oracle (fell back to Claude). Re-dispatching to a GPT-family reviewer via codex CLI (Path B) for Round 2.

### Round 2 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (units still too coarse); D3 FAIL (`HyaRuntime._mcp_manager` can't coexist with `with_mcp_manager` move of non-Clone McpManager; `hya-app` workspace dep unregistered); D4 PASS; D5 FAIL (proc-tcp namespace-wide, needs self-fd inode ownership); D6 PASS.
- Fixes applied: (D3) removed `_mcp_manager` from `HyaRuntime` + §6.1 NOTE; added 0e to register `hya-app`/`hya-hya` in root `[workspace.dependencies]`. (D2) split Phase 1 into 1a/1b-1f/1g1-1g5/1h1-1h4, each 1-3 tool calls. (D5) rewrote §7.1 to self-fd inode ownership + loopback-ESTABLISHED check.
- Re-dispatching codex `gpt-5.5` for Round 3.

### Round 3 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (units still bundle "flip symbols" + `Transport::Hya`+connect+shutdown); D3 FAIL (`config.rs:208` calls `crate::auth::load_token` — auth must move too); D4 FAIL (SSE fallback "if flaky" lacks abort threshold); D5 FAIL (invalid clippy syntax `-p hya -D warnings`); D6 PASS.
- Fixes applied: (D3) added `auth.rs` to the move set (1b-1g now 6 files) + `auth_cmd.rs` repoint in 1i3. (D2) split 3b→3b1/3b2/3b3, relabeled runtime/cli units 1h*/1i*, made "flip symbols" concrete with named symbols. (D4) §6.3 defines concrete fallback triggers (0 frames/30s, 2s event budget, 20-run flake). (D5) fixed to `cargo clippy -p hya -- -D warnings`.
- Re-dispatching codex `gpt-5.5` for Round 4.

### Round 4 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (auth/config moves marked parallel but config depends on auth → check-order race); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fix applied: made Phase-1 file moves SERIAL in dependency order (auth→config→skills→formatter_config→plugins→permission); verified config→auth is the only inter-file dep; updated dispatch map (P1 serial; parallelism preserved in P0/P2/P4).
- Re-dispatching codex `gpt-5.5` for Round 5.

### Round 5 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (Phase 2/4 units + "flip referenced symbols" too coarse); D3 PASS; D4 PASS; D5 FAIL (2a no check, 2b-2d defer to 2e, 4a/4b lack exact commands/assertions); D6 PASS.
- Fixes applied: Phase 2 rewritten with per-unit GATE commands + named inline tests with explicit assertions; Phase 4 rewritten with exact test bodies, assertions, and `cargo test --test` gates; added the deterministic compiler-driven `pub`-flip procedure (E0603/E0432-named) to Phase 1.
- Re-dispatching codex `gpt-5.5` for Round 6.

### Round 6 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (2d parallel with 2b/2c but exports their modules); D3 FAIL (1i3 missed repoint sites `models_cmd.rs:3`, `tui/controller.rs:8`, `tui.rs:37`); D4 PASS; D5 FAIL (socket test standalone not folded into turn; `DELETE /session` returns `Json(bool)` not empty); D6 PASS.
- Fixes applied: (D2) 2d serial after 2b+2c; (D3) 1i2/1i3 now enumerate EVERY `crate::{auth,config,permission}` site by file:line (grep-verified complete); (D5) folded the self-fd socket assertion into the single `native_turn_opens_no_socket` test body (post-turn), made 4b an automated strace gate, and corrected the DELETE test to `session_delete → Ok(())` (route returns `Json(bool)`, session_legacy.rs:146).
- Re-dispatching codex `gpt-5.5` for Round 7.

### Round 7 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 FAIL (non-goal forbade moving `auth*` but plan moves `auth.rs` — internal contradiction); D2 FAIL + D3 FAIL (headless `cmd_exec` (main.rs:415) uses `build_session_engine`+scoped responder DIRECTLY, not the router — `HyaRuntime::start` alone doesn't model it); D4 PASS; D5 FAIL (post-turn fd snapshot can't prove no TRANSIENT loopback connect; strace gate could auto-skip); D6 PASS.
- Fixes applied: (D1) narrowed non-goal to `auth_cmd.rs`; explicitly allow `auth.rs` helper move. (D2/D3) documented `hya-app`'s TWO-layer public API — low-level building blocks (`build_session_engine`, `resolve_runtime`, `agent_with_model`, `open_store`, `headless_policy`, `permission::*`, all `pub`) for headless cmds + `HyaRuntime::start` for server/native; marked 1h fns `pub`; clarified 1i3 headless call sites. (D5) made strace gate AUTHORITATIVE + mandatory in linux CI; reframed fd-check as supplementary; added the architectural no-reqwest/no-listener guarantee.
- Re-dispatching codex `gpt-5.5` for Round 8.

### Round 8 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (1i3/2b/4a still > 1-3 tool calls); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fixes applied: split 2b→2b1(impl)/2b2(tests), 2c→2c1/2c2; 1i3→1i3(imports)/1i4(call sites)/1i5(delete); 4a→4a1(turn)/4a2(append socket check to same fn). Updated dispatch map. All units now 1-3 tool calls; substantive dims D1/D3/D4/D5/D6 stable PASS across rounds.
- Re-dispatching codex `gpt-5.5` for Round 9 (final granularity confirmation).

### Round 9 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 PASS; D3 PASS; D4 PASS; D5 FAIL (strace gate on raw-mode `hya` TUI needs a PTY/harness it didn't specify); D6 PASS.
- Fix applied: 4b + §7.2 now trace the HEADLESS `native_round_trip` test binary (`<test-bin> native_turn_opens_no_socket --exact`) instead of the interactive TUI — identical native path, no PTY needed, deterministic.
- Re-dispatching codex `gpt-5.5` for Round 10.

### Round 10 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 PASS; D3 FAIL + D5 FAIL (zero-HTTP claim false while `HyaRuntime::start` loads real HTTP providers → backend hya→LLM calls; proof allowed provider connects so couldn't prove zero HTTP); D4 FAIL (fallback projector is async over private `ServerState`, not `&AppState`); D6 PASS.
- Fixes applied: (D3/D5) scoped accept gate to the hya↔hya TRANSPORT; added `force_offline: bool` to `RuntimeOptions` (1h5 honors it via `offline_router()`); proof gates (2b2/4a1) set `force_offline:true` and now assert ZERO sockets of any kind (dropped the "provider IPs allowed" carve-out). (D4) fallback now `pub async fn project_envelope_to_global_event(&ServerState, Envelope) -> Option<Value>` + `ServerState::new(AppState)` (state.rs:108) + `HyaRuntime::app_state()` accessor (`AppState: Clone`).
- Re-dispatching codex `gpt-5.5` for Round 11.

### Round 11 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL + D5 FAIL (1i4 removed stale `mod` decls / gated clean check while duplicated helper defs remained until 1i5 — no compilable intermediate exists); D3 FAIL (fallback used `ServerState::new`, but `ServerState`+`new` are `pub(crate)` — state.rs:88,108); D4 PASS; D6 PASS.
- Fixes applied: (D2/D5) merged 1i into 1i1(Cargo dep) + 1i2 = ONE indivisible hya-cli compile-unit (remove mod decls + delete dup defs + repoint all sites together; single `cargo check && test` gate), documented why it cannot be subdivided. (D3) public projector now `pub async fn project_envelope_to_global_event(state: &AppState, env) -> Option<Value>` defined IN hya-server, encapsulating `ServerState::new(state.clone())` internally — no `pub(crate)` leak; `HyaRuntime::app_state()` accessor added.
- Re-dispatching codex `gpt-5.5` for Round 12.

### Round 12 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (1i2 substeps not enumerated with one declared gate; 4b xtask-vs-bash ambiguity); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fixes applied: 1i2 now lists ordered substeps (s1-s6) + ONE declared final gate; 4b picks the `bash scripts/verify-no-http.sh` wrapper (xtask is the optional 4c). All substantive dims (D1,D3,D4,D5,D6) stable PASS.
- Re-dispatching codex `gpt-5.5` for Round 13.

### Round 13 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (1i2 still bundled); D3 FAIL (clarify `http-body-util` is dev-dep-only today); D4 FAIL (`app_state()` referenced but not in struct API/Phase 1); D5 FAIL (no unit wires strace into `.github/workflows/ci.yml`); D6 PASS.
- Fixes applied: (D2) adopted the reviewer's **re-export shim** idea — 1i2 replaces `mod X;` with `pub use hya_app::{…}` (keeps all `crate::X::…` refs resolving, green after one edit), 1i3 deletes dup defs + imports (separate green gate); no mega-unit. (D3) §5 states http-body-util is dev-dep-only today + 0a adds the root dep. (D4) added `app_state: AppState` field + `app_state()` accessor to §6.1 + 1h4/1h5. (D5) added unit 4d wiring `scripts/verify-no-http.sh` into the ubuntu-latest `check` job in ci.yml (lines 11-27), strace mandatory.
- Re-dispatching codex `gpt-5.5` for Round 14.

### Round 14 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL + D5 FAIL (same point: 4b strace gate marked parallel with 4a but needs 4a's test binary); D3 PASS; D4 PASS; D6 PASS.
- Fix applied: 4b now `[after 4a2]` (needs the `native_round_trip` test binary); dispatch map P4 serialized (4a1→4a2→4b→4c→4d).
- Re-dispatching codex `gpt-5.5` for Round 15.

### Round 15 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 PASS; D3 PASS; D4 PASS; D5 FAIL ("ZERO sockets" gate but strace asserted only zero `listen`/`connect`); D6 PASS.
- Fix applied: 4b + §7.2 now assert zero of the FULL INET syscall set — `socket(AF_INET*)`, `bind`, `connect`, `listen`, `accept` on inet fds (AF_UNIX allowed) — matching the accept gate.
- Re-dispatching codex `gpt-5.5` for Round 16.

### Round 16 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (`cmd_tail_session` main.rs:623 calls `offline_router`, omitted from 1i3 imports/sites); D3 FAIL (`--yolo` claimed "retained" but hya Args main.rs:364 has no such flag); D4 PASS; D5 PASS; D6 PASS.
- Fixes applied: grep-enumerated EVERY bootstrap call site (main.rs cmd_exec/goal/rpc/tui/tail_session, serve.rs, tui/harness.rs) and adopted the FULL re-export approach — 1i3 `pub use hya_app::{RuntimeConfig, …, offline_router, spawn_team_supervisor, today, discover_context_files, skill_dirs}` so ALL call sites resolve UNCHANGED (no per-site repointing; kills the missed-site bug class). 1h1/1h2 mark all fns + RuntimeConfig fields `pub`. §4 updated. (D3) removed the `--yolo` retained claim; native uses `yolo:false` (TUI-mediated); hya `--yolo` is out of scope; §6.4 connect shows the full `RuntimeOptions`.
- Re-dispatching codex `gpt-5.5` for Round 17.

### Round 17 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (Phase 4b/5 bundled artifact discovery/trace parse/gates/QA/commits); D3 PASS; D4 PASS; D5 FAIL (strace parsing + manual QA underspecified; `cargo run -p hya` didn't force offline/isolate config); D6 PASS.
- Fixes applied: 4b now embeds the EXACT `scripts/verify-no-http.sh` body (cargo-JSON artifact discovery via jq, isolated `HOME=$(mktemp -d)`, strace to file, precise grep asserting zero inet `socket/bind/connect/listen/accept`, AF_UNIX allowed); §7.4 manual QA uses isolated `HOME` → offline DevProvider (socket-free); Phase 5 split into 5a-5g (one command/assertion each, incl. fmt + verify-no-http.sh + per-phase-commit check). Architecture/Phases 0-3 + D1/D3/D4/D6 stable PASS since R12.
- Re-dispatching codex `gpt-5.5` for Round 18.

### Round 18 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (dispatch-map footnote still said "P4 (3-wide)" while P4 is serial); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fix applied: corrected the parallelism footnote — fan-out is P0 (4-wide) + P2 (2-wide); P1/P3/P4/P5 serial. Single-source ordering.
- Re-dispatching codex `gpt-5.5` for Round 19.

### Round 19 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: FAIL
- D1 PASS; D2 FAIL (dispatch-map P1 ended at 1i2, omitted 1i3); D3 PASS; D4 PASS; D5 PASS; D6 PASS.
- Fix applied: dispatch map P1 now ends `…→1i1→1i2→1i3`. (Substantive dims D1,D3,D4,D5,D6 all PASS; only a map typo remained.)
- Re-dispatching codex `gpt-5.5` for Round 20 (final).

### Round 20 — codex `gpt-5.5` xhigh (cross-family ✓) — VERDICT: **PASS** ✅
- D1 PASS, D2 PASS, D3 PASS, D4 PASS, D5 PASS, D6 PASS. **Gate satisfied — execution may begin.**
- 19 prior FAIL rounds caught & fixed ~20 substantive issues (non-Clone McpManager move; auth↔config dep; namespace-wide proc-tcp → self-fd ownership; pub(crate) ServerState leak → public AppState projector; re-export-shim migration; headless-cmd layering; transient-connect strace gate; offline-provider proof isolation; complete call-site enumeration; DELETE body shape; CI wiring; --yolo phantom flag). The cross-family gate earned its keep.
