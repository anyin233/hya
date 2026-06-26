//! `yaca-app` — backend bootstrap library extracted from the `yaca-cli` binary.
//!
//! Holds the engine/`AppState` assembly (config, providers, MCP, plugins, agent, store) so both
//! `yaca-cli` (server + headless commands) and `hya-yaca` (in-process native transport) can build
//! the backend without the binary. Public surface filled in during Phase 1.

pub mod auth;
pub mod config;
pub mod formatter_config;
pub mod permission;
pub mod plugins;
pub mod runtime;
pub mod skills;

pub use runtime::{
    OfflineNotice, RuntimeConfig, RuntimeOptions, YacaRuntime, agent_with_model,
    build_session_engine, compaction_config, discover_context_files, headless_policy, host_info,
    offline_router, open_store, resolve_runtime, skill_dirs, spawn_team_supervisor, today,
};
