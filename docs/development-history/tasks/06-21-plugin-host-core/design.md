# Design — Child A: Plugin host & hook-dispatch core

> Shared architecture, the full IPC protocol, the dependency DAG, and the failure
> posture model are in the **parent design**:
> [../06-21-plugin-system/design.md](../06-21-plugin-system/design.md). This
> document is the **A-specific implementation slice**: exact code edits, the host
> impl (modeled on `yaca-mcp`), the example plugin, and A's deliver/defer line.
> Requirements: [./prd.md](./prd.md).

## What Child A delivers vs defers

- **Delivers**: `yaca-plugin` crate (protocol, codec, client, host/manager,
  manifest, config, dispatcher, permission/goal/loop bridges); `HookDispatcher`
  trait + payloads in `yaca-core`; `PermissionInterceptor` trait in `yaca-tool`;
  the engine/permission insertion points; the `YacaRuntime` bootstrap across all 5
  modes; config `plugins:` + dir-scan + `plugin.toml`; failure posture; a native
  example plugin + e2e/QA.
- **Defers**: registry injection of plugin **tools** (Child B — A ships the
  `tool/call` types but does not register tools); the Compat Bun adapter (Child
  C). Plugin `tools` from `initialize` are parsed + stored, not wired.

## 1. `yaca-plugin` crate (modeled on `yaca-mcp`)

Modules per parent §1. Reuse `yaca-mcp` patterns directly:

- `client.rs` `PluginClient` ≈ `McpClient`: `Arc<ClientInner>` with a
  `Mutex<writer>`, `Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value,PluginError>>>>>`,
  `next_id: AtomicU64`, a reader task that demuxes responses by id and routes
  `event`/`shutdown` notifications, `close_pending` on EOF, `call(method,params,timeout)`,
  `notify(method,params)`. Copy `MAX_LINE_BYTES`, `INITIALIZE_TIMEOUT`, the
  `McpError`-style enum (`PluginError`).
- `child.rs` `ChildProcess::spawn(&command, env) -> (PluginClient, ChildGuard)` ≈
  `McpClient::spawn` + `ChildGuard` (drop-kills).
- `host.rs` `PluginHost::connect_all(specs)` ≈ `McpManager::connect_all`: `JoinSet`,
  fault-isolated; each plugin `spawn → initialize → record`. Plus the chain runner,
  the per-plugin event mpsc(256, drop-oldest), the supervisor (restart ≤3/60s).

`Cargo.toml` deps (all workspace, no new root deps): `yaca-proto`, `yaca-tool`,
`yaca-provider`, `yaca-core`, `async-trait`, `serde`, `serde_json`, `thiserror`,
`tokio`, `tracing`, `toml`; dev: `tokio` test-util.

## 2. `yaca-core` edits

### 2.1 New `hooks.rs`
The `HookDispatcher` trait + native payload/outcome types (parent §3). Re-export
from `lib.rs`. Native types intentionally avoid serde (engine never sees wire
shapes); `yaca-plugin` converts native ⇄ wire.

### 2.2 `SessionEngine` (engine.rs)
- Add field `hooks: Option<Arc<dyn crate::hooks::HookDispatcher>>` (struct
  [~L36-46](../../../crates/yaca-core/src/engine.rs#L36)); init `None` in `new`
  ([~L50-70](../../../crates/yaca-core/src/engine.rs#L50)); add `with_hooks`
  ([~L73](../../../crates/yaca-core/src/engine.rs#L73)).
- `emit` ([L169](../../../crates/yaca-core/src/engine.rs#L169)): build `envelope`
  before publish; `if let Some(d)=&self.hooks { d.dispatch_event(&envelope); }`;
  then `bus.publish(envelope)`.
- `admit_user_prompt` ([L195](../../../crates/yaca-core/src/engine.rs#L195)):
  rebind `text` from `message_user_before` outcome (gated by `hooks`).
- `run_turn` request site ([L294](../../../crates/yaca-core/src/engine.rs#L294)):
  rebind `request` from `chat_params` (partial-merge; gated).
- `run_turn` tool loop ([L336-370](../../../crates/yaca-core/src/engine.rs#L336)):
  `tool_execute_before` (`Continue`⇒replace input; `Veto`⇒emit `ToolError` +
  `continue`); after exec, `tool_execute_after` rewrites the `Result`. **Gate the
  `input.clone()` for `after` behind `self.hooks.is_some()`** so the hookless path
  stays zero-copy. **[D4] Permission-error guard:** when applying `after`, if the
  ORIGINAL result was `Err(ToolError::Permission(..))`, the host/engine keeps it
  `Err(Permission)` regardless of the plugin's output (a plugin cannot mask a
  denial into `Ok` or into a different error), and a plugin cannot synthesize a new
  `permission`-kind error (parent design §2.6). Unit-test both directions.

### 2.3 Goal/loop wrappers
`HookedGoalEvaluator`/`HookedLoopVerifier`/`HookedLoopPlanner` live in `yaca-plugin`
(`goal_bridge.rs`) and wrap the existing `GoalEvaluator`/`LoopVerifier`/`LoopPlanner`
trait objects; CLI installs them in `cmd_goal`/loop when plugins exist.

## 3. `yaca-tool` edits (permission.rs)

- Add `trait PermissionInterceptor { async fn intercept(&self, session, action, resource) -> Option<Decision>; }`.
- `PermissionPlane` gains `interceptor: Option<Arc<dyn PermissionInterceptor>>` +
  `with_interceptor(...)`; the existing `new(rules)` keeps working (`None`).
- In `assert` Ask arm ([L196](../../../crates/yaca-tool/src/permission.rs#L196)),
  after persistent-rules check and before the `AskRequest` send: if
  `Some(decision)` from the interceptor ⇒ route through a refactored
  `apply_decision(action, resource, decision)` helper (the same logic the existing
  `rx.await` arm uses at [L208-227](../../../crates/yaca-tool/src/permission.rs#L208));
  `None` ⇒ fall through to the existing user-ask flow. Extend the existing test
  module ([L231](../../../crates/yaca-tool/src/permission.rs#L231)).

## 4. `yaca-cli` edits

- `config.rs`: add `plugins: BTreeMap<String, PluginEntry>` to `FileConfig`
  ([~L25](../../../crates/yaca-cli/src/config.rs#L25)) reusing `resolve_secret`
  for `env`; extend the resolved config to carry the plugin specs (do NOT discard
  them as the current `ResolvedConfig` discards raw provider decls).
- New `plugins.rs` (mirror [skills.rs](../../../crates/yaca-cli/src/skills.rs)):
  `discover_plugin_manifests(dirs)`, `merge_with_config(config, manifests)`.
- New `bootstrap.rs` (or fold into `main.rs`): `bootstrap(store, model_override) ->
  YacaRuntime` (parent §6.3), replacing the split `resolve_router`
  ([~L281](../../../crates/yaca-cli/src/main.rs#L281)) + `build_session_engine`
  ([~L240](../../../crates/yaca-cli/src/main.rs#L240)). Thread it through
  `cmd_exec`/`cmd_rpc`/`cmd_goal`/`cmd_tui`/`cmd_serve`; `cmd_tail_session` gets an
  empty plugin set. `PluginHost` is the last-dropped field so its tasks outlive the
  engine.

## 5. Native example plugin (`crates/yaca-plugin-example`)

A tiny bin (deps: `yaca-plugin` types + `tokio` + `serde_json`) that: implements
`message.user.before` (prepend a marker), `chat.params` (set `temperature=0.1`),
`tool.execute.before` (veto a sentinel command to exercise the guard path), and
`event` (log to stderr). Deterministic, for integration tests + live QA. Auto-built
by `cargo test` (discoverable via `CARGO_BIN_EXE_*`).

## 6. Test strategy (TDD)

- Unit: protocol/codec round-trip every message kind; manifest/config parse +
  merge; dispatcher no-op equivalence (engine behaves identically with `None`);
  posture (timeout→open passes original, timeout→safe vetoes); permission
  interceptor short-circuits the ask channel.
- Integration: a Rust/python fixture plugin (yaca-mcp uses an inline python fixture
  — reuse that style) driving `connect_all` + a real `run_turn` against
  `FakeProvider`/echo; assert hooks fire and mutate; crash mid-call → turn
  completes per posture.
- **[D5] Chain load-order test (mandatory):** two plugins where the first-loaded
  plugin handshakes slowly and the second handshakes fast; assert the per-hook
  chain still applies them in **declared load order** (first-loaded mutates first),
  proving order is independent of `JoinSet` completion / handshake timing.
- **[D4] Permission-preservation test:** an original `Err(ToolError::Permission)`
  passed through `tool_execute_after` with a plugin that returns `Ok(..)` still
  yields `Err(Permission)`; a plugin cannot synthesize a permission error.
- Live QA: real `yaca exec` with the example plugin; observe mutated prompt + the
  veto; `kill -9` the plugin mid-turn and watch the turn finish (AC4/AC5).
- Bench: `yaca exec` wall-clock with no `plugins:` vs. a no-op plugin; assert no
  measurable regression (AC6/AC8).

## 7. A-specific risks

- The `bootstrap` refactor touches all 5 mode entrypoints — land it behind green
  tests per mode; keep the change additive (a thin `bootstrap` that internally
  calls today's helpers first, then layers plugins) to ease rollback.
- `FakeProvider` may need a tap to record the last `CompletionRequest` for the
  `chat.params` assertion — minimal, test-only addition.
- Watch the `permission.ask` refactor: preserve `AllowAlways` widening + typed
  `Reject` exactly (existing tests must stay green).
