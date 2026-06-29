# Implement - Child B: Plugin-registered tools

> Requirements: [./prd.md](./prd.md). Design: [./design.md](./design.md).
> Parent protocol and Child A host are locked. This plan is TDD-first and
> minimal: reuse the existing `Tool`/`ToolRegistry`/MCP patterns, wire plugin tools
> at bootstrap, and verify B-AC1..B-AC5 end to end.

## 0. Preconditions And Stop Rules

Preconditions:

- Child A is landed and green.
- `crates/hya-plugin` exposes the protocol types, `PluginClient`, and
  `PluginHost` with initialized plugin records that include declared tools.
- The parent `tool/call` frame remains exactly:
  `{ "tool", "session", "call", "input" } -> { "ok", "output", "time_ms" }`.

Stop rules:

- Do not change the parent protocol in Child B. If `tool/call` cannot be consumed
  as designed, stop and coordinate a Child A/parent protocol revision.
- Do not start from runtime hot-loading. Child B is bootstrap-only.
- Do not make provider changes for schema advertisement. If schemas do not appear,
  debug registry construction first.

Workspace gate after every phase:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

If a phase fails the gate, fix only failures caused by that phase. Pre-existing
unrelated failures are recorded before moving on.

## Phase 1 - Lock No-plugin Baseline And Registry Batch API

Acceptance criteria: B-AC5, B-AC4 groundwork.

RED tests first:

- Add a `ToolRegistry::extend` test in
  `crates/hya-tool/tests/tool.rs` beside
  [`registry_rejects_duplicate_tool_name`](../../../crates/hya-tool/tests/tool.rs#L69).
- Test that extending with one unique dummy tool registers it and extending with a
  duplicate returns `DuplicateName` without replacing the original.
- Add a no-plugin schema regression test that captures the builtin schema names
  from `ToolRegistry::builtins().schemas()` and asserts they are unchanged after
  `extend(Vec::<Arc<dyn Tool>>::new())`.

GREEN implementation:

- Add `ToolRegistry::extend<I>(&mut self, tools: I) -> Vec<DuplicateName>` in
  `crates/hya-tool/src/tool.rs` after `register`.
- Keep `builtins()` unchanged.
- Do not change `get()` or `schemas()`.

Rollback point:

- Revert only the `extend` helper and its tests. Current MCP registration can
  continue to loop over `register` if this helper is not needed.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 2 - Add Tool-call Metadata And Plugin Permission Action

Acceptance criteria: B-AC1, B-AC2, B-AC3 groundwork.

RED tests first:

- Update one existing `ToolCtx` test helper in `crates/hya-tool/tests/tool.rs` to
  require `session` and `tool_call`; leave `tool_call: None` for builtin tools.
- Add a serde round-trip test in `crates/hya-tool/src/permission.rs` mirroring the
  existing `Action::Mcp` test at
  [`permission.rs:267`](../../../crates/hya-tool/src/permission.rs#L267), asserting
  `Action::Plugin` serializes to `"plugin"`.
- Add policy tests in `crates/hya-cli/src/permission.rs` beside
  [`mcp_policy_matches_wave5_contract`](../../../crates/hya-cli/src/permission.rs#L176):
  `ReadOnly` rejects plugin tools, `Scoped` allows once, `Yolo` allows once.

GREEN implementation:

- Add `ToolCtx { session: SessionId, tool_call: Option<ToolCallId>, ... }` in
  `crates/hya-tool/src/tool.rs`.
- Update `SessionEngine`'s tool loop at
  [`engine.rs:340`](../../../crates/hya-core/src/engine.rs#L340) to set
  `session` and `tool_call: Some(tc.call)`.
- Update tests and helper constructors that build `ToolCtx`.
- Add `Action::Plugin` in `crates/hya-tool/src/permission.rs`.
- Add the TUI/CLI label and policy handling for `Action::Plugin` wherever action
  labels are exhaustively matched.

Rollback point:

- Revert `Action::Plugin`, CLI/TUI policy labels, and `ToolCtx` metadata together.
  Do not leave `ToolCtx` tests half-updated.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 3 - Build `PluginTool` With Unit Tests

Acceptance criteria: B-AC1, B-AC2, B-AC3.

RED tests first in `crates/hya-plugin/src/tool.rs`:

- `try_new_accepts_object_schema`: a declared tool with
  `{ "type":"object" }` returns a `Tool` whose `name()` and `schema()` match the
  plugin declaration.
- `try_new_rejects_non_object_schema`: string/array input schemas return `None`.
- `execute_sends_locked_tool_call_frame`: use `tokio::io::duplex` like
  [`McpTool` tests](../../../crates/hya-mcp/src/bridge.rs#L104) and assert the
  request method is `tool/call` with `tool`, `session`, `call`, and `input`.
- `execute_maps_ok_reply_to_output`: reply with
  `{ "ok": true, "output": { "remembered": true }, "time_ms": 1 }` and assert
  the returned value is the output.
- `execute_maps_plugin_failure_to_tool_error`: reply with
  `{ "ok": false, "output": { "error": "nope" } }` and assert
  `ToolError::Other`.
- `execute_denied_permission_sends_no_frame`: configure
  `PermissionRules::new(vec![Rule::new(Action::Plugin, "*", Mode::Deny)])`, call
  `execute`, and assert the server side receives no `tool/call` before timeout.
- `execute_without_tool_call_id_errors`: pass `tool_call: None` and assert a local
  `ToolError::Other` before IPC.

GREEN implementation:

- Add `crates/hya-plugin/src/tool.rs` with `PluginTool`, `ToolCallReply`,
  `plugin_tool_resource`, `format_plugin_error`, and the `Tool` impl.
- Re-export `PluginTool` from `crates/hya-plugin/src/lib.rs`.
- Use Child A's existing declaration type for plugin tools. Do not duplicate the
  protocol shape.
- Map all `PluginError` variants to `ToolError::Other` with prefix
  `plugin '<id>' tool '<name>': ...`.
- Accept `time_ms` but do not use it for engine timing.

Rollback point:

- Revert only `crates/hya-plugin/src/tool.rs` and its `lib.rs` re-export. Phases
  1 and 2 remain useful groundwork.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 4 - Register Plugin Tools In Bootstrap

Acceptance criteria: B-AC1, B-AC4, B-AC5.

RED tests first:

- In `crates/hya-plugin` or `crates/hya-cli` tests, create two initialized fake
  plugins with declared tools in a known order and assert registration preserves
  that order for collision precedence.
- Add a builtin collision test: plugin declares `read`; after bootstrap registry
  lookup for `read` still returns the builtin schema description from
  [`ReadTool`](../../../crates/hya-tool/src/tool.rs#L140).
- Add a plugin-vs-plugin collision test: two plugins declare `remember`; first
  plugin's proxy wins, second is skipped.
- Add a no-plugin bootstrap test: with an empty plugin set, the registered schema
  names match `ToolRegistry::builtins()` exactly as a set.

GREEN implementation:

- Add a Child B accessor on `PluginHost`, for example
  `plugins_in_load_order()` or `tools_in_load_order()`, using Child A's stored
  plugin order, not `JoinSet` completion order.
- In Child A's `bootstrap(...) -> HyaRuntime`, insert the plugin-tool loop after
  MCP registration and before `Arc::new(registry)`.
- Use `registry.register(...)` or `registry.extend(...)`; on `DuplicateName`, log
  `tracing::warn!(plugin, tool, error, "skipping duplicate plugin tool")` and keep
  bootstrapping.
- On invalid schema/name from `PluginTool::try_new`, log a warning and continue.

Rollback point:

- Revert the bootstrap loop and `PluginHost` accessor. Keep `PluginTool` unit tests
  intact until deciding whether the host contract needs revision.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 5 - Prove Schema Advertisement Without Provider Changes

Acceptance criteria: B-AC1, B-AC5.

RED tests first:

- Add a request-capturing provider test, using the pattern from
  `crates/hya-core/tests/category_routing.rs`, that records the
  `CompletionRequest` passed to `Provider::stream`.
- Configure one plugin tool named `remember` with an object schema.
- Run one engine turn with a fake provider that stops immediately.
- Assert `request.tools` contains `remember` with the plugin-declared schema.
- Run the same test with no plugin tools and assert `request.tools` equals the
  builtin schema set.

GREEN implementation:

- No provider code changes should be necessary.
- If the test fails, fix bootstrap registry assembly, not provider encoders.

Rollback point:

- Revert only the test if it exposes a Child A host gap; otherwise keep the test
  as the B-AC1 schema guard.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 6 - Prove Tool-call Round-trip End To End

Acceptance criteria: B-AC1.

RED tests first:

- Add an integration fixture plugin that responds to `initialize` with:

```json
{
  "protocol_version": 1,
  "plugin": { "id": "memory", "version": "0.1.0", "kind": "rust" },
  "hooks": [],
  "tools": [
    {
      "name": "remember",
      "description": "Remember a key/value pair for the current test.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "key": { "type": "string" },
          "value": { "type": "string" }
        },
        "required": ["key", "value"]
      }
    }
  ]
}
```

- The fixture handles `tool/call` for `remember` by returning
  `{ "ok": true, "output": { "remembered": true, "key": key, "value": value }, "time_ms": 1 }`.
- Use `FakeProvider::scripted_turns` to emit a `ToolCallRequested` for `remember`,
  then a second turn response that finishes.
- Assert the final projection contains a `ToolPartState::Completed` output with
  `remembered: true`.

GREEN implementation:

- Wire the fixture through actual `PluginHost::connect_all`, not a direct
  `PluginTool` constructor, so the test covers host storage, bootstrap registry
  injection, engine lookup, IPC call, and event projection.

Rollback point:

- If the test reveals a protocol mismatch, stop and coordinate with Child A. Do
  not patch the wire shape inside Child B.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 7 - Prove Permission Denial Blocks Side Effects

Acceptance criteria: B-AC2.

RED tests first:

- Use the same fixture plugin but increment an atomic counter whenever it receives
  `tool/call`.
- Configure permission rules with `Rule::new(Action::Plugin, "*", Mode::Deny)`.
- Run a fake-provider turn that requests `remember`.
- Assert the engine emits or projects a tool error.
- Assert the fixture counter remains zero.

GREEN implementation:

- The phase should require no new production code if Phase 3 put the permission
  assert before IPC. If it requires production changes, move the assert to the
  first line of `PluginTool::execute`.

Rollback point:

- Revert only the permission-specific test if the failure is caused by Child A's
  permission interceptor contract. Otherwise fix `PluginTool`.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 8 - Prove Timeout And Crash Behavior

Acceptance criteria: B-AC3.

RED tests first:

- Hung fixture: plugin initializes and declares `remember`, then never replies to
  `tool/call`; configure `timeout_ms: 50`.
- Crash fixture: plugin initializes and declares `remember`, then exits after
  reading `tool/call` before replying.
- For both fixtures, run a fake-provider turn requesting `remember`.
- Assert the turn completes, the projection contains `ToolPartState::Error`, and
  the test process does not hang.

GREEN implementation:

- Use the existing `PluginClient::call` timeout/closed behavior from Child A.
- Map timeout/closed errors to `ToolError::Other`; do not panic and do not retry
  inside `PluginTool`.

Rollback point:

- If timeout never fires, fix Child A's `PluginClient::call` or stop for a Child A
  bug. Do not add an ad hoc timeout wrapper that fights the shared client.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 9 - Extend The Example Plugin And Live QA

Acceptance criteria: B-AC1, B-AC2, B-AC3, B-AC5.

RED/manual target first:

- Extend `crates/hya-plugin-example/src/main.rs` from Child A so it declares and
  handles the `remember` tool used in tests.
- Build the CLI and example plugin:

```sh
cargo build -p hya-cli -p hya-plugin-example
```

- Create a temporary config that keeps an existing real tool-capable provider and
  adds the example plugin. If no real provider credentials are available, record
  the live QA as blocked and do not claim B-AC1 live QA complete.

Live `hya exec` QA command shape:

```sh
XDG_CONFIG_HOME="$TMP_HYA_CONFIG" \
  target/debug/hya exec --model "$HYA_TOOL_MODEL" --json \
  'Call the remember tool exactly once with key="qa" and value="plugin-round-trip", then summarize the returned JSON.'
```

Expected evidence:

- JSON transcript contains `ToolCallRequested` with `name: "remember"`.
- JSON transcript contains `ToolResult` with
  `output.remembered == true`, `output.key == "qa"`, and
  `output.value == "plugin-round-trip"`.
- Final assistant text mentions the remembered value.

Permission-deny live QA:

- Run the same command with a config/policy that denies `Action::Plugin`.
- Assert no plugin-side `tool/call` log appears and the transcript contains a
  `ToolError`.

Timeout/crash live QA:

- Run the example plugin in a mode that hangs or exits on `tool/call` with
  `timeout_ms: 50`.
- Assert `hya exec` exits normally and the transcript contains a `ToolError`.

GREEN implementation:

- Add only the example plugin tool declaration/handler needed for QA.
- Do not add protocol variants or special CLI flags.

Rollback point:

- Revert example-plugin additions if they introduce flakes, while keeping unit and
  integration tests as the required automated proof.

Gate:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## Phase 10 - Final Acceptance Sweep

Acceptance criteria: B-AC1..B-AC5.

Run and record:

```sh
cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Acceptance checklist:

- B-AC1: automated integration and live `hya exec` show schema advertisement,
  model-called `remember`, IPC `tool/call`, and normal `ToolResult`.
- B-AC2: permission denial produces a blocked/error result and fixture side-effect
  counter stays zero.
- B-AC3: hung and crashed plugins produce `ToolError`; the turn completes.
- B-AC4: duplicate builtin/plugin and plugin/plugin names follow documented
  first-wins precedence with warnings.
- B-AC5: no plugin tools registered leaves builtin schemas and builtin tool
  execution unchanged.

Rollback point:

- If final sweep fails from cross-phase interaction, revert the last phase that
  touched production code and rerun the full gate before trying a smaller fix.

## Notes For Reviewers

- The implementation must not change parent protocol frames. Any protocol change
  belongs to a coordinated parent/Child A revision.
- `Action::Plugin` is intentionally generic. Do not add per-plugin or per-tool
  action variants in Child B.
- Provider code should remain untouched for B-AC1 schema advertisement. The
  expected path is `ToolRegistry::schemas()` -> `request_from_messages` ->
  `CompletionRequest.tools`.
- If a plugin dies after bootstrap, the v1 registry still advertises its schema;
  calls fail with `ToolError`. Respawn-aware tool handles are a follow-up, not a
  Child B requirement.

## Plan-review gate

Cross-model plan-review runs at the **parent level** over the full plan set
(parent + A + B + C) before any `task.py start`. Do not start Child B
implementation until that gate passes and the user approves activation.
