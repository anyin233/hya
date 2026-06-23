# Implement — Child A: Plugin host & hook-dispatch core

> Design: [./design.md](./design.md) + parent
> [../06-21-plugin-system/design.md](../06-21-plugin-system/design.md).
> TDD throughout (test first). **Gate after every phase**:
> `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
> Stop and fix on first red. Estimated effort: Medium–Large (3–5 focused days).

## Phase 0 — Scaffold (no behavior change)
1. Create `crates/yaca-plugin/` (`Cargo.toml` + `src/lib.rs` with empty
   `pub mod protocol; pub mod messages; pub mod error;`) and
   `crates/yaca-plugin-example/` (bin stub). `members = ["crates/*"]` auto-includes.
2. Gate green; nothing references the new crates yet.
- **Rollback**: delete the two dirs. **AC**: baseline (AC6/AC8 green).

## Phase 1 — Protocol + codec + manifest/config (pure data, TDD)
1. `tests/protocol_roundtrip.rs`: assert every frame (request/response-ok/
   response-error/notification) and every hook/tool payload (parent §2) serde
   round-trips as **JSON-RPC 2.0** (`jsonrpc:"2.0"`), matching `yaca-mcp` shapes.
2. Implement `protocol.rs` (reuse `yaca-mcp::protocol` shapes) + `messages.rs`
   (typed payloads, `HookName`, `HookPosture`, constants).
3. `tests/codec_lines.rs`: duplex stream with valid + malformed + oversized lines →
   `FrameReader` yields valid frames, surfaces `Malformed`/`OversizedLine` once, no
   partial-line corruption. Implement `codec.rs` (`MAX_LINE_BYTES`).
4. `tests/manifest_config.rs`: parse `plugin.toml` (unknown hook → warn+drop) and
   the `plugins:` YAML; table-driven merge (config wins, `enabled:false` disables).
   Implement `manifest.rs` + `config.rs`.
- **Rollback**: crate is self-contained. **AC**: foundation.

## Phase 2 — `HookDispatcher` trait + engine seam (no-op proven)
1. `yaca-core/src/hooks.rs`: trait + native payload/outcome types (parent §3);
   re-export from `lib.rs`.
2. Engine: add `hooks: Option<Arc<dyn HookDispatcher>>` (default `None`) +
   `with_hooks`. Wire ALL insertion points (design §2.2) as gated no-op calls.
   Add `PermissionInterceptor` trait + `with_interceptor` + the `assert` Ask-arm
   short-circuit + `apply_decision` refactor (design §3).
3. Test (yaca-core): a `CountingHookHost` (in-crate test impl) asserts each hook is
   invoked exactly once per logical event over a `FakeProvider` turn, and that text/
   request/results pass through unchanged. Test (yaca-tool): interceptor returning
   `Some(AllowOnce)` means the ask channel never receives an `AskRequest`; existing
   permission tests stay green.
4. **[D4] Permission-preservation test:** a test `HookDispatcher` whose
   `tool_execute_after` returns `Ok(rewritten)` for a tool call whose original
   result was `Err(ToolError::Permission)` — assert the engine still emits a
   `ToolError` for that call (the denial is NOT masked into a `ToolResult`). Also
   assert a plugin returning a fabricated permission-kind error is rejected.
- **Rollback**: revert engine + permission edits; new crate stays inert.
- **AC**: AC6/AC8 (no-op path identical; zero overhead).

## Phase 3 — `PluginClient` + `ChildProcess` transport (TDD vs fixture)
1. Add a fixture plugin (inline python like `yaca-mcp`'s manager test, or a small
   rust bin): replies to `initialize`; echoes `hook/tool.execute.before`.
2. `tests/client_demux.rs` (port `yaca-mcp`'s `demuxes_responses_by_id` +
   `returns_timeout_errors`). Implement `client.rs` (`PluginClient`: reader/writer
   tasks, `Pending` demux, `call`/`notify`/`close_pending`) + `child.rs`
   (`spawn` + `ChildGuard`).
3. `tests/handshake.rs`: spawn fixture → `initialize` → assert declared hooks/tools;
   version mismatch → child killed.
- **Rollback**: keep Phases 0–2; drop Phase 3. **AC**: groundwork for AC1.

## Phase 4 — `PluginHost` chains + `HookDispatcher` impl (TDD)
1. `tests/host_dispatch.rs`: a host with the fixture runs `tool_execute_before` →
   mutated input returned; `event` notification reaches the plugin (bounded
   channel). Implement `host.rs` (`connect_all` via JoinSet that **stores declared
   load order and re-sorts results into load-order positions**; chain runner:
   sequential mutate in **load order**, first-non-`defer` for answer hooks,
   broadcast for `event`) + `dispatcher.rs` (`impl HookDispatcher`) +
   `permission_bridge.rs` + `goal_bridge.rs`.
2. **[D5] `tests/chain_load_order.rs`:** two fixtures — first-loaded handshakes
   slowly (sleep before `initialize` reply), second fast; both register
   `tool.execute.before` and append a tag to the args; assert the final args show
   first-loaded's tag applied BEFORE second's, i.e. chain order == load order, not
   `JoinSet` completion order.
- **Rollback**: engine still works on `None`. **AC**: AC1 (interception asserted),
  load-order correctness.

## Phase 5 — Failure posture (TDD)
1. `tests/posture_open_timeout.rs`: `chat.params` plugin sleeps > timeout, posture
   `open` → original `CompletionRequest` returned, no error.
2. `tests/posture_safe_timeout.rs`: `tool.execute.before` posture `safe`, sleeps →
   `Veto{reason contains "guard failed safe"}` → engine emits `ToolError`.
3. `tests/crash_restart.rs`: fixture exits mid-call → pending resolves `Closed`,
   status `Dead`, next call respawns; >3/60s → `Disabled`.
   Implement `run_hook` timeout + `apply_posture` + watcher/restart.
- **Rollback**: posture local to host/chain. **AC**: AC3, AC4.

## Phase 6 — Config schema + dir-scan + `YacaRuntime` bootstrap (all 5 modes)
1. `config.rs`: `plugins:` on `FileConfig`; carry specs through resolution. New
   `plugins.rs` (discover + merge) with unit tests.
2. New `bootstrap()` → `YacaRuntime` (parent §6.3); wire `PluginHost` →
   `with_interceptor` + `with_hooks` + goal/loop wrappers. Thread through
   `cmd_exec`/`cmd_rpc`/`cmd_goal`/`cmd_tui`/`cmd_serve`; empty set for
   `cmd_tail_session`. `PluginHost` dropped last.
3. Keep the change additive (bootstrap internally reuses today's router/engine
   builders, then layers plugins) for easy rollback.
- **Rollback**: revert `main.rs`/`config.rs`; rest unused. **AC**: AC5.

## Phase 7 — Example plugin + e2e + live QA + bench
1. `crates/yaca-plugin-example`: implements `message.user.before` (marker),
   `chat.params` (`temperature=0.1`), `tool.execute.before` (veto sentinel),
   `event` (stderr log).
2. e2e test (`yaca-cli`): temp `config.yaml` → example bin; `yaca exec` on the
   offline `DevProvider`; assert mutated prompt + (via a `FakeProvider` last-request
   tap) `temperature=0.1`; assert the sentinel command is vetoed.
3. **Live QA runbook** (record evidence): real `yaca exec`, observe mutation + veto;
   `kill -9` the plugin mid-turn → turn completes (AC4 in the wild).
4. **[D1] Overhead protocol (concrete, per AC8):** (a) HARD GATE unit test —
   default `SessionEngine::new` has `hooks == None`; with a counting global
   allocator assert the `None` branch adds **zero allocations** over a `run_turn`
   and makes zero dispatcher calls; (b) PERF EVIDENCE — manual-timer microbench
   behind `--features bench` runs `emit` 100_000× and `run_turn`/`FakeProvider`
   1_000×, comparing `hooks=None` vs the pre-plugin baseline; record medians, assert
   the `None` path stays within **+3%** median. Numbers → QA log.
- **Rollback**: delete example + tests. **AC**: AC1, AC2, AC6, AC7 + overhead gate (AC8).

## Risky files / rollback map
- High-risk (revert with care): `crates/yaca-core/src/engine.rs` (run_turn/emit),
  `crates/yaca-tool/src/permission.rs` (assert), `crates/yaca-cli/src/main.rs`
  (bootstrap touches all 5 modes).
- Low-risk: `crates/yaca-plugin/*` (new), `config.rs` additions, `plugins.rs` (new),
  `crates/yaca-plugin-example/*` (new).

## Pre-start gate
- Cross-model **plan-review** on this `implement.md` + both `design.md` files
  (parent + Child A) before `task.py start` (per project mandate).

## Cross-child contract reminder
- Child B reuses `protocol.rs` `tool/call` + the stored `declared_tools`; it only
  adds the `PluginTool` proxy and registers it via the **existing**
  `ToolRegistry::register` (no new registry primitive).
- Child C reuses the whole stack; adds only a `kind:opencode` Bun child speaking
  this protocol. **Protocol must not change for B/C** without a coordinated bump.
