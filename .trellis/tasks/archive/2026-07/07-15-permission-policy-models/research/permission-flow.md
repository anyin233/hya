# Permission Flow Research

## Existing Contracts

- `hya-tool::PermissionRules` evaluates ordered action/resource wildcard rules; unmatched operations ask and the last matching rule wins (`crates/hya-tool/src/permission.rs`).
- `PermissionPlane::assert` treats snapshot allow/deny as final. Snapshot ask can use remembered approvals, plugin interception, and the existing ask channel.
- Existing wildcard rules are used by compatibility agents, external-directory grants, tests, MCP setup, and remembered `AllowAlways`; they must not be reinterpreted as regex.
- MCP tools assert `Action::Mcp` against their stable `mcp__<server>__<tool>` name (`crates/hya-mcp/src/bridge.rs`).
- Subagent spawning asserts `Action::Task` against each subagent type (`crates/hya-tool/src/task.rs`).
- Plugin tools currently execute without a permission assertion (`crates/hya-plugin/src/plugin_tool.rs`).

## Runtime Flow

- `build_session_engine` creates the shared permission plane and currently allows only `Read`, `Glob`, and `Grep` by default (`crates/hya-app/src/runtime.rs:433`).
- Model tool calls expose the requested name and transformed input before registry lookup/execution (`crates/hya-core/src/engine/turn.rs:231`).
- After lookup, `Tool::name()` provides the canonical registered name, including hidden aliases.
- Direct shell requests use a separate execution path (`crates/hya-core/src/engine/shell.rs:146`).
- Interactive and serve surfaces can expose asks to clients. Headless exec/RPC/goal currently auto-answer asks through `PermissionPolicy::{Scoped,Yolo}` (`crates/hya-app/src/permission.rs`).

## Config Flow

- Native YAML has one typed path: `FileConfig -> ResolvedConfig -> RuntimeConfig -> build_session_engine` (`crates/hya-app/src/config.rs`, `crates/hya-app/src/runtime.rs`).
- `hya-tool` already depends on the workspace `regex` crate.
- The starter file is owned by `DEFAULT_CONFIG_YAML`; public examples live in `docs/configuration.md` and current behavior in `docs/architecture/tools-and-permissions.md`.

## Confirmed Product Contract

- YAML uses `permission.model` and ordered `permission.rules[]` entries with `target`, regex `selector`, and `permission: Allow|Deny|Ask`.
- `tool` matches canonical built-in and plugin names; MCP and shell text use `mcp` and `command` targets.
- `allow`: any matching deny denies; everything else allows.
- `default`: last matching rule wins; unmatched read-only tools and `task` allow, all other tools/MCP/commands ask.
- `strict`: matching denies deny; every other operation asks.
- `danger`: every operation allows and configured rules are ignored.
- `--yolo` is shipped behavior and should remain as a danger-mode override.
- Headless asks must fail closed rather than be silently approved.
- Default-read classification is limited to `read`, `ls`, `glob`, `find`, `grep`, `lsp`, `skill`, `list_agents`, `roster`, and `channels`; `webfetch` and `websearch` remain ask-by-default.

## Design Constraints

- Evaluate exactly one native target per invocation to avoid stacked native prompts.
- Preserve the existing ask/event/remember/interceptor flow.
- Preserve wildcard resource checks, especially external-directory grants.
- Unknown or malformed tool calls should fail before prompting where possible.
- Invalid regex/model/target/permission config must fail through the existing typed config error path.
- A naive dispatch preflight plus existing action assertions double-prompts mutating tools; the design must compose these layers explicitly.
