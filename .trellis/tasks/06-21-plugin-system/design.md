# Design — yaca plugin system (shared architecture & protocol)

> Parent/cross-child design. Authoritative for the **IPC protocol**, the
> **crate/dependency layout**, the **hook-dispatch model**, and the **failure
> posture mechanics** that Children A/B/C all share. Child-specific design lives in
> each child's `design.md`. Requirements/decisions: [parent prd.md](./prd.md).

Merged from two diverse parallel planners (architecture-first `oracle` +
implementation-first `ultrabrain`) per the parallel-planning pipeline. Merge
decisions are recorded inline as **[MERGE]** notes.

## 0. Pivotal precedent: `yaca-mcp`

yaca **already** ships `crates/yaca-mcp` — a stdio JSON-RPC subprocess integration
that is a near-exact precedent for the plugin host:

- `McpClient` ([client.rs](../../../crates/yaca-mcp/src/client.rs)): `jsonrpc:"2.0"`
  framing, `MAX_LINE_BYTES = 1MiB`, `Pending = Arc<Mutex<HashMap<u64, oneshot::Sender>>>`
  id-correlated demux, `INITIALIZE_TIMEOUT = 5s`, `DEFAULT_CALL_TIMEOUT = 30s`,
  `McpError::{Closed, Timeout, OversizedLine, Rpc, …}`, `close_pending` on EOF, a
  `demuxes_responses_by_id` duplex test.
- `McpClient::spawn(&command, env) -> (client, ChildGuard)` (drop-kills the child).
- `McpManager::connect_all(configs)` ([manager.rs](../../../crates/yaca-mcp/src/manager.rs)):
  `tokio::task::JoinSet`, **fault-isolated** (`one_failed_server_does_not_abort_others`),
  per server `spawn → initialize → tools/list → wrap`.
- `McpServerConfig { command: Vec<String>, env, enabled, timeout_ms }` — the exact
  config shape we want for plugins.
- `McpTool: impl yaca_tool::Tool` ([bridge.rs](../../../crates/yaca-mcp/src/bridge.rs))
  — the precedent for Child B's plugin-tool proxy.
- Wired once in [`yaca-cli/src/main.rs`](../../../crates/yaca-cli/src/main.rs) — the
  bootstrap integration point.

**[MERGE] The plugin host is a generalization of `yaca-mcp`'s client+manager**, and
the protocol adopts **JSON-RPC 2.0** (the house style) rather than either planner's
bespoke envelope. This de-risks transport, reuses proven patterns, keeps the
codebase consistent, and makes the Compat Bun adapter (Child C) trivial (JSON-RPC
2.0 is ubiquitous in JS). Follow-up (not now): extract a shared `jsonrpc-stdio`
helper crate that both `yaca-mcp` and `yaca-plugin` use; for v1 we keep a parallel
minimal impl in `yaca-plugin` to avoid refactoring a working crate.

> Note: MCP is yaca's **parallel** extension mechanism (external tool servers). The
> plugin system is broader (lifecycle interception + tools + Compat compat) but
> deliberately shares MCP's transport DNA.

## 1. Crate & dependency layout (the DAG)

New crate **`yaca-plugin`** (host/manager + protocol + bridges) and a new bin
crate **`yaca-plugin-example`** (QA fixture). `members = ["crates/*"]` auto-includes
both.

```
crates/yaca-plugin/src/
  lib.rs            public re-exports
  protocol.rs       JSON-RPC 2.0 frames (Request/Response/Error) + notifications
  messages.rs       typed hook/tool payloads, HookName, HookPosture, constants
  codec.rs          line-framed async read/write + MAX_LINE_BYTES guard
  client.rs         PluginClient (per-plugin RPC handle; modeled on McpClient)
  child.rs          ChildProcess::spawn + ChildGuard (reuse yaca-mcp pattern)
  manifest.rs       plugin.toml parser + dir-scan
  config.rs         config.yaml `plugins:` schema
  host.rs           PluginHost: plugins + supervisor + event fan-out + chains
  dispatcher.rs     impl yaca_core::HookDispatcher for PluginHost
  permission_bridge.rs  impl yaca_tool::PermissionInterceptor
  goal_bridge.rs    HookedGoalEvaluator / HookedLoopVerifier / HookedLoopPlanner
```

**[MERGE] Trait-in-the-consumer inversion (chosen over "trait in yaca-plugin").**
The traits the existing code calls are defined where they are *used*, and
`yaca-plugin` (the implementor) depends on those crates. This keeps the DAG acyclic
and means **`yaca-core` does NOT depend on `yaca-plugin`**:

- `HookDispatcher` trait + native payload types → **`yaca-core`** (new
  `hooks.rs`). The engine holds `Option<Arc<dyn HookDispatcher>>`.
- `PermissionInterceptor` trait → **`yaca-tool`** (in `permission.rs`). The plane
  holds `Option<Arc<dyn PermissionInterceptor>>`.
- `yaca-plugin` → `yaca-core`, `yaca-tool`, `yaca-provider`, `yaca-proto`, tokio,
  serde/serde_json, thiserror, async-trait, toml, tracing (all already in the
  workspace; no new root deps).
- `yaca-cli` → `yaca-plugin` (loads/spawns/wires).

Edges: `yaca-cli → yaca-plugin → {yaca-core, yaca-tool, yaca-provider, yaca-proto}`;
`yaca-core → {yaca-tool, yaca-provider, yaca-proto}`. No cycle.

## 2. The IPC protocol (JSON-RPC 2.0 over stdio JSONL)

One JSON object per line, UTF-8, `\n`-terminated, `MAX_LINE_BYTES = 1MiB`
(reuse the `yaca-mcp` constant + `OversizedLine` handling). Frames are JSON-RPC
2.0:

```jsonc
// Request (needs a reply)        — id is u64, monotonic per plugin
{ "jsonrpc":"2.0", "id":17, "method":"hook/tool.execute.before", "params": { … } }
// Response (success)
{ "jsonrpc":"2.0", "id":17, "result": { … } }
// Response (error)               — JSON-RPC error codes
{ "jsonrpc":"2.0", "id":17, "error": { "code":-32601, "message":"…" } }
// Notification (no id, no reply) — async observation, either direction
{ "jsonrpc":"2.0", "method":"event", "params": { … } }
```

Reuse `yaca-mcp` `protocol.rs` shapes (`JsonRpcRequest/Response/Error`). Error
codes: `-32601` method-not-found, `-32602` invalid-params, `-32603` internal,
`1` veto. Id correlation via an `AtomicU64` + `Pending` map (as McpClient).

### 2.1 Method namespace

| Method | Direction | Reply? | Purpose |
|---|---|---|---|
| `initialize` | host→plugin | yes | handshake + registration |
| `hook/<name>` | host→plugin | yes | blocking interception hook |
| `event` | host→plugin | no (notif) | async observation |
| `tool/call` | host→plugin | yes | invoke a plugin-registered tool (Child B) |
| `shutdown` | host→plugin | yes | graceful stop |

### 2.2 Handshake & version negotiation

```jsonc
// host→plugin (first frame)
{ "jsonrpc":"2.0","id":1,"method":"initialize",
  "params": { "protocol_version":1, "host":{"name":"yaca","version":"0.x"} } }
// plugin→host (must arrive within INITIALIZE_TIMEOUT=5s)
{ "jsonrpc":"2.0","id":1,"result": {
    "protocol_version":1,
    "plugin":{ "id":"example","version":"0.1.0","kind":"rust" },   // kind: rust|compat|other
    "hooks":[ {"name":"tool.execute.before","posture":"safe"},
              {"name":"chat.params","posture":"open"},
              {"name":"event","posture":"open"} ],
    "tools":[ {"name":"remember","description":"…","inputSchema":{…}} ]  // consumed by Child B
} }
```

Rules: host calls **only declared hooks** (cheap dispatch). `protocol_version`
mismatch ⇒ `warn!` + kill the child (no degrade-and-pray). `tools` is parsed and
stored in Child A but only wired into the registry by Child B. Declared `posture`
overrides the per-hook default; the host forces the **safer** of {declared, policy}
on mismatch.

**[D6] Tool-schema wire key is `inputSchema` (camelCase) EVERYWHERE** — in
`initialize.tools[]` above and in any Child C adapter output. This mirrors the
existing `yaca-mcp` `ToolInfo` (`#[serde(rename_all = "camelCase")]`, whose test
asserts the wire contains `inputSchema`) and Compat's own `inputSchema`. Rust
structs may name the field `input_schema` internally but MUST serde-rename to
`inputSchema` on the wire. Children B and C MUST NOT emit snake_case `input_schema`
on the wire; a protocol-codec round-trip test pins this.

### 2.3 Hook request/reply (interception)

`input` = read-only context; `output`/outcome = the mutable thing the engine
applies (mirrors Compat's `(input, output)` in-place mutation). Outcome is a
tagged enum so mutation, pass-through, veto, and defer are explicit:

```jsonc
// host→plugin
{ "jsonrpc":"2.0","id":42,"method":"hook/tool.execute.before",
  "params": { "session":"…","message":"…","call":"…","tool":"shell",
              "input": { "command":"ls" } } }
// plugin→host — mutate
{ "jsonrpc":"2.0","id":42,"result": { "outcome":"continue","input":{"command":"ls --color=never"} } }
// plugin→host — veto (guard hooks only)
{ "jsonrpc":"2.0","id":42,"result": { "outcome":"veto","reason":"blocked by policy" } }
```

Per-hook payloads (typed structs in `messages.rs`; native equivalents in
`yaca-core::hooks`). `[MERGE]` chose the impl-first planner's explicit
`outcome`-tagged results (+ a `defer` for answer-hooks):

| `hook/<name>` | input | outcome → effect | veto | posture default |
|---|---|---|---|---|
| `message.user.before` | `{session,text}` | `continue{text}` | no | open |
| `chat.params` | `{session,message,request}` | `continue{request}` (model/system/temperature/max_output_tokens/reasoning/messages/tools) | no | open |
| `tool.execute.before` | `{session,message,call,tool,input}` | `continue{input}` \| `veto{reason}` | **yes** | **safe** |
| `tool.execute.after` | `{…,input,result}` | `continue{result}` (Ok/Err swappable, see §2.6) | no | open |
| `permission.ask` | `{session?,action,resource}` | `allow_once`\|`allow_always`\|`reject{feedback?}`\|`defer` | n/a | **safe** |
| `goal.evaluate` | `{condition,transcript}` | `verdict{met,reason}`\|`defer` | n/a | open |
| `loop.verifier` | `{target,transcript}` | `verdict{…}`\|`defer` | n/a | open |
| `loop.planner` | `{target,history,last,planner_notes}` | `plan{…}`\|`defer` | n/a | open |

`request`/`result`/verdict payloads serialize the corresponding yaca types
(`CompletionRequest`, tool output, `VerifierVerdict`, `PlannerOutput`). `chat.params`
mutation is **partial-merge**: only present keys overwrite; invalid values (e.g.
`temperature` out of range) ⇒ fail-open to original.

### 2.4 Event notification (observation)

```jsonc
{ "jsonrpc":"2.0","method":"event",
  "params": { "envelope": { "seq":42,"ts_millis":…, "event": { /* yaca_proto::Event */ } } } }
```

The **only** high-frequency frame. Host side: per-plugin **bounded** mpsc (cap 256),
**drop-oldest** under backpressure, sampled `warn!` on drops. Never blocks `emit`.

### 2.5 Tool-call dispatch (Child B contract, defined here)

```jsonc
{ "jsonrpc":"2.0","id":99,"method":"tool/call",
  "params": { "tool":"remember","session":"…","call":"…","input":{ … } } }
{ "jsonrpc":"2.0","id":99,"result": { "ok":true,"output":{ … },"time_ms":12 } }
```

Child A ships the types; Child B wires `ToolRegistry` injection + the `PluginTool`
proxy (the `McpTool` analog).

### 2.6 `tool.execute.after` safety

The wire result tags the original error **kind**. Plugins may rewrite output text,
but the host enforces TWO guards on apply (both unit-tested — review D4):
1. A plugin may **not synthesize** a `permission`-kind error (no fabricated denial).
2. A plugin may **not rewrite an original `permission`-kind `Err` into `Ok`** (or
   into a non-permission error) — i.e. a real permission denial cannot be masked
   into success. If the original result was `Err(ToolError::Permission)`, the host
   keeps it `Err(Permission)` regardless of the plugin's output.
Other Ok⇄Err swaps (non-permission) are allowed.

### 2.7 Shutdown

`host→plugin {method:"shutdown"}` → reply → child EOFs. Host waits
`shutdown_grace` (2s) then SIGTERM, then SIGKILL (ChildGuard).

## 3. Hook-dispatch model (`yaca-core`)

New `crates/yaca-core/src/hooks.rs`: the `HookDispatcher` trait the engine calls,
plus **native** (non-serde) payload/outcome types so engine code never touches wire
shapes.

```rust
#[async_trait] pub trait HookDispatcher: Send + Sync {
    fn dispatch_event(&self, env: &Envelope);                                  // sync, non-blocking
    async fn message_user_before(&self, i: MessageUserBeforeInput) -> MessageUserBeforeOutcome;
    async fn chat_params(&self, i: ChatParamsInput) -> ChatParamsOutcome;
    async fn tool_execute_before(&self, i: ToolExecuteBeforeInput) -> ToolExecuteBeforeOutcome; // Continue|Veto
    async fn tool_execute_after(&self, i: ToolExecuteAfterInput) -> ToolExecuteAfterOutcome;
    async fn permission_ask(&self, i: PermissionAskInput) -> Option<Decision>;  // None == defer
    async fn goal_evaluate(&self, i: GoalEvaluateInput) -> Option<GoalVerdict>;
    async fn loop_verifier(&self, i: LoopVerifierInput) -> Option<LoopVerdict>;
    async fn loop_planner(&self, i: LoopPlannerInput) -> Option<LoopPlan>;
}
```

`SessionEngine` gains `hooks: Option<Arc<dyn HookDispatcher>>` (default `None`) +
`with_hooks(...)` builder (mirrors `with_interaction`/`with_spawner` at
[engine.rs:73-93](../../../crates/yaca-core/src/engine.rs#L73)).

**[MERGE] R10 zero-overhead is structural**: every site is
`if let Some(d) = &self.hooks { … } else { <original> }`. `None` ⇒ one predicted
branch, no `.await`, no alloc, no clone. `dispatch_event` is a **sync `fn`** (no
Future) because it runs on every text-delta token.

### 3.1 Insertion points (both planners converged; verified file:line)

| Hook | Site | Edit |
|---|---|---|
| `event` | `emit` [engine.rs:169](../../../crates/yaca-core/src/engine.rs#L169) | build `envelope` first → `d.dispatch_event(&envelope)` → `bus.publish(envelope)` |
| `message.user.before` | `admit_user_prompt` [engine.rs:195](../../../crates/yaca-core/src/engine.rs#L195) | rebind `text` from outcome |
| `chat.params` | between `request_from_messages` & `providers.stream` [engine.rs:294](../../../crates/yaca-core/src/engine.rs#L294) | rebind `request` (partial-merge) |
| `tool.execute.before` | top of tool loop [engine.rs:336](../../../crates/yaca-core/src/engine.rs#L336) | `Continue`⇒replace `tc.input`; `Veto`⇒emit `ToolError`, `continue` |
| `tool.execute.after` | result mapping [engine.rs:353](../../../crates/yaca-core/src/engine.rs#L353) | rewrite `Result<Value,ToolError>` before building the event |
| `permission.ask` | `PermissionPlane::assert` Ask arm [permission.rs:196](../../../crates/yaca-tool/src/permission.rs#L196) | `Some(decision)`⇒apply; `None`⇒existing user-ask flow |
| session/message observe | (subsumed by `event`) | — |
| `goal.evaluate` | wrapper, no engine edit | `HookedGoalEvaluator` wraps `ModelGoalEvaluator` ([completion.rs:119](../../../crates/yaca-core/src/completion.rs#L119)) |
| `loop.verifier`/`loop.planner` | wrapper, no engine edit | `HookedLoopVerifier`/`HookedLoopPlanner` wrap impls ([loop_mode.rs:36-58](../../../crates/yaca-core/src/loop_mode.rs#L36)) |

**[MERGE] goal/loop via wrapper adapters** (both planners independently chose this)
keeps `completion.rs`/`loop_mode.rs` untouched: the wrapper prefers the plugin's
`Some(verdict/plan)` and falls back to the inner evaluator on `None`.

**[MERGE] `permission.ask` crosses crates** via the `PermissionInterceptor` trait in
`yaca-tool` (impl in `yaca-plugin`, wired by CLI) — both planners converged after
considering and rejecting a `yaca-tool → yaca-plugin` dep. Returning `None`/`defer`
falls through to the existing ask channel, preserving today's safety semantics.

## 4. Failure posture (R6 / D7)

A shared `run_hook(plugin, posture, fut)` in `host.rs`:

```rust
match tokio::time::timeout(posture_timeout(posture), fut).await {
  Ok(Ok(v))   => v,                          // happy path
  Ok(Err(_))  => apply_posture(posture),     // plugin error / crash mid-call
  Err(_)      => apply_posture(posture),     // timeout
}
```

- **safe** ⇒ answer-hooks return `None`/`defer` (engine uses its default flow);
  `tool.execute.before` ⇒ `Veto{reason:"plugin guard failed safe"}` (block the
  tool). `permission.ask` deferring ⇒ normal user-ask runs.
- **open** ⇒ return input unchanged; the turn proceeds. `warn!` logged.

Timeouts (defaults; per-plugin `timeout_ms` + per-hook override): `permission.ask`
5s (humans answer slowly), `tool.execute.before` 1s, others 500ms,
`INITIALIZE` 5s (matches yaca-mcp).

**Crash/restart**: a per-plugin watcher (`child.wait()` / JoinSet) drains the
`Pending` map with `Closed` on exit, marks the plugin `Dead` (its chain entries
become no-ops), and lazily respawns on next use up to **3 restarts / 60s**, then
`Disabled` with a loud log. Missed events during downtime are dropped (the on-disk
event log is the durable record; plugins are not durable subscribers).

## 5. Host / manager lifecycle (`PluginHost`)

Generalize `McpManager`:

- `PluginHost::connect_all(specs) -> PluginHost` — `JoinSet`, fault-isolated; each
  plugin: `spawn → initialize → record hooks/tools`. A failed/slow handshake is
  logged and skipped (others proceed). **CRITICAL (review D5): `JoinSet` yields
  completion order, which is NON-deterministic. The host MUST store the declared
  LOAD order (the merged `Vec<PluginSpec>` index) and build the per-hook chain in
  that load order, NOT JoinSet completion order.** Sort/insert results back into
  load-order positions before any chain runs.
- Per plugin: `PluginClient` (send/notify channels, `Pending` map, reader+writer+
  notifier+watcher tasks), status `Alive|Dead|Disabled`, declared hooks/tools.
- **Chains**: per-hook, plugins run **sequentially in declared LOAD order**; each
  plugin's output feeds the next (mirrors Compat). `permission.ask`/goal/loop =
  first-non-`defer` wins. `event` = broadcast to all (bounded, drop-oldest).
  **Mandatory test:** two plugins where the first-loaded plugin is slow to hand
  shake and the second is fast — assert the chain still applies them in load order
  (first-loaded mutates first), proving order is independent of `JoinSet`/handshake
  timing.
- `PluginHost` implements `HookDispatcher` (`dispatcher.rs`) and provides the
  `PermissionInterceptor` (`permission_bridge.rs`) and goal/loop wrappers.

## 6. Config, loading & bootstrap (R7)

### 6.1 `plugins:` config (mirrors `McpServerConfig`)

```yaml
plugins:
  remember:
    kind: rust                    # rust | compat
    command: ["./bin/remember"]   # argv
    enabled: true
    timeout_ms: 1000
    env: { KEY: "{env:REMEMBER_KEY}" }   # same secret templating as providers
```

### 6.2 Dir-scan + `plugin.toml` manifest (mirrors `skills.rs`)

Scan `.yaca/plugins/*/plugin.toml` and `$XDG_CONFIG_HOME|~/.config/yaca/plugins/*/plugin.toml`:

```toml
id = "remember"
kind = "rust"
command = ["./remember"]     # resolved relative to the manifest dir
enabled = true
timeout_ms = 1000
[[hooks]]
name = "tool.execute.before"
posture = "safe"
```

Trust boundary = "you own these dirs" (D6); same isolation/posture as config
plugins; unknown hook names → warn + drop. Config wins on id collision; either
side's `enabled:false` disables.

### 6.3 Bootstrap → `YacaRuntime`

**[MERGE]** Replace the split `resolve_router` + `build_session_engine` (called by
all 5 modes) with a single `bootstrap(store, model_override) -> YacaRuntime`
returning a struct (not a growing tuple):

```rust
pub struct YacaRuntime {
    pub engine: Arc<SessionEngine>,
    pub agent: AgentSpec,
    pub asks: AskRx,
    pub questions: QuestionRx,
    pub plugins: PluginHost,   // must outlive engine; dropped last
    pub mcp: McpManager,
}
```

Flow: load config (providers + `plugins:`) → discover manifests → merge →
`PluginHost::connect_all` → `host: Arc<dyn HookDispatcher>` →
`PermissionPlane::new(rules).with_interceptor(host.clone())` →
`SessionEngine::new(...).with_hooks(host.clone())` → wrap goal/loop evaluators for
`cmd_goal`/loop. All five modes (`cmd_exec`/`cmd_rpc`/`cmd_goal`/`cmd_tui`/`cmd_serve`)
use it; `cmd_tail_session` uses an empty plugin set.

## 7. Child boundaries

- **Child A** delivers everything in §1–§6 except the registry injection of plugin
  tools. It ships the `tool/call` types (§2.5) but does not wire them.
- **Child B** adds the `PluginTool` proxy (the `McpTool` analog) using §2.5 and
  registers declared `tools` during bootstrap (via the **already-existing**
  `ToolRegistry::register`, [tool.rs:94](../../../crates/yaca-tool/src/tool.rs#L94))
  before the registry freezes into `Arc`. No new registry primitive is required
  (the registry is already dynamic); an `extend(...)` convenience wrapper is
  optional.
- **Child C** adds a `kind:compat` Bun plugin (one child process) that speaks this
  exact protocol on stdio and re-emits Compat `Hooks`. **The protocol does not
  change for C.** C maps Compat `(input,output)`+throw ⇄ our outcome enums, and
  points the Compat SDK `client` at a `yaca serve` instance.

## 8. Risks / explicit tradeoffs

| Risk | Resolution |
|---|---|
| `notify event` flood under token streaming | bounded 256 + drop-oldest; never block `emit`; sampled warn |
| `tool.execute.after` masking permission errors | wire tags original kind; plugins can't synthesize permission errors |
| per-hook timeout vs. legit slow plugins | per-plugin `timeout_ms` + per-hook override; generous `permission.ask` 5s |
| bootstrap ordering vs. `Arc`-freeze (Child B tools) | `connect_all` awaited before registry `Arc::new` |
| restart storm on a bad plugin | 3/60s then Disabled + loud log |
| no OS sandbox (D7) | explicit; plugins inherit parent fs/net; documented trust boundary |
| duplicating yaca-mcp transport | accept minor dup for v1; follow-up extract shared `jsonrpc-stdio` |

## 9. Open follow-ups (not v1)

Extract shared `jsonrpc-stdio`; out-of-process providers; OS sandbox; full
`experimental.*`/provider-frame/compaction hooks; a first-run trust prompt for
dir-scanned plugins; surfacing plugin status on the event bus.
