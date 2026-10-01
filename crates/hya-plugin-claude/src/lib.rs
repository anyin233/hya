//! Version pin and offline translation contract for the Claude Code adapter.
//!
//! This crate is the Rust-side anchor for `kind: claude` plugin support. The
//! adapter itself lives in [`adapter/`](../../adapter/) as a Bun/TypeScript
//! package that speaks the hya plugin ABI v1 (NDJSON JSON-RPC 2.0 over stdio;
//! see `docs/plugin-protocol.md`) and discovers/translates Claude Code plugin
//! sources (`plugin.json`, `agents/`, `skills/`, `commands/`, `hooks/`,
//! `.mcp.json`, `marketplace.json`).
//!
//! The Rust side owns two things:
//!
//! - [`CLAUDE_ADAPTER_VERSION`] — the adapter package version the host and the
//!   adapter must agree on. Bumping it without shipping the matching adapter
//!   breaks `kind: claude` activation; treat a change as a coordinated
//!   release.
//! - [`emit`] — the typed contract for the adapter's offline
//!   `--emit-bundle-manifest` mode, which `hya bundle install
//!   --claude` consumes to stage an [`crate::emit::ManifestEmit`] through
//!   `hya_bundle::prepare_package`.

pub mod emit;

/// Pinned `@hya/claude-adapter` package version the Bun adapter ships as.
pub const CLAUDE_ADAPTER_VERSION: &str = "1.0.0";

/// Plugin implementation kind declared by the adapter on the wire.
pub const CLAUDE_PLUGIN_KIND: &str = "claude";

/// Claude Code plugin manifest file name inside a plugin source directory.
pub const PLUGIN_MANIFEST_FILE: &str = "plugin.json";

#[cfg(test)]
mod tests {
    use super::{CLAUDE_ADAPTER_VERSION, CLAUDE_PLUGIN_KIND, PLUGIN_MANIFEST_FILE};

    #[test]
    fn pinned_constants_match_adapter_package() {
        let Ok(package) =
            serde_json::from_str::<serde_json::Value>(include_str!("../adapter/package.json"))
        else {
            panic!("Claude adapter package.json must be valid JSON");
        };
        assert_eq!(package["version"].as_str(), Some(CLAUDE_ADAPTER_VERSION));
        assert_eq!(CLAUDE_PLUGIN_KIND, "claude");
        assert_eq!(PLUGIN_MANIFEST_FILE, "plugin.json");
    }
}
