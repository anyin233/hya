/**
 * File-name constants shared with the Rust `hya-plugin-claude` crate.
 *
 * Keep these in sync with `crates/hya-plugin-claude/src/lib.rs`; the Rust
 * side pins the adapter version and the install-layout names.
 */

/** Claude Code plugin manifest file name inside a plugin source directory. */
export const PLUGIN_MANIFEST_FILE = "plugin.json"

/** Claude Code marketplace manifest file name. */
export const MARKETPLACE_MANIFEST_FILE = "marketplace.json"

/** Claude Code per-plugin metadata directory (`<plugin>/.claude-plugin/`). */
export const CLAUDE_PLUGIN_METADATA_DIR = ".claude-plugin"

/** Directory holding Claude Code hook scripts inside a plugin. */
export const CLAUDE_HOOKS_FILE = "hooks/hooks.json"

/** Claude Code per-project MCP declaration. */
export const CLAUDE_MCP_FILE = ".mcp.json"

/** Plugin implementation kind declared by the adapter on the wire. */
export const CLAUDE_PLUGIN_KIND = "claude"
