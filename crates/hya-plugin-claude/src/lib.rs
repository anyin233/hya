//! Typed contract for offline Claude Code plugin translation.
//!
//! The Bun/TypeScript adapter under `adapter/` translates Claude plugin directories
//! and marketplace entries. It also runs `kind: claude` hooks using the hya plugin
//! ABI v1 over NDJSON JSON-RPC stdio. See `docs/claude-plugin-import.md`.
//!
//! [`emit`] decodes the `--emit-bundle-manifest` result for
//! `hya bundle install --claude`.

pub mod emit;

/// Historical adapter version constant.
#[deprecated(note = "Query the adapter entry point with --version")]
pub const CLAUDE_ADAPTER_VERSION: &str = "1.0.0";

/// Historical constant for the Claude plugin implementation kind.
#[deprecated(note = "Use PluginKindWire::Claude from hya-plugin for host configuration")]
pub const CLAUDE_PLUGIN_KIND: &str = "claude";

/// Historical constant for the Claude Code plugin manifest filename.
#[deprecated(note = "The Claude adapter owns plugin manifest discovery")]
pub const PLUGIN_MANIFEST_FILE: &str = "plugin.json";
