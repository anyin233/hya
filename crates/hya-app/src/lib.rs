//! `hya-app` — backend bootstrap library shared by `hya-backend` and other hosts.
//!
//! Assembles the live runtime from config, auth, providers, MCP, plugins, agent
//! base, and the session store: [`resolve_runtime`], [`build_session_engine`],
//! and [`HyaRuntime::start`] are the main entry points. Headless CLI commands
//! and the HTTP server both call into this crate so they share one composition
//! path without duplicating bootstrap glue in each binary.

/// Durable per-Agent model preference control owned by the application layer.
pub mod agent_model_control;
/// Provider credential files under `~/.config/hya/auth/`.
pub mod auth;
/// `config.yaml` load, first-run bootstrap, and Compat import.
pub mod config;
/// Formatter plane construction from optional formatter config.
pub mod formatter_config;
mod installed_bundle_refresh;
/// Interactive OAuth login and access-token refresh.
pub mod oauth;
/// Headless permission auto-reject responder for non-interactive runs.
pub mod permission;
/// Resolve plugin specs from config and `.hya/plugins` manifests.
pub mod plugins;
/// Runtime assembly: store, engine, team supervisor, and [`HyaRuntime`].
pub mod runtime;
mod runtime_reconcile;
mod spawn_intent;
/// Shared Workflow discovery, selection, state, and execution control.
pub mod workflow_control;

pub use agent_model_control::{
    AgentModelControlError, AgentModelIdentity, PersistentAgentModelControl,
};
pub use hya_proto::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowInfo, WorkflowRunResult,
    WorkflowStageInfo, WorkflowSummary,
};
pub use hya_tool::{InvocationPolicy, WebSearchConfig};
pub use installed_bundle_refresh::{
    InstalledBundleRefresh, bundle_registry_path, first_party_catalog,
};
pub use runtime::{
    BuiltSessionEngine, HARNESS_AGENT_BASE, HyaRuntime, OfflineNotice, RuntimeConfig,
    RuntimeOptions, agent_base_with_model, agent_with_model, build_session_engine,
    builtin_agent_catalog, compaction_config, discover_context_files, host_info, offline_router,
    open_store, resolve_runtime, spawn_team_supervisor, today, with_built_session_engine,
};
pub use workflow_control::{
    FIRST_PARTY_BUNDLE_ID, ResolvedWorkflow, WorkflowCatalog, WorkflowCatalogOwner,
    WorkflowCatalogRoots, WorkflowControl, WorkflowControlError, WorkflowInvocation,
};
