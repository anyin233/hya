//! `hya-app` — backend bootstrap library extracted from the `hya-backend` binary.
//!
//! Holds the engine/`AppState` assembly (config, providers, MCP, plugins, agent, store) so both
//! `hya-backend` (server + headless commands) and `hya-native` (in-process native transport) can build
//! the backend without the binary. Public surface filled in during Phase 1.

pub mod auth;
pub mod config;
pub mod formatter_config;
mod installed_bundle_refresh;
pub mod oauth;
pub mod permission;
pub mod plugins;
pub mod runtime;
mod runtime_reconcile;
mod spawn_intent;

pub use hya_tool::{InvocationPolicy, WebSearchConfig};
pub use installed_bundle_refresh::{InstalledBundleRefresh, bundle_registry_path};
pub use runtime::{
    BuiltSessionEngine, HARNESS_AGENT_BASE, HyaRuntime, OfflineNotice, RuntimeConfig,
    RuntimeOptions, agent_base_with_model, agent_with_model, build_session_engine, builtin_catalog,
    compaction_config, discover_context_files, host_info, offline_router, open_store,
    resolve_runtime, spawn_team_supervisor, today, with_built_session_engine,
};
