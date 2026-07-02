# Plugin-registered tools (Child B)

> Parent: [06-21-plugin-system](../06-21-plugin-system/prd.md). Shared decisions,
> architecture, and the IPC protocol are defined by the parent + Child A
> ([06-21-plugin-host-core](../06-21-plugin-host-core/prd.md)). This child adds the
> tool-extension capability on top of that protocol/host.

## Goal

Let a plugin contribute **new model-callable tools**: the plugin declares tools
(name + JSON schema) at registration; yaca advertises them to the model alongside
builtins; when the model calls one, a plugin-tool proxy dispatches the call over
the IPC protocol to the owning plugin and returns its JSON result, all under the
normal permission plane. Mirrors Compat's `tool:` registration.

## Scope (owns)

Parent requirement: **R5** (and the protocol's tool-call request/reply frames,
which Child A defines but B exercises end-to-end).

## Dependency ordering

- **Depends on Child A** (the host/manager + protocol + registration handshake +
  the tool-call frames). Execute **after** A lands.
- Does not depend on Child C; Child C reuses B's plugin tools through the same
  registry path.

## Requirements

- **B-R1 (R5).** Register plugin tools at bootstrap via the **already-existing**
  `ToolRegistry::register` ([tool.rs:94](../../../crates/yaca-tool/src/tool.rs#L94),
  already used by MCP) — the registry is **not** static, so no new registry
  primitive is required. Add at most an optional `extend(...)` convenience wrapper;
  the `builtins()` path is unchanged.
- **B-R2 (R5).** A `PluginTool` proxy implementing `trait Tool`
  (`name`/`schema`/`execute`) that, on `execute`, sends a tool-call request to the
  owning plugin over the protocol and awaits the JSON result, mapping protocol
  errors to `ToolError`.
- **B-R3 (R5).** Plugin tool schemas are advertised to the model exactly like
  builtin schemas (so providers pick them up automatically via
  `request_from_messages` → `tools.schemas()`).
- **B-R4 (R5).** Plugin tools run **under the permission plane**: a plugin tool
  call still flows through `ctx.permission.assert(...)`; define the `Action`/
  `Resource` mapping for plugin tools (and how `permission.ask`/posture interact).
- **B-R5 (timeout/posture).** A plugin-tool call has its own timeout and failure
  mapping (a hung/crashed plugin yields a `ToolError`, not a hung turn) consistent
  with Child A's host lifecycle.
- **B-R6 (naming).** Define collision behavior when a plugin tool name equals a
  builtin or another plugin's tool (recommend: explicit precedence + a warning;
  decide in design.md).
- **B-R7 (R9/R10).** Quality gate green; no regression to builtin tools when no
  plugin tools are registered.

## Acceptance criteria

- [ ] **B-AC1 (AC2).** An example plugin registers a tool; its schema appears in
      the provider request; the model can call it; the call round-trips over IPC
      and the result appears as a normal `ToolResult` — test + live `yaca exec` QA.
- [ ] **B-AC2.** A plugin tool call is permission-checked: denying it yields a
      `ToolError`/blocked result, not an executed side effect.
- [ ] **B-AC3 (B-R5).** A plugin that hangs/crashes during a tool call yields a
      `ToolError` and the turn continues; no hang.
- [ ] **B-AC4 (B-R6).** Name-collision behavior matches the documented precedence
      (asserted by test).
- [ ] **B-AC5 (R10).** With no plugin tools registered, builtin tools behave
      identically (regression test) and schemas are unchanged.

## Out of scope

- Hook interception (Child A), Compat adapter (Child C), out-of-process
  providers. Plugin tools that need new permission `Action` kinds beyond a generic
  plugin-tool action are deferred unless design.md shows a concrete need.

## Notes

- Complex task: `design.md` + `implement.md` required before `task.py start`.
- Reuses Child A's protocol tool-call frames; if those frames need changes, treat
  it as a coordinated protocol revision with Child A.
