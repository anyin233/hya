//! `hya-app` — backend bootstrap library shared by `hya` and other hosts.
//!
//! Assembles the live runtime from config, auth, providers, MCP, plugins, agent
//! base, and the session store: [`resolve_runtime`], [`build_session_engine`],
//! and [`HyaRuntime::start`] are the main entry points. Headless CLI commands
//! and the HTTP server both call into this crate so they share one composition
//! path without duplicating bootstrap glue in each binary.

/// Configuration-file model defaults for built-in and installed-bundle Agents.
pub mod agent_model_config;
/// Durable per-Agent model preference control owned by the application layer.
pub mod agent_model_control;
/// Provider credential files under `~/.config/hya/auth/`.
pub mod auth;
/// Per-bundle `config.yml` location, environment contract, and scope resolver.
pub mod bundle_admin;
pub mod bundle_config;
mod bundle_install;
/// SQLite registry and install types for user bundles.
pub mod bundle_registry;
mod bundle_runtime;
/// Core Code Mode runtime promoted into `hya-bare` (ADR-0039).
pub mod code_mode;
/// `config.yaml` load, first-run bootstrap, and Compat import.
pub mod config;
mod config_edit;
mod headless;
mod host_runtime;
mod installed_bundle_refresh;
mod lead_wrapup;
/// SQLite remote-model cache (`$XDG_CACHE_HOME/hya/model_cache.db`).
pub mod model_cache;
/// Interactive OAuth login and access-token refresh.
pub mod oauth;
/// Headless permission auto-reject responder for non-interactive runs.
pub mod permission;
/// Adapt plugin process handles to the engine runtime.
pub mod plugin_adapter;
/// Resolve plugin specs from config and per-root `.hya/plugins` manifests.
pub mod plugins;
mod preset_inventory;
pub mod project_bundles;
pub mod project_plugins;
mod project_scope;
/// Live provider management behind the v1 provider routes.
pub mod provider_control;
/// Runtime assembly: store, engine, team supervisor, and [`HyaRuntime`].
pub mod runtime;
mod runtime_extensions_refresh;
mod runtime_mcp_control;
mod runtime_mcp_source;
mod runtime_reconcile;
/// Opt-in `HYA_STARTUP_TRACE` phase marks for `hya serve` startup.
pub mod startup_trace;
#[cfg(test)]
mod test_env;
mod workflow_inspect;

pub use agent_model_control::{
    AgentModelControlError, AgentModelIdentity, PersistentAgentModelControl,
};
pub use headless::{
    DEFAULT_LOOP_BUDGET, GOAL_DRIVER_UNAVAILABLE_MSG, HARD_MAX_LOOP_BUDGET, HeadlessEvaluatorRoute,
    LoopPredicateMode, LoopPredicateSpec, LoopRunOptions, clamp_loop_budget, prepare_goal_driver,
    prepare_loop, resolve_goal_driver,
};
pub use hya_coding_extras::BuiltinFormatterProvider;
/// Formatter plane construction from optional formatter config (owned by `hya/coding-extras`).
pub use hya_coding_extras::formatter::config as formatter_config;
pub use hya_proto::{WorkflowInfo, WorkflowStageInfo, WorkflowSummary};
pub use hya_tool::{
    InvocationPolicy, WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowRunResult,
};
pub use hya_workflow_bundle::{
    FIRST_PARTY_BUNDLE_ID, ResolvedWorkflow, WorkflowCatalog, WorkflowCatalogOwner,
    WorkflowCatalogRoots, WorkflowControlError, WorkflowInvocation,
};
pub use installed_bundle_refresh::{
    InstalledBundleRefresh, bundle_registry_path, first_party_catalogs, first_party_catalogs_for,
};
pub use lead_wrapup::run_headless_lead;
pub use preset_inventory::{
    TrustedPresetDescriptor, trusted_preset_inventory, trusted_preset_inventory_for,
};
pub use project_scope::ProjectScopeRefresh;
pub use provider_control::ProviderManager;
pub use runtime::{
    BUILTIN_DEFAULT_AGENT_ID, BuiltSessionEngine, HARNESS_AGENT_BASE, HyaRuntime, OfflineNotice,
    RuntimeConfig, RuntimeOptions, RuntimeStoreOpenError, RuntimeTaskOwner, ServerEngine,
    ServerRuntime, agent_base_for_runtime, agent_base_with_model, agent_for_runtime,
    agent_with_model, agent_with_model_pure, bare_tool_registry, build_session_engine,
    build_session_engine_pure, builtin_agent_catalog, builtin_agent_catalog_for,
    compose_builtin_tool_registry, discover_context_files, host_info, offline_router,
    open_runtime_store, open_runtime_store_or_memory, open_store, open_store_or_memory,
    resolve_headless_agent_name, resolve_runtime, spawn_team_supervisor, today,
    with_built_session_engine,
};
pub use workflow_inspect::inspect_workflow_catalog_read_only;

#[cfg(test)]
#[path = "../../hya-engine/tests/it/support/resident.rs"]
mod resident_fixture;
