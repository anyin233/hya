//! External interoperability contract.
//!
//! This is the single, clearly-labeled home for the literals that mirror the
//! upstream agent tool's external spec — the HTTP headers real clients send, the
//! on-disk config directories/files hya reads to interoperate with an existing
//! install, and (documented below) the external npm scope and plugin-kind alias.
//!
//! These strings are an EXTERNAL CONTRACT, not our branding. They intentionally
//! retain the upstream token so drop-in compatibility keeps working, and they MUST
//! NOT be renamed. Production code references these constants instead of inlining
//! the literals, so the brand's functional footprint stays confined to this file.

/// Request header carrying the caller's working directory.
pub(crate) const DIRECTORY_HEADER: &str = "x-opencode-directory";
/// Request header carrying the caller's workspace root.
pub(crate) const WORKSPACE_HEADER: &str = "x-opencode-workspace";
/// One-time PTY connect ticket header.
pub(crate) const CONNECT_TOKEN_HEADER: &str = "x-opencode-ticket";

/// Project-local config directory the external tool uses (read for interop).
/// Documented here as the canonical contract value; the discovery modules build
/// their `.opencode/<sub>` paths from this base, and tests pin it.
#[allow(dead_code)]
pub(crate) const PROJECT_CONFIG_DIR: &str = ".opencode";
/// Config directory name under `~/.config/` (read for interop).
pub(crate) const GLOBAL_CONFIG_DIR: &str = "opencode";
/// Config file names the external tool writes (read for interop).
pub(crate) const CONFIG_FILE_JSON: &str = "opencode.json";
pub(crate) const CONFIG_FILE_JSONC: &str = "opencode.jsonc";

// Remaining external-brand touchpoints, documented here for a complete audit:
//   * `@opencode-ai/plugin` / `@opencode-ai/sdk` — the external plugin SDK npm
//     packages the Bun adapter depends on (`crates/hya-plugin-compat/adapter/
//     package.json`). Real published packages; renaming breaks `bun install`.
//   * plugin `kind: opencode` — accepted as a serde alias of `PluginKindWire::Compat`
//     (`crates/hya-plugin/src/messages.rs`) so existing user configs keep parsing.
//   * `.opencode/{agents,commands,modes,skills,tools,plugins}` subdirectories and
//     `x-opencode-ticket` variants — the config subdirs derive from
//     `PROJECT_CONFIG_DIR` above; test suites additionally assert the literal wire
//     values to pin the contract.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_values_are_pinned() {
        // Guard against accidental edits to the external contract.
        assert_eq!(DIRECTORY_HEADER, "x-opencode-directory");
        assert_eq!(WORKSPACE_HEADER, "x-opencode-workspace");
        assert_eq!(CONNECT_TOKEN_HEADER, "x-opencode-ticket");
        assert_eq!(PROJECT_CONFIG_DIR, ".opencode");
        assert_eq!(GLOBAL_CONFIG_DIR, "opencode");
        assert_eq!(CONFIG_FILE_JSON, "opencode.json");
        assert_eq!(CONFIG_FILE_JSONC, "opencode.jsonc");
    }
}
