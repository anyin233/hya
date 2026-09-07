# Rustdoc Q8 - `hya-mcp`

You are writing Rust API documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

## Your batch

Crate(s): `hya-mcp`. **71 undocumented public items.** Do not touch any other
crate -- other writers are working in parallel.

`lib.rs` has NO `//!` at all. Write one: what MCP is here, how servers are configured and prepared, and how their tools reach the model namespaced.

Only `McpManager` is documented today. Cover `McpServerConfig`, `prepare`, `McpClient`, `McpError`, `PreparedMcpServer`, `McpStatus`, `McpTool`, `namespaced_tool_name`, and the entire 6-item `protocol.rs` wire module (JsonRpcRequest/Response/Error, ToolInfo, ToolsListResult, ToolCallResult).

## Non-negotiable rules

1. **Read the item before documenting it.** A doc comment that restates the item
   name adds nothing. Say what it is FOR, and for a function, its parameters and
   what it returns.
2. **Do not change code.** No signature, visibility, derive, or behaviour changes.
   Only `///`, `//!`, and `#[doc]` comments. If an item looks like it should not be
   public, leave it public and say so in your report.
3. **Do not run `cargo`.** You do not have approval for shell commands and the
   orchestrator runs the lint to verify. Work from the item list below.
4. **Do not add `#![deny(missing_docs)]`** -- the orchestrator adds it once the
   crate verifies at zero.
5. **Do not run `git commit`.**
6. For a `struct field` or `enum variant`, a single clear line is enough. Spend
   your effort on traits, public entry points, and anything security- or
   protocol-related.
7. If two items mean the same thing, do not paste identical text -- say how they
   differ.

## Item list

Every entry below is a `missing_docs` warning from the compiler, so this list is
exhaustive and mechanical. Grouped by file, with line numbers from the current
tree. Line numbers SHIFT as you insert doc comments -- work bottom-up within a
file, or re-find the item by name.

### `crates/hya-mcp/src/bridge.rs` (3)

14:struct, 22:assoc fn, 47:function

### `crates/hya-mcp/src/client.rs` (20)

15:constant, 16:constant, 22:enum, 24:var, 26:var, 28:var, 30:var, 32:field, 32:field, 32:var, 34:field, 34:var, 36:var, 38:var, 42:struct, 52:struct, 92:assoc fn, 108:assoc fn, 133:method, 168:method

### `crates/hya-mcp/src/lib.rs` (4)

1:module, 2:module, 3:module, 4:module

### `crates/hya-mcp/src/manager.rs` (21)

14:struct, 16:field, 18:field, 20:field, 22:field, 27:enum, 30:var, 31:var, 32:var, 33:field, 35:var, 36:var, 37:field, 54:struct, 64:method, 69:method, 74:method, 79:function, 155:method, 164:method, 169:method

### `crates/hya-mcp/src/protocol.rs` (23)

5:struct, 6:field, 7:field, 8:field, 10:field, 14:struct, 15:field, 16:field, 18:field, 20:field, 24:struct, 25:field, 26:field, 28:field, 33:struct, 34:field, 36:field, 37:field, 41:struct, 42:field, 47:struct, 48:field, 50:field

## When you are done

Report:

1. Which files you touched and roughly how many items you documented.
2. Any item you could NOT document because its purpose was not inferable from the
   code, named as `file:line - Name`.
3. Any public item that in your judgement should not be public. Do not change it.
4. Any code defect you noticed. Do not fix it.
