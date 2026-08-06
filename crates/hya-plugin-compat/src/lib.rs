//! Version pins for the Compat/OpenCode JS plugin adapter.
//!
//! This crate does not implement protocol logic. It only publishes the two
//! package versions the Bun adapter and host config must agree on:
//!
//! - [`COMPAT_PLUGIN_VERSION`] — `@opencode-ai/plugin` package version
//! - [`COMPAT_SDK_VERSION`] — `@opencode-ai/sdk` package version
//!
//! Bumping either constant without also shipping a matching adapter (and
//! re-verified hooks/tools translation) breaks Compat plugin load: the adapter
//! may import APIs that no longer exist, or the host may reject an unexpected
//! plugin SDK surface. Treat a change here as a coordinated release.

/// Pinned `@opencode-ai/plugin` version the Compat Bun adapter is built against.
pub const COMPAT_PLUGIN_VERSION: &str = "1.17.9";

/// Pinned `@opencode-ai/sdk` version the Compat Bun adapter and TUI share.
pub const COMPAT_SDK_VERSION: &str = "1.17.9";
