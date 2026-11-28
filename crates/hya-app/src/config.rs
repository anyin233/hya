//! Build a provider router from hya's own config (`~/.config/hya/config.yaml`).
//!
//! Reads YAML: each entry under `providers.<id>` maps to one `HttpProvider`
//! (route chosen by `kind`), and the union of `models` becomes the set hya can
//! address. API keys come from `~/.config/hya/auth/<id>.yaml` (via `hya login`)
//! or an inline `api_key` in the provider block. `kind: grok-build` always uses
//! CLI chat-proxy session headers with that configured bearer token (self-contained
//! config — it does not read `~/.grok/auth.json`). `kind: openai-codex` targets the
//! ChatGPT Codex backend. OAuth credentials live under `~/.config/hya/auth/` and
//! are auto-refreshed when near expiry. No resolved live model rows → offline.

use std::collections::BTreeMap;
use std::sync::Arc;

use hya_mcp::McpServerConfig;
use hya_plugin::config::PluginEntry;
use hya_provider::{ProviderCatalogSnapshot, ProviderRouter};
use hya_tool::InvocationPolicy;

mod catalog;
mod edit;
mod file;
mod provider;

pub use catalog::{
    DiscoverMode, DiscoveryResult, PendingCatalogDiscovery, ProviderRebuild, load, probe_provider,
    rebuild_providers, refresh_pending_catalogs, store_discovery,
};
pub use edit::{
    ConfigEditError, ModelEntryOverride, ProviderDeclaration, provider_declarations,
    provider_model_references, remove_model_entry, remove_provider_entry, set_model_entry,
    upsert_oauth_provider, upsert_provider_entry,
};
pub use file::{
    BundlesConfig, CONFIG_SCHEMA_VERSION, CodeMode, ContextSettings, CreatedConfig, EngineSettings,
    GoalSettings, active_config_path, configured_builtin_bundles, configured_code_mode,
    ensure_config_file, ensure_config_file_at, expected_config_path, first_run_config_bootstrap,
    load_configured_plugin_ids, resolve_evaluator_model, resolve_event_bus_capacity,
};
pub(crate) use file::{parse_plugin_config, read_bundle_config_json};
pub(crate) use provider::resolve_secret;
pub use provider::{PROVIDER_KIND_LABELS, validate_base_url};

/// Fully loaded Hya config: provider routes, one immutable catalog snapshot,
/// and derived runtime knobs.
pub struct ResolvedConfig {
    /// Ordered HTTP provider routes built from final catalog rows.
    pub router: ProviderRouter,
    /// Shared immutable model/provider startup snapshot.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Row-backed default model id for new sessions.
    pub default_model: String,
    /// Named MCP server configs from `mcp:`.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Named plugin entries from `plugins:` (before manifest merge).
    pub plugins: BTreeMap<String, PluginEntry>,
    /// Preferred primary agent id when workdir does not select one.
    pub default_agent: Option<String>,
    /// Compiled tool permission policy (`permission:` block).
    pub permission: InvocationPolicy,
    /// Providers that still need background discovery refresh after startup.
    pub pending_discovery: Vec<PendingCatalogDiscovery>,
    /// Parsed startup settings passed unchanged to the engine builder.
    pub settings: EngineSettings,
}

#[cfg(test)]
mod tests;
