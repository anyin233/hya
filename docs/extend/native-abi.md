# Native Bundle Runtime ABI v2 (`crates/hya-runtime-abi`)

## 1. Introduction

**Lockstep Native ABI v2** (`crates/hya-runtime-abi`) is the unified Rust dynamic-library (`cdylib`) contract between the `hya` host binary and first-party or compiled native bundles (`runtime: { kind: native, abi_version: 2 }`).

Under Hya v2 ([ADR-0039](../adr/0039-hya-bare-and-v2-modular-architecture.md)), core crates (`hya-core`, `hya-engine`, `hya-server`, `hya-store`, `hya-proto`) stay slim and single-agent focused while domain capabilities—hashline editing, LSP/formatters, context compaction, network fetch/search, plan mode, goal/loop drivers, multi-agent supervision, vertical channels, and workflows—live in modular native bundles under `bundles/builtin/`.

Native ABI v2 solves two problems that limited ABI v1:

1. **Unified registration:** A single `cdylib` can register any combination of **tools**, **context rewriters**, **background services**, **lifecycle hooks**, **HTTP/gRPC `BundleApi` handlers**, **event-log `ProjectionReducer`s**, **custom URI `UriSchemaHandler`s**, **`NativeDriver`s**, and **declared hook points** through one entry symbol (`hya_native_register_v2`).
2. **Zero-linkage host mediation:** Native bundles never link `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, or `tonic`. Instead, the host injects an `Arc<dyn HostRuntime>` giving typed, mediated access to session logs, generic KV/claim persistence, turn execution, model calls, the live event bus, catalogs, and sibling extensions—verified before registration by a 32-byte lockstep SHA-256 ABI digest.

---

## 2. Usage

### Declaring a Native ABI v2 bundle (`bundle.yaml`)

Declare `schema_version: 2`, `runtime.kind: native`, `runtime.abi_version: 2`, `crate_dir`, and the capability flags your native library registers:

```yaml
kind: Bundle
schema_version: 2
identity:
  id: acme/metrics-pack
  version: 1.0.0
  publisher: acme
namespace: metrics-pack
runtime:
  kind: native
  abi_version: 2
  crate_dir: native
  capabilities:
    - tools
    - services
    - hooks
    - apis
resources:
  tools:
    - id: record_metric
      path: tools/record_metric.json
```

If your bundle intentionally replaces core `hya-bare` primitives (`read`, `write`, `edit`, or `grep`), declare `overrides: hya/bare` on the bundle and/or individual tool entries. **Reserved kernel tools (`todo` and `rewrite`) can never be overridden, shadowed, or aliased** by any bundle.

### Zero-linkage red line & crate layout

Every native bundle crate MUST respect the **zero-linkage red line** (enforced in CI and crate boundaries):

- **Allowed host/contract crates:** `hya-runtime-abi`, `hya-proto`, `hya-tool`, `hya-tool-error`, `hya-context`, `hya-bundle`, `hya-version` (and `hya-workflow` for workflow authoring/compilation).
- **Forbidden host-internal crates:** `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`, `tonic`.

> **Tip (rlib core + thin `cdylib` wrapper):** If a workspace test or another crate needs to import your bundle's Rust types directly alongside other native bundles, put the domain logic in a pure library crate (`crate-type = ["rlib"]`) and keep `export_native_bundle_v2!` inside a thin `cdylib`-only wrapper crate so two linked crates never export colliding `#[no_mangle]` ABI v2 C symbols.

### Worked example (`native/Cargo.toml` & `native/src/lib.rs`)

```toml
# native/Cargo.toml
[package]
name = "acme-metrics-native"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
async-trait = "0.1"
hya-bundle = { path = "../../../crates/hya-bundle" }
hya-proto = { path = "../../../crates/hya-proto" }
hya-runtime-abi = { path = "../../../crates/hya-runtime-abi" }
hya-tool = { path = "../../../crates/hya-tool" }
serde_json = "1"
```

```rust,ignore
// native/src/lib.rs
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_proto::{ToolName, ToolSchema};
use hya_runtime_abi::{
    HostError, HostRuntime, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest, NativePluginContext, NativeRegistrations, NativeService, ServiceHealth,
};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};

struct RecordMetricTool {
    bundle_id: String,
    runtime: Arc<dyn HostRuntime>,
}

#[async_trait]
impl Tool for RecordMetricTool {
    fn name(&self) -> &str {
        "record_metric"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("record_metric"),
            description: "Persist a session metric via HostBundleStorage and emit an Event::Extension.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "key": { "type": "string" },
                    "value": { "type": "number" }
                },
                "required": ["key", "value"]
            }),
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let key = input["key"]
            .as_str()
            .ok_or_else(|| ToolError::Input("missing string `key`".into()))?;
        let scope = ctx
            .session
            .map(|id| format!("session:{id}"))
            .unwrap_or_else(|| "global".to_string());

        // Write scoped KV state through HostBundleStorage (no sqlx/hya-store linkage!)
        let entry = self
            .runtime
            .bundle_storage()
            .put_state(&self.bundle_id, &scope, key, input["value"].clone(), None)
            .await
            .map_err(ToolError::from)?;

        // Append a durable Event::Extension if running inside a session
        if let Some(session) = ctx.session {
            self.runtime
                .session_store()
                .append_extension_event(
                    session,
                    &self.bundle_id,
                    "metric_recorded",
                    json!({ "key": key, "value": entry.value }),
                    None,
                )
                .await
                .map_err(ToolError::from)?;
        }

        Ok(json!({ "key": entry.key, "version": entry.version }))
    }
}

struct MetricsApi {
    bundle_id: String,
    runtime: Arc<dyn HostRuntime>,
    endpoints: Vec<NativeApiEndpoint>,
}

#[async_trait]
impl NativeBundleApiHandler for MetricsApi {
    fn id(&self) -> &str {
        "metrics-api"
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let scope = request
            .session
            .map(|id| format!("session:{id}"))
            .unwrap_or_else(|| "global".to_string());
        let rows = self
            .runtime
            .bundle_storage()
            .list_state(&self.bundle_id, &scope, None)
            .await?;
        Ok(NativeBundleApiReply {
            status: 200,
            body: json!({ "metrics": rows }),
        })
    }
}

struct MetricsService;

#[async_trait]
impl NativeService for MetricsService {
    fn id(&self) -> &str {
        "metrics-service"
    }

    async fn start(&self, _runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        ServiceHealth::healthy("ready")
    }
}

fn register(ctx: &NativePluginContext, out: &mut NativeRegistrations) {
    let runtime = Arc::clone(ctx.runtime());
    let bundle_id = ctx.bundle_id().to_string();

    out.tools.push(Arc::new(RecordMetricTool {
        bundle_id: bundle_id.clone(),
        runtime: Arc::clone(&runtime),
    }));
    out.services.push(Arc::new(MetricsService));
    out.api_handlers.push(Arc::new(MetricsApi {
        bundle_id,
        runtime,
        endpoints: vec![NativeApiEndpoint {
            id: "list-metrics".into(),
            method: ApiMethod::Get,
            scope: ApiScope::Session,
            path: "/metrics".into(),
            description: "List metrics stored for this session".into(),
        }],
    }));
}

// Export all four lockstep ABI v2 C symbols:
hya_runtime_abi::export_native_bundle_v2!(|ctx, out| {
    register(ctx, out);
});
```

### Building, inspecting symbols & packaging

```sh
# 1. Build the cdylib
cargo build --manifest-path native/Cargo.toml --release

# 2. Verify all four ABI v2 C symbols are exported (Linux example)
nm -D --defined-only target/release/libacme_metrics_native.so | grep hya_native_

# 3. Validate and pack into a deterministic .hyabundle archive
hya bundle verify --strict .
hya bundle pack . acme-metrics-pack.hyabundle
```

### Host loading (`crates/hya-tool-loader`)

The host loads native ABI v2 bundles via `hya-tool-loader` (`load_v2` / `NativeBundleLoader`) in four strict steps:

1. `dlopen(RTLD_NOW | RTLD_LOCAL)` the verified dynamic library.
2. Call `hya_native_abi_v2` and compare its 32-byte digest against `hya_runtime_abi::abi_digest_v2()` (mismatches abort immediately with `AbiError::DigestMismatch` before any Rust object crosses FFI).
3. Call `hya_native_share_deadline_v2` to adopt the host's process-lifetime run-deadline cell and bind `hya_native_with_runtime_v2` for Tokio TLS bridging across the `cdylib` boundary.
4. Construct `NativePluginContext::new(bundle_id, Arc<dyn HostRuntime>)` and call `hya_native_register_v2`, returning a `NativeRegistrations` container wrapped in runtime-bridged adapters.

---

## 3. Interface Definitions

### 3.1 Exported ABI v2 C Symbols & Export Macro

`ABI_VERSION_V2: u32 = 2`. Generated automatically by `hya_runtime_abi::export_native_bundle_v2!`:

- `export_native_bundle_v2!(|ctx, out| { ... });`
- `export_native_bundle_v2!(|out| { ... });`

The block may return `()` or `Result<(), E>` (where `E: Display`, via `IntoRegistrationResult`).

| Constant / Symbol | C Function Signature (`extern "C"`) | Responsibility |
| --- | --- | --- |
| `SYMBOL_ABI_V2` (`hya_native_abi_v2`) | `unsafe extern "C" fn(out: *mut u8)` (`AbiDigestFn`) | Writes the 32-byte lockstep SHA-256 digest (`abi_digest_v2()`). Verified before any other symbol is called. |
| `SYMBOL_WITH_RUNTIME_V2` (`hya_native_with_runtime_v2`) | `unsafe extern "C" fn(handle: *const c_void, callback: unsafe extern "C" fn(*mut c_void), state: *mut c_void)` (`WithRuntimeFn`) | Enters the `cdylib`'s Tokio runtime TLS handle while synchronously polling one future. |
| `SYMBOL_SHARE_DEADLINE_V2` (`hya_native_share_deadline_v2`) | `unsafe extern "C" fn(cell: *const c_void)` (`ShareDeadlineFn`) | Adopts the host's process-lifetime deadline cell (`deadline_cell_v2()`) inside the bundle's `hya-tool` copy. |
| `SYMBOL_REGISTER_V2` (`hya_native_register_v2`) | `unsafe extern "C" fn(ctx: *const NativePluginContext, out: *mut NativeRegistrations, error_out: *mut Option<String>) -> i32` (`RegisterV2Fn`) | Populates `NativeRegistrations`. Returns `0` on success, `-1` on null pointer, or `1` with `error_out` populated on registration failure. |

**Lockstep digest coverage (`abi_digest_v2()`):** Covers the domain separator (`"hya.runtime.native-bundle-abi/v2"`), `ABI_VERSION_V2`, `hya_version::VERSION`, `hya_version::BACKEND_VERSION`, `hya_version::RUSTC`, `std::env::consts::{OS, ARCH}`, `hya_tool::native_bundle::abi_digest_v1()`, `size_of`/`align_of` of every ABI-crossing struct and trait object (`NativePluginContext`, `NativeRegistrations`, `HostError`, `AbiError`, `Arc<dyn HostRuntime>`, `Arc<dyn Tool>`, `Arc<dyn Rewriter>`, `Arc<dyn NativeService>`, `Arc<dyn NativeHook>`, `Arc<dyn NativeBundleApiHandler>`, `Arc<dyn ProjectionReducer>`, `Arc<dyn UriSchemaHandler>`, `Arc<dyn NativeDriver>`, `ToolCtx`, `ToolError`, `tokio::runtime::Handle`, `Option<String>`), plus raw source bytes of every `hya-runtime-abi`, `hya-tool-error`, and `hya-context` ABI file.

### 3.2 `NativePluginContext` & `NativeRegistrations`

```rust,ignore
pub struct NativePluginContext {
    // Methods:
    pub fn new(bundle_id: impl Into<String>, host_runtime: Arc<dyn HostRuntime>) -> Self;
    pub fn bundle_id(&self) -> &str;
    pub fn runtime(&self) -> &Arc<dyn HostRuntime>;
}

#[derive(Default)]
pub struct NativeRegistrations {
    pub tools: Vec<Arc<dyn Tool>>,
    pub rewriters: Vec<Arc<dyn Rewriter>>,
    pub services: Vec<Arc<dyn NativeService>>,
    pub hooks: Vec<Arc<dyn NativeHook>>,
    pub api_handlers: Vec<Arc<dyn NativeBundleApiHandler>>,
    pub projection_reducers: Vec<Arc<dyn ProjectionReducer>>,
    pub uri_schema_handlers: Vec<Arc<dyn UriSchemaHandler>>,
    /// Hook points this bundle declares (local names; namespaced `<bundle-id>:<name>`).
    pub hook_points: Vec<String>,
    /// Drivers that own a session run (namespaced `<bundle-id>:<name>`).
    pub drivers: Vec<Arc<dyn NativeDriver>>,
}
```

### 3.3 `HostRuntime` & Its Seven Sub-Traits

`HostRuntime` is passed as `Arc<dyn HostRuntime>` and exposes seven capability plane accessors plus optional driver lookups:

```rust,ignore
pub trait HostRuntime: Send + Sync {
    fn session_store(&self) -> &dyn HostSessionStore;
    fn bundle_storage(&self) -> &dyn HostBundleStorage;
    fn turn_runner(&self) -> &dyn HostTurnRunner;
    fn model_caller(&self) -> &dyn HostModelCaller;
    fn event_bus(&self) -> &dyn HostEventBus;
    fn catalog_reader(&self) -> &dyn HostCatalogReader;
    fn extension_registry(&self) -> &dyn HostExtensionRegistry;
}
```

| Sub-Trait | Methods & Purpose | Key DTOs |
| --- | --- | --- |
| **`HostSessionStore`** | `session_exists`, `read_events(session, after_seq, limit)`, `append_events(session, claim, events)`, `append_extension_event(session, bundle, kind, payload, claim)`, `read_projection(session)`, `read_extension_state(session, bundle)`, `session_metadata(session)`, `list_sessions(filter)`, `runtime_owner_id()`, `find_sessions_with_extension(bundle, kind)` | `SessionSummaryMeta`, `SessionListFilter`, `Envelope`, `Event`, `Projection` |
| **`HostBundleStorage`** | Scoped KV (`bundle_state` table): `get_state`, `put_state(bundle, scope, key, value, expected_version)` (optimistic CAS), `delete_state`, `list_state(bundle, scope, prefix)`. Fenced leases (`bundle_claim` table): `acquire_claim`, `renew_claim`, `release_claim`, `read_claim`, `list_claims`. | `BundleStateEntry`, `BundleClaimRequest`, `BundleClaimLease` |
| **`HostTurnRunner`** | Session trees: `spawn_session`, `list_child_sessions`, `archive_session_tree`. Core turns: `run_turn(request, cancel)`, `spawn_session_and_run`, `steer_turn`, `cancel_turn`, `turn_status`. Managed resident members & channels: `preflight_member_capacity`, `begin_root_activity`, `spawn_managed_member`, `activate_managed_member`, `end_managed_member`, `session_channel_policy`, `mail_mark`, `notify_mail_consumed`, `is_member_stopped_by_user`, `is_mail_steer_suppressed`, `active_actor_claim`, `stage_mail_steer_ack`, `revive_archived_member_with_mail`, `archive_subagent`, `steer_subagent`. | `TurnStartRequest`, `TurnSpawnRequest`, `TurnOutcome`, `TurnStatus`, `ManagedMemberHandle`, `ManagedMemberSpawnRequest`, `ManagedMemberActivateRequest`, `ManagedMemberActivationOutcome`, `MemberMailRevivalRequest`, `StagedMailAck`, `TurnRouteOutcome`, `TurnRouteFailureClass` |
| **`HostModelCaller`** | `complete(request, cancel)`, `stream(request, cancel)`, `compact_window(model, messages)`, `model_window(model)`, `resolve_model_route(entries)` | `ModelCallRequest`, `ModelCallResponse`, `ModelToolCall`, `ModelStreamChunk`, `ModelWindowInfo`, `ModelRouteEntry`, `ResolvedModelRoute`, `ResolvedModelRouteCandidate` |
| **`HostEventBus`** | `publish(envelope)`, `subscribe() -> broadcast::Receiver<Envelope>`, `publish_extension(session, bundle, kind, payload)` | `Envelope` |
| **`HostCatalogReader`** | `list_agents(session)`, `get_agent(name, session)`, `list_skills(session)`, `list_bundles()`, `get_bundle(id)`, `bundle_config(bundle_id)`, `global_config()`, `resolve_scope_catalog(session, directory)`, `authorize_workflow_agent(session, caller, agent)` | `BundleCatalogSummary`, `ScopeCatalogSnapshot`, `AgentDef`, `PreparedAgent`, `SkillCatalogEntry`, `Bundle` |
| **`HostExtensionRegistry`** | `list_extensions(kind)`, `has_bundle(bundle_id)`, `invoke_bundle_api(call)`, `invoke_tool(session, tool_name, input, cancel)`, and bundle-declared hook dispatch: `has_hook(session, point)` and `invoke_hook(session, point, input) -> Option<Value>` (first answer wins; an undeclared point is `InvalidInput`) | `ExtensionKind`, `ExtensionEntry`, `BundleApiInvocation`, `BundleApiResult` |

### Generic bundle host primitives

Transient state lets a native bundle share per-session signals with host callbacks.
Use the bundle ID from `NativePluginContext` and the session ID as the scope.
Values live in the host process and disappear on restart.

| Interface | Contract | Current consumer |
| --- | --- | --- |
| `bundle_storage().get_transient_state(bundle: &str, session: SessionId, key: &str)` | Returns `Result<Option<Value>, HostError>`. | The unread-mail steer hook in `hya/agent-channels` reads `steer_suppressed` through its compatibility method. |
| `bundle_storage().set_transient_state(bundle: &str, session: SessionId, key: &str, value: Option<Value>)` | Returns `Result<(), HostError>`. Pass `None` to remove the key. | The tool-result path sets and clears `steer_suppressed` for `hya/agent-channels` around after-hooks. |
| `bundle_storage().subscribe_transient_state(bundle: &str, session: SessionId, key: &str)` | Returns `Result<watch::Receiver<Option<Value>>, HostError>`. The receiver exposes the current value and coalesces updates. Equal writes do not signal a change. | The state regression test observes the suppression key. The current unread-mail hook reads it synchronously. |

For example, subscribe to a bundle's `"ready"` key, copy the value from
`borrow_and_update()`, and await `changed()` for a later value. Release the
borrow before waiting or writing to the key.

### Session trees

A bundle can bind an agent to a child session with
`TurnSpawnRequest { parent: Some(lead), agent, .. }`. For example, the loop
driver starts each worker iteration with `spawn_session_and_run` and passes
the lead session as its parent.

| Interface | Contract | Current consumer |
| --- | --- | --- |
| `turn_runner().spawn_session(request: TurnSpawnRequest)` and `spawn_session_and_run(request, cancel)` | The first returns `Result<SessionId, HostError>`. The second waits for a `Result<TurnOutcome, HostError>`. Both record the request's `parent: Option<SessionId>` and `agent: AgentName`. | `hya/loop` creates and runs an iteration worker under its lead. |
| `turn_runner().list_child_sessions(parent: SessionId)` | Returns `Result<Vec<SessionId>, HostError>` with direct children in creation order, including archived children. A parent with no rows has an empty list. | Subtree archival uses the same parent-linked traversal as child listing. |
| `turn_runner().archive_session_tree(root: SessionId)` | Returns `Result<SessionTreeArchive, HostError>` with `cancelled_turn: bool` for the requested session and `descendants: Vec<SessionId>` for sessions newly archived below it, descendants first. Ordinary sessions record `SessionArchived` and let active turns finish. Registered members use the supervisor's archival path, including cancellation and handoff, and retain the supervisor's repeated-archive errors. | The `hya/multi-agent` client archive action calls the old ABI adapter, which uses this operation and converts session IDs to handles. |

### Turn admission

Register a native hook that lists `HookPoint::TurnAdmit` under the name
`turn.admit`. Return `Steer { message: "Use the cached result".into() }` to
append a system message before the first model request. The engine runs the
process hooks followed by the selected agent's hooks while the turn holds
its lease. A refusal discards all accumulated steer messages. The engine
cancels a deferred wait when the turn is cancelled. An admission hook should
signal readiness without starting another turn on the current session.

| Interface | Contract | Current consumer |
| --- | --- | --- |
| `NativeHook::turn_admit(input: NativeTurnAdmitInput)` | Input fields are `session: SessionId`, `root: SessionId`, and `agent: Option<AgentName>`. Returns `Result<NativeTurnAdmitOutcome, HostError>`. Outcomes are `Admit`, `Defer { until: oneshot::Receiver<()> }`, `Refuse { reason: String }`, or `Steer { message: String }`. A dropped readiness sender or a hook error fails admission. | The engine's external-turn guard refuses direct turns on registered multi-agent members. Its existing error and mail guidance remain in effect. |

The hook is available to native bundles. The JSON-RPC plugin hook list remains
defined by the plugin wire contract.

### Legacy host methods

The CHR-47 removals still expose resident capacity, root activity, managed
spawn/activation/ending, and actor claims for older callers. Those
operations carry supervisor scheduling or fencing rules beyond session
creation and turn execution. Channel policies still come from the bound
agent. Mail marks, consumption, and user-stop checks read or update the
supervisor synchronously. Mail revival and steering still deliver through
the parent DM. The unread-mail hook stages its compatibility acknowledgment
at `tool.execute.after` for the tool-result transaction. Root deletion still
waits for member teardown before deleting the session tree.

### 3.4 Native Extension Traits

| Trait | Required Methods | Notes & Contracts |
| --- | --- | --- |
| **`Tool`** (`hya_tool::Tool`) | `name`, `schema`, `execute(&self, ctx: &ToolCtx, input: Value)` | Model-callable tool; uses slim v2 `ToolCtx` + `ExtensionPlanes`. |
| **`Rewriter`** (`hya_context::rewrite::Rewriter`) | Context reduction pass invoked before Kernel `Rewrite` at 80% window occupancy | Used by `hya/compaction` (`shake` + `remote`). |
| **`NativeService`** | `id`, `start(runtime)`, `stop()`, `health() -> ServiceHealth`; optional `is_session_busy`, `cancel_session`, `wait_session_idle` | Host starts/stops managed background services with the runtime lifecycle. |
| **`NativeHook`** | `id`, `hook_points() -> &[HookPoint]` plus default no-op methods for all 17 `HookPoint` variants; `declared_points()` + `invoke_declared(point, input)` for bundle-declared points; `is_healthy()`, `permission_identity()`, and a synchronous `on_event(envelope)` | Covers `tool.execute.{before,after}`, `tool.visibility`, `compaction.{before,after}`, `session.{start,end}`, `agent.spawn`, `command.execute.before`, `message.user.before`, `turn.admit`, `text.complete`, `chat.params`, `model.fallback`, `permission.{ask,approve}`, `event`. This is the one hook shape: the engine's `HookChain` holds native bundle hooks, the JS plugin host, and core built-ins as `NativeHook` entries, and dispatches each point through one ordered chain. A hook is called only for the points it lists. |
| **`NativeBundleApiHandler`** | `id`, `endpoints() -> &[NativeApiEndpoint]`, `handle(NativeBundleApiRequest) -> Result<NativeBundleApiReply, HostError>` | Mounted uniformly on `hya.v2.BundleApi` (`/v2/bundles/{bundle}/api/{path}` and `/v2/sessions/{session}/bundles/{bundle}/{path}`). |
| **`ProjectionReducer`** | `bundle_id`, `initial_state() -> Value`, `apply(&self, state: &mut Value, event: &ExtensionEventContext)` | Deterministically folds `Event::Extension` records into `Projection::extensions[bundle_id]`. |
| **`UriSchemaHandler`** | `scheme`, `writable()`, `read(UriResolveCtx, uri) -> Result<UriContent, HostError>`, `write(UriResolveCtx, uri, content)` | Plugged into `HandleRouter` via `adapt_uri_schema_handler` (`UriSchemaHandleAdapter`). Internal schemes (`artifact`, `skill`, `local`) are protected. |
| **`NativeDriver`** | `name`, `run(runtime, DriverRequest, cancel) -> Result<DriverReport, HostError>` | A run that owns a session until it returns. The host registers it as `<bundle-id>:<name>` and knows nothing else about it. `DriverRequest` carries `session`, `agent`, `model`, `workdir`, and JSON `args` (for a CLI command, its parsed arguments); `DriverReport` carries `stdout`, `stderr`, and `exit_code`, which the CLI prints and exits with. `DriverRunOutcome` is a shared terminal-outcome type for iterating drivers. Consumers: `hya/goal:goal` (`hya -p`) and `hya/loop:loop` (`hya loop`). |

### 3.5 `ErasedPlane` & `ExtensionPlanes` (`crates/hya-tool`)

Core `hya_tool::ToolCtx` contains only single-agent v2 core planes (`workdir`, `roots`, `cancel`, `session`, `operation`, `permission`, `interaction`, `skills`, `artifacts`, `schemas: UriSchemaPlane`, `nested`) plus `extensions: ExtensionPlanes`:

```rust,ignore
#[derive(Clone, Default)]
pub struct ExtensionPlanes {
    planes: Option<Arc<BTreeMap<String, Arc<ErasedPlane>>>>,
}
```

- **Cross-`cdylib` safety without `TypeId`:** Host binaries and native `cdylib` bundles built in separate Cargo invocations can assign different crate SVH hashes—and therefore different `std::any::TypeId` values—to the exact same Rust type. `ExtensionPlanes` avoids `TypeId` completely:
  1. Entries are keyed by `std::any::type_name::<T>()`.
  2. Each `ErasedPlane` stores `(ptr: *mut (), drop_fn: unsafe fn(*mut ()), size: usize, align: usize)`.
  3. `ErasedPlane::downcast_ref::<T>()` verifies `size == size_of::<T>()` and `align == align_of::<T>()` under the verified lockstep ABI before returning `Option<&T>`.
  4. `Drop` invokes `drop_fn`, reconstructing `Box<T>` inside the allocating side's allocator so memory is freed safely across dynamic-library boundaries.
- **Usage on `ToolCtx`:** Attach with `ctx.with_extension(plane)` (or `extensions.insert(plane)` / `.with(plane)`) and borrow with `ctx.extension::<T>()` (or `extensions.get::<T>()` / `.contains::<T>()`). In `--bare` mode, `ToolCtx::extensions` is empty.

### 3.6 Error Contracts (`HostError` & `AbiError`)

| Enum | Variants |
| --- | --- |
| **`HostError`** (`#[serde(tag = "type", content = "detail", rename_all = "snake_case")]`) | `NotFound(String)`, `Conflict(String)`, `StaleClaim(String)`, `InvalidInput(String)`, `PermissionDenied(String)`, `Unavailable(String)`, `Cancelled`, `Storage(String)`, `Internal(String)`. Converts bidirectionally with `hya_tool_error::ToolError`. |
| **`AbiError`** | `InvalidBundleIdentity(String)`, `DisabledOrMissing { bundle, detail }`, `LoadFailed { bundle, detail }`, `MissingSymbol { bundle, symbol }`, `DigestMismatch { bundle }`, `RegistrationFailed { bundle, detail }`, `RuntimeBridge(String)`. |
