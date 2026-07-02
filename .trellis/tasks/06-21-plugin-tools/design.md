# Design - Child B: Plugin-registered tools

> Parent: [../06-21-plugin-system/design.md](../06-21-plugin-system/design.md).
> Child A: [../06-21-plugin-host-core/design.md](../06-21-plugin-host-core/design.md).
> Requirements: [./prd.md](./prd.md) (B-R1..B-R7, B-AC1..B-AC5).
>
> Scope: Child B consumes the locked parent protocol and Child A host. It owns
> parent R5 only: translating plugin-declared tools into normal yaca `Tool`
> instances during bootstrap. It does not redesign the protocol, hook host,
> provider layer, or runtime loading model.

## Merge decisions (parallel-planning: reuse-first + edge-case-first)

- **[MERGE] Raw declared tool names, NOT `plugin__<id>__<tool>` namespacing.** The
  edge-case planner proposed namespacing to make collisions structurally
  impossible. **Rejected** because Child C (Compat compat) routes Compat
  `tool:` registrations through this same `PluginTool`, and Compat tools are
  **not** namespaced — a plugin's `weather` tool MUST be callable by the model as
  `weather`, not `plugin__compat__weather`. Namespacing would break Compat
  fidelity. Collisions are instead handled by the registry's deterministic
  **first-wins + warn** (builtins register first, so they always win); see §7.
- **[MERGE] `ToolCtx` needs `session` + `call` (the key correctness catch).** The
  locked `tool/call` frame (parent §2.5) carries `session` and `call`, but today's
  `ToolCtx` only has `parent_session`. One planner caught this; the other silently
  used `parent_session` and dropped `call` — a latent protocol violation. The fix
  (§2) is a required supporting change, **coordinated with Child A** (A owns the
  engine tool-loop `ToolCtx` construction it also edits for hooks).
- **[MERGE] New `Action::Plugin`** (both planners converged) over reusing
  `Action::Mcp`, so operators can scope plugin vs. MCP trust independently (§6).
- **[MERGE] Mirror `McpTool`/`McpManager`** exactly (both converged) — the proven
  in-repo precedent.

## 0. Locked Inputs And Boundary

Child B starts after Child A has landed these contracts:

- `PluginHost::connect_all(...)` spawns and initializes configured plugins.
- Each initialized plugin records the `tools` declared by the locked
  `initialize` response in parent design section 2.2.
- `PluginClient::call(method, params, timeout)` behaves like
  [`McpClient::call`](../../../crates/yaca-mcp/src/client.rs#L137): JSON-RPC id
  correlation, per-call timeout, pending-drain on EOF, and a typed error enum.
- The locked tool-call wire shape is parent design section 2.5:
  `{ "method":"tool/call", "params": { "tool", "session", "call", "input" } }`
  with reply `{ "ok": true, "output": ..., "time_ms": ... }`.

Child B must stop and coordinate with Child A if any implementation pressure
requires changing that frame. A local Rust API change, such as adding metadata to
`ToolCtx`, is allowed because it only lets Child B fill the already-locked frame.

The precedent to mirror is `yaca-mcp`:

| Concern | Existing precedent | Child B reuse |
|---|---|---|
| Proxy tool shape | [`McpTool`](../../../crates/yaca-mcp/src/bridge.rs#L12) | `PluginTool` with client, raw tool name, cached schema, timeout |
| Schema gate | [`McpTool::try_new`](../../../crates/yaca-mcp/src/bridge.rs#L20) rejects non-object inputs | same gate for plugin-declared `inputSchema` |
| Permission first | [`McpTool::execute`](../../../crates/yaca-mcp/src/bridge.rs#L59) asserts before IPC | same, with plugin-specific action/resource |
| RPC call mapping | [`client.call(...).map_err(ToolError::Other)`](../../../crates/yaca-mcp/src/bridge.rs#L63) | same mapping for `PluginError` |
| Tool collection | [`McpManager::tools`](../../../crates/yaca-mcp/src/manager.rs#L55) | `PluginHost` exposes declared plugin tools in deterministic load order |
| Bootstrap insertion | [`build_session_engine`](../../../crates/yaca-cli/src/main.rs#L255) registers MCP before `Arc::new(registry)` | plugin tools register in that same window |

The only intentional divergence from MCP is user-visible naming: MCP tools are
namespaced because their servers are a parallel extension mechanism. Plugin tools
preserve their declared names so Compat-style `tool:` registration maps cleanly
and the parent example tool `remember` is advertised as `remember`. Collisions are
handled explicitly in section 6.

## 1. Registry registration (reuse the existing `register`)

`ToolRegistry` is **already** mutable/extensible — it is NOT static. Current code
builds builtins at [`tool.rs:74`](../../../crates/yaca-tool/src/tool.rs#L74),
exposes the public `register(...) -> Result<(), DuplicateName>` at
[`tool.rs:94`](../../../crates/yaca-tool/src/tool.rs#L94) (already used by MCP
bootstrap), and freezes the registry into `Arc<ToolRegistry>` at
[`main.rs:264`](../../../crates/yaca-cli/src/main.rs#L264). Child B reuses
`register` as-is — **no new registry primitive is required.**

Registration is bootstrap-time only:

- No runtime hot-loading.
- No `Arc<Mutex<ToolRegistry>>`.
- No provider-time schema callback into `PluginHost`.
- No mutation after the registry is shared with `SessionEngine`.

The smallest useful edit is a batch helper that preserves `builtins()` and
`register()` exactly. The helper is optional for correctness because callers can
loop over `register`, but it makes plugin/MCP/bootstrap code and tests less
duplicative.

Exact edit in [`crates/yaca-tool/src/tool.rs`](../../../crates/yaca-tool/src/tool.rs#L94):

```diff
 impl ToolRegistry {
@@
     pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
         let name = tool.name().to_string();
         if self.tools.contains_key(&name) {
             return Err(DuplicateName { name });
         }
         self.tools.insert(name, tool);
         Ok(())
     }
+
+    pub fn extend<I>(&mut self, tools: I) -> Vec<DuplicateName>
+    where
+        I: IntoIterator<Item = Arc<dyn Tool>>,
+    {
+        let mut duplicates = Vec::new();
+        for tool in tools {
+            if let Err(error) = self.register(tool) {
+                duplicates.push(error);
+            }
+        }
+        duplicates
+    }

     #[must_use]
     pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
```

No `builtins()` behavior changes. The existing duplicate semantics stay first-wins,
as tested by [`registry_rejects_duplicate_tool_name`](../../../crates/yaca-tool/tests/tool.rs#L69).

## 2. Minimal `ToolCtx` Metadata For The Locked Frame

Parent design section 2.5 requires `session` and `call` in every plugin
`tool/call` request. Current `ToolCtx` has `parent_session` but not the current
session or tool-call id ([`tool.rs:38`](../../../crates/yaca-tool/src/tool.rs#L38)).
`PluginTool::execute` cannot correctly fill the locked frame without those
fields.

Child B adds metadata to `ToolCtx` rather than changing the `Tool` trait:

```diff
-use yaca_proto::{SessionId, ToolName, ToolSchema};
+use yaca_proto::{SessionId, ToolCallId, ToolName, ToolSchema};
@@
 pub struct ToolCtx {
     pub permission: PermissionPlane,
     pub interaction: InteractionPlane,
     pub spawner: SpawnerPlane,
+    pub session: SessionId,
+    pub tool_call: Option<ToolCallId>,
     pub parent_session: Option<SessionId>,
     pub workdir: PathBuf,
     pub cancel: CancellationToken,
 }
```

The engine fills those fields at the current tool loop site
[`engine.rs:340`](../../../crates/yaca-core/src/engine.rs#L340):

```diff
 let ctx = ToolCtx {
     permission: self.permission.for_session(session),
     interaction: self.interaction.for_session(session),
     spawner: self.spawner.for_session(session),
+    session,
+    tool_call: Some(tc.call),
     parent_session: projection.session.parent,
     workdir: agent.workdir.clone(),
     cancel: cancel.clone(),
 };
```

Unit tests that construct `ToolCtx` can use `SessionId::new()` and
`tool_call: None` unless they are testing `PluginTool`. `PluginTool` returns
`ToolError::Other("plugin tool executed without tool call id")` if invoked
without `Some(tool_call)`. That keeps accidental direct invocation explicit while
leaving builtin tools unaffected.

## 3. `PluginTool` Proxy

Child B adds `crates/yaca-plugin/src/tool.rs` and re-exports it from
`crates/yaca-plugin/src/lib.rs`.

The struct mirrors `McpTool`:

```rust
pub struct PluginTool {
    plugin_id: String,
    client: PluginClient,
    tool: String,
    schema: ToolSchema,
    timeout: Duration,
}
```

Constructor shape:

```rust
impl PluginTool {
    pub fn try_new(
        plugin_id: &str,
        info: PluginToolInfo,
        client: PluginClient,
        timeout: Duration,
    ) -> Option<Arc<dyn Tool>> {
        if info.input_schema.get("type").and_then(Value::as_str) != Some("object") {
            return None;
        }
        if info.name.trim().is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            plugin_id: plugin_id.to_string(),
            client,
            tool: info.name.clone(),
            schema: ToolSchema {
                name: ToolName::new(info.name),
                description: info.description,
                input_schema: info.input_schema,
                output_schema: None,
            },
            timeout,
        }))
    }
}
```

`PluginToolInfo` is the Child A type used to store an initialized plugin's
declared tools. If Child A uses a different name, use that type directly; do not
create a parallel declaration type just for Child B.

Execution mirrors [`McpTool::execute`](../../../crates/yaca-mcp/src/bridge.rs#L59)
with the locked plugin method and response:

```rust
#[async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        self.schema.name.as_str()
    }

    fn schema(&self) -> ToolSchema {
        self.schema.clone()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        ctx.permission
            .assert(
                Action::Plugin,
                Resource::Command(plugin_tool_resource(&self.plugin_id, self.name())),
            )
            .await?;

        let call = ctx
            .tool_call
            .ok_or_else(|| ToolError::Other("plugin tool executed without tool call id".to_string()))?;

        let value = self
            .client
            .call(
                "tool/call",
                json!({
                    "tool": self.tool,
                    "session": ctx.session,
                    "call": call,
                    "input": input,
                }),
                self.timeout,
            )
            .await
            .map_err(|error| ToolError::Other(format_plugin_error(&self.plugin_id, self.name(), error)))?;

        let reply: ToolCallReply = serde_json::from_value(value)
            .map_err(|error| ToolError::Other(format!("plugin '{}' tool '{}': invalid response: {error}", self.plugin_id, self.name())))?;
        if !reply.ok {
            return Err(ToolError::Other(format!(
                "plugin '{}' tool '{}' reported failure: {}",
                self.plugin_id,
                self.name(),
                compact_json(&reply.output),
            )));
        }
        Ok(reply.output)
    }
}
```

`ToolCallReply` matches parent section 2.5:

```rust
#[derive(Deserialize)]
struct ToolCallReply {
    ok: bool,
    output: Value,
    #[serde(default)]
    time_ms: Option<u64>,
}
```

`time_ms` is accepted but not used for engine accounting; the engine already
records local elapsed time at [`engine.rs:337`](../../../crates/yaca-core/src/engine.rs#L337)
and emits it in `Event::ToolResult` at [`engine.rs:353`](../../../crates/yaca-core/src/engine.rs#L353).

Timeout and error mapping:

- `timeout` is captured from the plugin config's `timeout_ms`, falling back to the
  same 30s default used by MCP
  ([`DEFAULT_CALL_TIMEOUT`](../../../crates/yaca-mcp/src/client.rs#L15)).
- `PluginClient::call` handles id correlation and timeout like
  [`McpClient::call`](../../../crates/yaca-mcp/src/client.rs#L137).
- Any `PluginError::{Closed, Timeout, Rpc, OversizedLine, Json, Io}` maps to
  `ToolError::Other` with prefix `plugin '<plugin_id>' tool '<tool_name>': ...`.
- Permission failures remain `ToolError::Permission` via `#[from]`, preserving the
  normal engine mapping to `Event::ToolError`.

Explicit failure matrix (every row maps to `ToolError`; the engine then emits
`Event::ToolError` and the turn continues — no hang, no panic, no engine change):

| Source | Maps to | Message (prefix `plugin '<id>' tool '<name>'`) |
|---|---|---|
| `PluginError::Closed` (Dead/Disabled/EOF/drained pending) | `ToolError::Other` | `: connection closed` |
| `PluginError::Timeout` | `ToolError::Other` | `: timed out after <ms>ms` |
| `PluginError::Rpc{code,message}` | `ToolError::Other` | `: rpc error <code>: <message>` |
| `PluginError::OversizedLine` | `ToolError::Other` | `: response exceeded 1 MiB` |
| `PluginError::Json`/`Io` | `ToolError::Other` | `: <original>` |
| reply `{"ok":false,...}` | `ToolError::Other` | ` reported failure: <output json>` |
| malformed reply envelope | `ToolError::Other` | `: invalid response: <parse error>` |
| permission denied | `ToolError::Permission` (via `#[from]`) | (unchanged) |
| `ctx.cancel` fires mid-call | `ToolError::Cancelled` | (existing variant) |

Plugin-state behavior: **Dead/Disabled** → instant `Closed` (no hang);
**slow/unresponsive** → per-tool timeout fires; **killed mid-call** → reader EOF →
`close_pending` → `Closed`. **Respawn caveat (documented v1 limitation):** a
`PluginTool` holds the `PluginClient` captured at bootstrap; if the plugin dies and
the host respawns it, the tool keeps the old (closed) client and fast-fails — hooks
recover, tools do not (follow-up: a respawn-aware client handle). The schema stays
advertised; the model calls it, gets a fast error, adapts.

Cancellation uses the existing `ctx.cancel` if Child A's `PluginClient::call` is
not already cancellation-aware:

```rust
tokio::select! {
    biased;
    _ = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
    result = self.client.call("tool/call", params, self.timeout) => map_result(result),
}
```

## 4. Registration Flow In `YacaRuntime` Bootstrap

Child A's parent design section 6.3 replaces the current split helpers with a
single `bootstrap(...) -> YacaRuntime`. Child B slots into that bootstrap after
plugins connect and before `ToolRegistry` is frozen.

Current MCP registration lives at
[`main.rs:255`](../../../crates/yaca-cli/src/main.rs#L255):

```rust
let mut registry = ToolRegistry::builtins();
let mcp_manager = McpManager::connect_all(mcp).await;
for tool in mcp_manager.tools() {
    if let Err(error) = registry.register(tool) {
        eprintln!("yaca: skipping MCP tool ({error})");
    }
}
let tools = Arc::new(registry);
```

After Child A, the bootstrap order becomes:

```rust
let plugin_host = PluginHost::connect_all(plugin_specs).await;

let mut registry = ToolRegistry::builtins();

let mcp_manager = McpManager::connect_all(mcp).await;
for tool in mcp_manager.tools() {
    if let Err(error) = registry.register(tool) {
        tracing::warn!(%error, "skipping MCP tool");
    }
}

for plugin in plugin_host.plugins_in_load_order() {
    for info in plugin.declared_tools() {
        let Some(tool) = PluginTool::try_new(
            plugin.id(),
            info.clone(),
            plugin.client(),
            plugin.call_timeout(),
        ) else {
            tracing::warn!(plugin = plugin.id(), tool = %info.name, "skipping invalid plugin tool");
            continue;
        };
        if let Err(error) = registry.register(tool) {
            tracing::warn!(plugin = plugin.id(), %error, "skipping duplicate plugin tool");
        }
    }
}

let tools = Arc::new(registry);
let (permission, asks) = PermissionPlane::new(rules);
let permission = permission.with_interceptor(plugin_host.clone());
let engine = SessionEngine::new(store, router, tools, permission, EventBus::default())
    .with_hooks(plugin_host.clone());
```

The exact accessor names can follow Child A's implementation, but the semantics
are fixed:

- Iterate plugins in deterministic load order, not `JoinSet` completion order.
- Wrap each declaration into `PluginTool` during bootstrap.
- Register plugin tools after builtins and MCP tools.
- Freeze once with `Arc::new(registry)`.
- Keep `PluginHost` alive in `YacaRuntime` so its clients outlive the engine.

## 5. Schema Advertisement

No provider changes are needed.

`PluginTool::schema()` returns a normal `ToolSchema`. Once registered, it flows to
the model through the existing engine path:

1. Provider request is built by
   [`request_from_messages`](../../../crates/yaca-core/src/engine.rs#L487).
2. That function sets `tools: tools.schemas()` at
   [`engine.rs:496`](../../../crates/yaca-core/src/engine.rs#L496).
3. `ToolRegistry::schemas()` simply maps every registered tool to its schema at
   [`tool.rs:109`](../../../crates/yaca-tool/src/tool.rs#L109).
4. When the model calls a plugin tool, the existing engine lookup at
   [`engine.rs:338`](../../../crates/yaca-core/src/engine.rs#L338) finds the
   `PluginTool` by the declared name.

This satisfies B-R3 and B-AC1 without touching `yaca-provider` encoders,
decoders, or router behavior.

## 6. Permission Mapping

Use one generic plugin-tool action and the existing command resource:

| Concept | Choice | Rationale |
|---|---|---|
| Action | `Action::Plugin` | Distinguishes plugin tools from MCP tools without per-plugin action explosion |
| Resource | `Resource::Command("<plugin_id>:<tool_name>")` | Reuses glob matching and CLI/TUI permission infrastructure |
| Assert site | first statement in `PluginTool::execute` | Denial prevents any plugin side effect |

Exact enum edit in [`permission.rs`](../../../crates/yaca-tool/src/permission.rs#L10):

```diff
 pub enum Action {
     Read,
     Edit,
     Glob,
     Grep,
     Bash,
     Task,
     Mcp,
+    Plugin,
     ExternalDirectory,
 }
```

`#[serde(rename_all = "lowercase")]` means the serialized action is `"plugin"`,
matching current action naming style.

Resource helper:

```rust
#[must_use]
pub fn plugin_tool_resource(plugin_id: &str, tool: &str) -> String {
    format!("{plugin_id}:{tool}")
}
```

CLI policy mirrors MCP in [`crates/yaca-cli/src/permission.rs`](../../../crates/yaca-cli/src/permission.rs#L69):

- `Yolo` allows everything through the existing catch-all.
- `ReadOnly` rejects `Action::Plugin`, like it rejects MCP today.
- `Scoped` allows `Action::Plugin` once, matching the current external-tool stance
  for `Action::Mcp` at [`permission.rs:74`](../../../crates/yaca-cli/src/permission.rs#L74).

`permission.ask` interaction is inherited from Child A. If the permission plane is
in `Ask`, Child A's `PermissionInterceptor` may answer first. `None`/defer falls
through to the existing user ask. Child B does not add a second permission path.

## 7. Name Collisions And Precedence

Plugin schemas preserve declared names. Therefore collisions are possible and must
be deterministic.

Rule: the registry entry that is already present wins; the later plugin tool is
skipped with a warning.

Registration order defines precedence:

1. Builtins first. A plugin declaring `read`, `write`, `shell`, or any other
   builtin is skipped. Builtin behavior is unchanged.
2. MCP tools next, if configured. A plugin declaring an already registered MCP
   name is skipped. This is not a Child B requirement, but it falls out of the
   same registry rule.
3. Plugin tools last, in deterministic plugin load order. If two plugins declare
   `remember`, the first loaded plugin owns `remember`; the second is skipped.
4. If a single plugin declares the same tool twice, its first declaration wins and
   later duplicates are skipped.

Warnings use `tracing::warn!` and include at least `plugin`, `tool`, and the
`DuplicateName` error. Do not fail bootstrap on duplicate plugin tools; other
tools from that plugin still register and the plugin's hooks still run.

This rule is simpler than Compat's override behavior and protects B-AC5: no
plugin can silently shadow a builtin in v1. A future explicit override feature can
be designed as a separate task if users need it.

## 8. Acceptance Criteria Mapping

| Acceptance criterion | Design mechanism |
|---|---|
| B-AC1: schema advertised, model calls tool, IPC round-trip, normal `ToolResult` | sections 3, 4, 5; `PluginTool` is a normal registered `Tool` |
| B-AC2: permission denial blocks side effect | section 6; assert happens before `tool/call` |
| B-AC3: hung/crashed plugin yields `ToolError` and turn continues | section 3; `PluginClient::call` timeout/closed errors map to `ToolError::Other`, engine already emits `Event::ToolError` |
| B-AC4: collision behavior documented and tested | section 7; existing `DuplicateName` first-wins registry rule |
| B-AC5: no-plugin builtins unchanged | sections 1 and 4; `builtins()` path unchanged and plugin loop runs only after plugin host connect |

## 9. Explicit Deferrals

Child B does not build:

- Runtime hot registration or deregistration.
- Provider-layer tool schema callbacks.
- Protocol changes to `initialize` or `tool/call`.
- Plugin-defined permission action kinds beyond the generic `Action::Plugin`.
- Builtin shadowing or override configuration.
- Tool output streaming.
- Respawn-aware schema replacement after bootstrap. If a plugin dies after its
  tool registered, calls fail with `ToolError`; the schema remains advertised for
  v1, matching the frozen registry model.
