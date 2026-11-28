//! Backend runtime bootstrap: config → store → session engine → optional HTTP router.
//!
//! Call [`resolve_runtime`] (or build a [`RuntimeConfig`] offline), open a store with
//! [`open_store`], assemble a [`BuiltSessionEngine`] via [`build_session_engine`], or
//! use [`HyaRuntime::start`] for the full server-ready process path.

// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use hya_bundle::BundleCatalog;
use hya_engine::agent_catalog::AgentCatalog;
#[cfg(test)]
use hya_engine::resident::resolve_spawn_member;
use hya_engine::resident::{
    ResolveSpawnMemberCtx, resolve_main_activation_context, resolve_spawn_member_with_catalog,
};
use hya_engine::{
    AgentSpec, BoundSpawnRequest, BoundSpawnSender, CategoryRegistry, CoreError, DRAIN_DEADLINE,
    EventBus, ModelSummarizer, ResidentSupervisor, RuntimeRegistry, RuntimeSourceKind,
    SessionEngine, SubagentGovernor, Summarizer, TerminalHandoffOptions, TokenAccounting,
    TurnDrainReport, run_lifecycle_service, run_mailbox_service,
};

// Single discovery/date implementation lives in hya-engine; re-export for callers.
pub use hya_engine::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::HostInfo;
use hya_plugin::config::PluginSpec;
use hya_proto::{AgentName, FinishCause, ModelRef, OwnerRunId, SessionId};
use hya_provider::{DevProvider, ProviderCatalogSnapshot, ProviderRouter, ReasoningEffort};
use hya_store::SessionStore;
use hya_tool::{
    Action, AliasVisibility, AskRequest, InteractionPlane, InvocationPolicy, LifecyclePlane,
    MailboxPlane, Mode, PermissionModel, PermissionPlane, PermissionRules, QuestionRequest, Rule,
    SpawnError, ToolPermission, ToolRegistry,
};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

use crate::agent_model_control::PersistentAgentModelControl;
use crate::bundle_runtime::sidecar::environment::BundleSidecarEnvironment;
use crate::config;
use crate::runtime_extensions_refresh::RuntimeExtensionsRefresh;
use crate::runtime_mcp_control::RuntimeMcpControl;
use crate::runtime_reconcile::{DesiredSource, RuntimeReconciler, SourceId};
use crate::{InstalledBundleRefresh, bundle_registry_path, plugins};

/// Catalog materialization and export validation for Bundle sidecars.
pub use crate::bundle_runtime::sidecar;

/// Whether `hya/core-agents` is both enabled by `selection` and present on disk.
pub(crate) fn core_agents_available(selection: &hya_bundle::BuiltinBundleSelection) -> bool {
    selection.is_enabled("hya/core-agents")
        && hya_bundle::first_party_bundle_optional("hya/core-agents")
            .ok()
            .flatten()
            .is_some()
}

/// Build an [`AgentCatalog`] over `bundles` honoring `selection`: uses
/// `hya/core-agents` when enabled and present, otherwise the minimal compiled-in
/// `hya-bare` default agent (zero first-party bundle loads).
pub(crate) fn agent_catalog_for_selection(
    bundles: Arc<BundleCatalog>,
    selection: &hya_bundle::BuiltinBundleSelection,
) -> Result<AgentCatalog, hya_bundle::BundleError> {
    if core_agents_available(selection) {
        AgentCatalog::new(bundles)
    } else {
        AgentCatalog::new_bare(bundles)
    }
}

/// Agent catalog for a process before installed bundles are refreshed.
///
/// Honors the active `config.yaml` [`hya_bundle::BuiltinBundleSelection`].
pub fn builtin_agent_catalog() -> anyhow::Result<Arc<AgentCatalog>> {
    builtin_agent_catalog_for(&crate::config::configured_builtin_bundles())
}

/// Agent catalog for an explicit [`hya_bundle::BuiltinBundleSelection`].
///
/// Under `BuiltinBundleProfile::Bare` (or when `bundles/builtin/` is absent),
/// loads zero first-party bundles and publishes only the minimal `hya-bare`
/// default agent.
pub fn builtin_agent_catalog_for(
    selection: &hya_bundle::BuiltinBundleSelection,
) -> anyhow::Result<Arc<AgentCatalog>> {
    let first_party = crate::installed_bundle_refresh::first_party_catalogs_for(selection)
        .context("decode embedded first-party bundles")?;
    let first_party_refs = first_party.iter().collect::<Vec<_>>();
    let bundles = BundleCatalog::from_verified_catalogs(&first_party_refs)
        .context("build first-party bundle catalog")?;
    let catalog = agent_catalog_for_selection(Arc::new(bundles), selection)
        .context("build agent catalog over built-ins")?;
    Ok(Arc::new(catalog))
}

/// Register the six registry-backed standard `hya-bare` primitives (`read`,
/// `write`, `edit`, `bash`, `grep`, `skill`) whenever an overriding tool-family
/// bundle has not already registered them. `todo` and `rewrite` remain
/// permanently kernel-exclusive and are never registered in [`ToolRegistry`].
fn register_missing_bare_tools(registry: &hya_tool::ToolRegistry) {
    for tool in hya_core::kernel::tools::bare_tools() {
        let name = tool.name();
        if hya_core::kernel::tools::is_reserved(name) || registry.resolve(name).is_some() {
            continue;
        }
        let (permission, aliases): (ToolPermission, &[(&str, AliasVisibility)]) = match name {
            "read" | "grep" | "skill" => (ToolPermission::ReadOnly, &[]),
            "bash" => (
                ToolPermission::Command,
                &[("shell", AliasVisibility::Hidden)],
            ),
            _ => (ToolPermission::Tool, &[]),
        };
        registry.register_builtin(tool, permission, aliases.iter().copied(), "hya/bare");
    }
}

/// Standalone [`hya_tool::ToolRegistry`] containing the six registry-backed
/// standard `hya-bare` tools (`read`, `write`, `edit`, `bash`, `grep`, `skill`).
/// Combined with the two kernel-exclusive primitives (`todo`, `rewrite`), this
/// forms the complete 8-tool `hya-bare` surface (ADR-0039).
#[must_use]
pub fn bare_tool_registry() -> hya_tool::ToolRegistry {
    let registry = hya_tool::ToolRegistry::default();
    register_missing_bare_tools(&registry);
    registry
}

/// Host identity sent to plugins during `initialize` (`name` + crate version).
pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: hya_version::BACKEND_VERSION.to_string(),
    }
}

/// Dev/offline provider router and model id when no live config is usable.
///
/// The router claims only `hya/offline`; an explicit unknown override remains
/// unknown and therefore fails through the normal typed routing error.
pub fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "hya/offline".to_string()),
    )
}

/// Canonical Harness agent base string (no Environment / AGENTS).
pub const HARNESS_AGENT_BASE: &str = "You are hya, a coding agent.";

/// Built-in root-session agent id used when neither an explicit override nor
/// config `default_agent` selects one. Matches `serve`'s own fallback
/// (`ServerState.default_agent`, then this same built-in agent).
pub const BUILTIN_DEFAULT_AGENT_ID: &str = hya_engine::MAIN_AGENT_ID;

/// Agent base for server and interactive TUI sessions.
///
/// The server has no working directory. The turn harness records initial
/// context from each session's workdir.
pub fn agent_base_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt: HARNESS_AGENT_BASE.to_string(),
        workdir: PathBuf::new(),
        reasoning,
    }
}

/// The caller's working directory, absolute, for the client-side commands
/// (`hya exec`/`run`/`-p`/`loop`/`goal`) that work where the user runs them.
/// They name it explicitly as their session's workdir; the server never
/// falls back to its own cwd (ADR-0024).
fn caller_workdir() -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| std::path::absolute(cwd).ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Agent base for server and interactive TUI sessions using `runtime`'s active
/// built-in bundle selection (`hya-bare` when `hya/core-agents` is disabled or
/// absent, otherwise `hya-main`).
pub fn agent_base_for_runtime(
    runtime: &RuntimeConfig,
    reasoning: Option<ReasoningEffort>,
) -> AgentSpec {
    let name = if core_agents_available(runtime.settings.builtin_bundles()) {
        BUILTIN_DEFAULT_AGENT_ID
    } else {
        hya_engine::BARE_AGENT_ID
    };
    AgentSpec {
        name: AgentName::new(name),
        ..agent_base_with_model(&runtime.model, reasoning)
    }
}

/// Agent base for direct-mode commands, honoring `runtime`'s active built-in
/// bundle selection (`hya-bare` in bare mode, `hya-main` in full mode).
pub fn agent_for_runtime(runtime: &RuntimeConfig, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        workdir: caller_workdir(),
        ..agent_base_for_runtime(runtime, reasoning)
    }
}

/// Agent base for direct-mode commands, with the caller's working directory.
pub fn agent_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        workdir: caller_workdir(),
        ..agent_base_with_model(model, reasoning)
    }
}

/// Agent base for direct `--pure` commands. The pure runtime omits external
/// project instructions when it records the session's initial context.
pub fn agent_with_model_pure(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    agent_with_model(model, reasoning)
}

/// Resolve which agent id a headless root session (`exec`/`run`, `-p` goal
/// mode, `loop`, `rpc`) should use, mirroring `serve`'s own precedence:
/// explicit override (a CLI flag, when the caller has one) first, then config
/// `default_agent`, then the built-in [`BUILTIN_DEFAULT_AGENT_ID`].
///
/// Unlike `serve` (which resolves lazily per HTTP session-create request and
/// can silently keep serving other sessions on failure), a headless run has
/// exactly one root session: an unresolvable candidate fails the whole
/// invocation up front with the same typed error `serve` uses
/// (`BundleError::UnknownAgentId`) rather than silently falling back to the
/// built-in agent.
///
/// # Errors
/// Propagates [`SessionEngine::bind_root_runtime`] failures, and returns
/// `CoreError::Bundle(BundleError::UnknownAgentId)` when the resolved
/// candidate is not a selectable agent in the bound catalog.
pub async fn resolve_headless_agent_name(
    engine: &SessionEngine,
    workdir: &Path,
    explicit: Option<&str>,
    default_agent: Option<&str>,
) -> Result<AgentName, CoreError> {
    let binding = engine.bind_root_runtime(workdir).await?;
    let candidate = explicit
        .or(default_agent)
        .unwrap_or_else(|| binding.agent_catalog().default_agent_id());
    binding
        .resolve_agent(candidate)
        .map(|definition| AgentName::new(definition.stable_id))
        .ok_or_else(|| {
            CoreError::bundle(hya_bundle::BundleError::UnknownAgentId {
                agent_id: candidate.to_string(),
            })
        })
}

/// First-run guidance produced when no usable config is found and hya falls
/// back to the offline echo provider.
///
/// It is carried as *data* on [`RuntimeConfig`] rather than printed at the point
/// of resolution: that keeps it out of machine-readable surfaces (JSONL RPC,
/// `exec`/`-p` piping, `serve`), which never call [`OfflineNotice::emit`]. Only
/// interactive startup paths surface it, and always to stderr — never stdout.
pub struct OfflineNotice {
    /// Where a config file is expected (and should be created).
    pub config_path: PathBuf,
}

impl OfflineNotice {
    /// Render the multi-line guidance: what happened, that hya is offline, and
    /// how to connect a real model.
    #[must_use]
    pub fn render(&self) -> String {
        let path = self.config_path.display();
        format!(
            "hya: no usable provider config found at {path}\n\
             hya: running in OFFLINE mode — the built-in provider only echoes input, \
             so models cannot reason or use tools.\n\
             hya: to connect a real model, edit {path} (see docs/configuration.md)\n\
             hya:   and/or save a provider token with `hya login <provider> <token>`."
        )
    }

    /// Print the notice to stderr so it never corrupts machine-readable stdout.
    pub fn emit(&self) {
        eprintln!("{}", self.render());
    }
}

/// Resolved provider/MCP/plugin/permission inputs ready to build a session engine.
pub struct RuntimeConfig {
    /// Live model routes (or the canonical offline provider).
    pub router: ProviderRouter,
    /// Shared immutable startup model/provider catalog.
    pub catalog: Arc<ProviderCatalogSnapshot>,
    /// Active request model id for new sessions.
    pub model: String,
    /// MCP server configs to connect at engine build.
    pub mcp: BTreeMap<String, McpServerConfig>,
    /// Plugin specs from `config.yaml` (process-wide). Project plugins
    /// (`<root>/.hya/plugins`) load per Project at its first bind.
    pub plugins: Vec<PluginSpec>,
    /// Preferred primary agent when workdir does not select one.
    pub default_agent: Option<String>,
    /// Set when no usable config or live provider was found.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
    /// Tool permission policy for the engine.
    pub permission: InvocationPolicy,
    /// Empty-`models` providers awaiting background discovery refresh.
    pub pending_discovery: Vec<crate::config::PendingCatalogDiscovery>,
    /// `--pure`: load no external AGENTS.md context, MCP servers, or plugins.
    /// Websearch keeps its own configuration; builtin tools and the embedded
    /// skill catalog are unaffected.
    pub pure: bool,
    /// Parsed startup settings for engine composition.
    pub settings: config::EngineSettings,
}

impl RuntimeConfig {
    /// When `yolo` is true, force [`PermissionModel::Danger`] (auto-approve all tools).
    #[must_use]
    pub fn with_yolo(mut self, yolo: bool) -> Self {
        if yolo {
            self.permission = self.permission.with_model(PermissionModel::Danger);
        }
        self
    }

    /// `--pure` mode: load no external MCP servers or plugins (config is
    /// cleared), and mark the runtime so engine builds and per-turn guidance
    /// skip external context. Websearch keeps its own configuration; builtin
    /// tools and the embedded skill catalog are unaffected.
    #[must_use]
    pub fn with_pure(mut self, pure: bool) -> Self {
        if pure {
            self.mcp.clear();
            self.plugins.clear();
        }
        self.pure = pure;
        self
    }

    /// `--bare` mode (`BuiltinBundleProfile::Bare`): disable all built-in
    /// first-party bundles, use the 8 standard bare tools (`read`, `write`,
    /// `edit`, `bash`, `grep`, `todo`, `rewrite`, `skill`), core `code-mode`
    /// without `agent()`, and the minimal `hya-bare` default agent while
    /// keeping `AGENTS.md`, filesystem skills, MCP, and user/project bundles
    /// and plugins active (unless `--pure` is also set).
    #[must_use]
    pub fn with_bare(mut self, bare: bool) -> Self {
        if bare {
            self.settings
                .set_builtin_bundles(hya_bundle::BuiltinBundleSelection::bare());
            self.default_agent = None;
        }
        self
    }
}
fn offline_runtime(model_override: Option<String>, strict: bool) -> RuntimeConfig {
    let (router, model) = offline_router(model_override);
    let catalog = Arc::new(ProviderCatalogSnapshot::build(
        Vec::new(),
        Vec::new(),
        Some(ModelRef::new("hya/offline")),
    ));
    let router = router.with_catalog_snapshot(Arc::clone(&catalog));
    RuntimeConfig {
        router,
        catalog,
        model,
        mcp: BTreeMap::new(),
        plugins: Vec::new(),
        default_agent: None,
        offline_notice: Some(OfflineNotice {
            config_path: config::expected_config_path(),
        }),
        permission: if strict {
            InvocationPolicy::default().with_model(PermissionModel::Strict)
        } else {
            InvocationPolicy::default()
        },
        pending_discovery: Vec::new(),
        settings: config::EngineSettings::default(),
        pure: false,
    }
}

/// Resolve providers and the immutable catalog before returning runtime state.
/// A configuration error falls back to the offline provider (with a notice).
pub async fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match resolve_runtime_strict(model_override.clone()).await {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("hya: config error ({error:#}); using the offline provider");
            offline_runtime(model_override, true)
        }
    }
}

/// [`resolve_runtime`] without the offline fallback: a configuration error
/// is returned rather than composing the offline provider.
///
/// # Errors
///
/// The configuration could not be loaded.
async fn resolve_runtime_strict(model_override: Option<String>) -> anyhow::Result<RuntimeConfig> {
    let Some(cfg) = config::load().await? else {
        return Ok(offline_runtime(model_override, false));
    };
    let model = model_override
        .or_else(|| std::env::var("HYA_MODEL").ok())
        .unwrap_or_else(|| cfg.default_model.clone());
    let offline_notice = cfg.catalog.notice().map(|_| OfflineNotice {
        config_path: config::expected_config_path(),
    });
    Ok(RuntimeConfig {
        router: cfg.router,
        catalog: cfg.catalog,
        model,
        mcp: cfg.mcp,
        plugins: plugins::resolve(cfg.plugins),
        default_agent: cfg.default_agent,
        offline_notice,
        permission: cfg.permission,
        pending_discovery: cfg.pending_discovery,
        settings: cfg.settings,
        pure: false,
    })
}

/// A runtime store open failure with its SQLite or ownership error as the source.
#[derive(Debug, thiserror::Error)]
#[error("{context}")]
pub struct RuntimeStoreOpenError {
    context: String,
    #[source]
    source: hya_store::StoreError,
}

impl RuntimeStoreOpenError {
    /// Whether another runtime owns the database before SQLite opens.
    #[must_use]
    pub fn is_owner_busy(&self) -> bool {
        matches!(self.source, hya_store::StoreError::RuntimeOwnerBusy)
    }

    fn into_anyhow(self) -> anyhow::Error {
        anyhow::Error::new(self.source).context(self.context)
    }
}

/// Reserve ownership and open a SQLite runtime store. File-open failures and
/// timeouts are returned to the caller.
pub async fn open_runtime_store(db: &str) -> Result<SessionStore, RuntimeStoreOpenError> {
    open_runtime_store_with_fallback(db, false).await
}

/// Reserve ownership before opening a runtime store, falling back to memory on
/// open timeout or storage fault. Contention remains an error.
pub async fn open_runtime_store_or_memory(db: &str) -> Result<SessionStore, RuntimeStoreOpenError> {
    let store = open_runtime_store_with_fallback(db, true).await?;
    if let Some((path, reason)) = store.memory_fallback() {
        eprintln!(
            "warning: could not open store at {} ({reason}); running in memory — nothing will be persisted",
            path.display()
        );
    }
    Ok(store)
}

async fn open_runtime_store_with_fallback(
    db: &str,
    fallback: bool,
) -> Result<SessionStore, RuntimeStoreOpenError> {
    let opened = match (db, fallback) {
        ("", _) => SessionStore::connect_memory().await,
        (_, true) => SessionStore::connect_runtime_or_memory(db).await,
        (_, false) => SessionStore::connect_runtime(db).await,
    };
    opened.map_err(|source| RuntimeStoreOpenError {
        context: if db.is_empty() {
            "open in-memory store".to_owned()
        } else {
            format!("open store at {db}")
        },
        source,
    })
}

/// Open a runtime store strictly, returning the failure through `anyhow`.
/// Use [`open_runtime_store`] to handle owner contention directly.
pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    open_runtime_store(db)
        .await
        .map_err(RuntimeStoreOpenError::into_anyhow)
}

/// Open a runtime store with memory fallback, returning failures through
/// `anyhow`. Use [`open_runtime_store_or_memory`] to handle contention directly.
pub async fn open_store_or_memory(db: &str) -> anyhow::Result<SessionStore> {
    open_runtime_store_or_memory(db)
        .await
        .map_err(RuntimeStoreOpenError::into_anyhow)
}

pub use hya_server::RuntimeTaskOwner;

/// Public aggregate returned by [`build_session_engine`].
///
/// Owns engine products plus composition services and request handlers. Not `Clone`.
/// Callers must keep this value until they finish using the engine and then call
/// [`shutdown`](BuiltSessionEngine::shutdown) (or drop for a nonblocking fallback).
#[must_use]
pub struct BuiltSessionEngine {
    engine: Arc<SessionEngine>,
    resident_supervisor: Arc<ResidentSupervisor>,
    agent_model_control: PersistentAgentModelControl,
    asks: Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>>,
    questions: Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>>,
    mcp_control: Arc<dyn hya_server::McpControl>,
    host_runtime: Arc<dyn hya_runtime_abi::HostRuntime>,
    native_services: Arc<[Arc<dyn hya_runtime_abi::NativeService>]>,
    lifecycle: RuntimeTaskOwner,
    runtime_owner: Option<OwnerRunId>,
}

impl BuiltSessionEngine {
    /// Shared session engine handle.
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        Arc::clone(&self.engine)
    }

    /// Host runtime handle backing ABI v2 native bundles and drivers.
    #[must_use]
    pub fn host_runtime(&self) -> Arc<dyn hya_runtime_abi::HostRuntime> {
        Arc::clone(&self.host_runtime)
    }

    /// Registered native [`hya_runtime_abi::GoalDriver`], or `None` when
    /// `hya/goal` is disabled or running under `--bare`.
    #[must_use]
    pub fn goal_driver(&self) -> Option<Arc<dyn hya_runtime_abi::GoalDriver>> {
        self.host_runtime.goal_driver()
    }

    /// Shared resident scheduling owner used by Workflow actor activations.
    #[must_use]
    pub fn resident_supervisor(&self) -> Arc<ResidentSupervisor> {
        Arc::clone(&self.resident_supervisor)
    }

    /// Shared durable Agent model preference control for backend and server surfaces.
    #[must_use]
    pub fn agent_model_control(&self) -> PersistentAgentModelControl {
        self.agent_model_control.clone()
    }

    /// Resolve the effective model for a new root Session without an explicit override.
    ///
    /// The supplied workdir binds one current Agent catalog/preference snapshot.
    /// Configured direct/category policy and an eligible remembered model are
    /// applied before the process base model.
    ///
    /// # Errors
    /// Returns runtime binding, unknown-Agent, or model-control failures.
    pub async fn effective_root_model(
        &self,
        agent: &AgentSpec,
        workdir: &Path,
    ) -> anyhow::Result<ModelRef> {
        let binding = self
            .engine
            .bind_root_runtime(workdir)
            .await
            .context("bind root runtime for Agent model resolution")?;
        self.agent_model_control
            .effective_model(&binding, agent.name.as_str(), &agent.model)
            .context("resolve effective root Agent model")
    }

    /// Take the permission-ask receiver exactly once.
    pub fn take_asks(&mut self) -> Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>> {
        self.asks.take()
    }

    /// Take the interaction-question receiver exactly once.
    pub fn take_questions(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>> {
        self.questions.take()
    }

    /// MCP control plane handle.
    #[must_use]
    pub fn mcp_control(&self) -> Arc<dyn hya_server::McpControl> {
        Arc::clone(&self.mcp_control)
    }

    /// Return the active [`hya_runtime_abi::LoopDriver`] registered by `hya/loop`,
    /// or `None` when `hya/loop` is disabled or running in `--bare` mode.
    #[must_use]
    pub fn loop_driver(&self) -> Option<Arc<dyn hya_runtime_abi::LoopDriver>> {
        self.host_runtime.loop_driver()
    }

    /// Admit every daemon restore before starting recovered resident episodes.
    /// The restore turn owns its root lease before recovered child mail can wake main.
    pub async fn resume_interrupted(
        &self,
        state: &hya_server::AppState,
    ) -> Result<Vec<SessionId>, CoreError> {
        let restored = state.resume_interrupted_turns().await;
        let residents = self.resident_supervisor.start_recovered().await?;
        crate::startup_trace::mark("residents_recovered", Some(&residents.to_string()));
        Ok(restored)
    }

    /// Graceful stop of every in-flight turn in every session (see
    /// [`ResidentSupervisor::drain`]): new turns are refused, active turns are
    /// cancelled with `cause` and given [`DRAIN_DEADLINE`] to close their
    /// messages, then team members are made terminal and each lead is parked.
    /// Stops and joins composition intake and registrations before draining
    /// residents. The owned future can be passed to a server shutdown callback.
    /// Idempotent; the first cause wins.
    pub fn drain(
        &self,
        cause: FinishCause,
    ) -> impl std::future::Future<Output = TurnDrainReport> + Send + 'static + use<> {
        let engine = self.engine.clone();
        let residents = self.resident_supervisor.clone();
        let native_services = Arc::clone(&self.native_services);
        let shutdown = self.lifecycle.shutdown();
        let io_tasks = self.lifecycle.io_tasks.clone();
        async move {
            engine.begin_drain(cause);
            shutdown.await;
            for service in native_services.iter() {
                if let Err(error) = service.stop().await {
                    tracing::warn!(service = service.id(), %error, "native service stop failed");
                }
            }
            let report = residents.drain(cause, DRAIN_DEADLINE).await;
            engine.runtime_registry().shutdown_sources().await;
            io_tasks.close();
            io_tasks.wait().await;
            report
        }
    }

    /// Stop intake, cancel and join admitted handlers, then drain turns and
    /// residents so no late registration can escape the drain. Safe to call repeatedly.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.drain(FinishCause::Shutdown).await;
        self.finish_shutdown().await
    }

    async fn finish_shutdown(&mut self) -> Result<(), CoreError> {
        self.engine
            .store()
            .drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT)
            .await;
        if let Some(owner) = self.runtime_owner {
            self.engine.store().release_runtime_owner(owner)?;
            self.runtime_owner = None;
        }
        Ok(())
    }
}

/// Start recovered residents, run a body, and always attempt supervisor shutdown.
/// Server embedders admit restore prompts with `resume_interrupted` first.
pub async fn with_built_session_engine<T, E, F, Fut>(
    mut built: BuiltSessionEngine,
    body: F,
) -> Result<T, E>
where
    F: FnOnce(&mut BuiltSessionEngine) -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: From<CoreError>,
{
    let body_result = match built.resident_supervisor.start_recovered().await {
        Ok(_) => body(&mut built).await,
        Err(error) => Err(E::from(error)),
    };
    let shutdown_result = built.shutdown().await.map_err(E::from);
    match (body_result, shutdown_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Spawn the background task that admits multi-agent team/member spawn requests.
///
/// Keep the returned owner alive; call its `shutdown().await` before teardown.
pub fn spawn_team_supervisor(
    rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
) -> RuntimeTaskOwner {
    let owner = RuntimeTaskOwner::new();
    let sidecar_environment =
        Arc::new(BundleSidecarEnvironment::production(owner.io_tasks.clone()));
    let lifecycle = spawn_team_supervisor_with_environment(
        rx,
        engine.clone(),
        base,
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
        RuntimeTaskOwner::new(),
    );
    // The standalone owner must perform the same ordered teardown as a built engine.
    let stop = owner.stop.clone();
    let io_tasks = owner.io_tasks.clone();
    owner.tasks.spawn(async move {
        stop.cancelled().await;
        engine.begin_drain(FinishCause::Shutdown);
        lifecycle.shutdown().await;
        resident_supervisor
            .drain(FinishCause::Shutdown, DRAIN_DEADLINE)
            .await;
        engine.runtime_registry().shutdown_sources().await;
        io_tasks.close();
        io_tasks.wait().await;
    });
    owner
}

fn spawn_team_supervisor_with_environment(
    mut rx: tokio::sync::mpsc::Receiver<BoundSpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
    sidecar_environment: Arc<BundleSidecarEnvironment>,
    lifecycle: RuntimeTaskOwner,
) -> RuntimeTaskOwner {
    let stop_child = lifecycle.stop.clone();
    let tasks = lifecycle.tasks.clone();
    lifecycle.tasks.spawn(async move {
        loop {
            let bound_request = tokio::select! {
                biased;
                _ = stop_child.cancelled() => break,
                bound_request = rx.recv() => match bound_request {
                    Some(request) => request,
                    None => break,
                },
            };
            let (binding, req) = bound_request.into_parts();
            let parent = match engine.read_projection_shared(req.parent).await {
                Ok(parent) => parent,
                Err(error) => {
                    eprintln!("hya: failed to resolve spawn parent ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            };
            let Some(caller) = parent.session.agent.as_ref() else {
                let _ = req.reply.send(Err(SpawnError::Unavailable));
                continue;
            };
            let providers = engine.provider_snapshot();
            let is_servable = |model: &ModelRef| providers.router().resolve(model).is_some();
            // Guidance is request-scoped: one Arc cloned to every resolved member.
            let request_guidance = req.guidance.clone();
            // Scope the ctx so the non-Sync `is_servable` borrow ends before any await.
            let resolved = {
                let resolve_ctx = ResolveSpawnMemberCtx {
                    engine: &engine,
                    binding: &binding,
                    base: &base,
                    caller: caller.as_str(),
                    allowed_agents: &req.agents,
                    categories: &categories,
                    is_servable: &is_servable,
                    guidance: request_guidance.clone(),
                    sidecar_environment: sidecar_environment.as_ref(),
                };
                req.members
                    .iter()
                    .cloned()
                    .map(|member| {
                        resolve_spawn_member_with_catalog(&resolve_ctx, member, providers.catalog())
                    })
                    .collect::<Result<Vec<_>, _>>()
            };
            let resolved = match resolved {
                Ok(resolved) => resolved,
                Err(error) => {
                    let _ = req.reply.send(Err(error));
                    continue;
                }
            };
            // Every batch needs team-root main activation context resolved from
            // the same captured TurnBinding before any durable admission
            // (ADR-0015: all members are residents). Nested callers must not
            // supply their own roster/resource policy for main.
            let main_activation = {
                match resolve_main_activation_context(
                    &engine,
                    &binding,
                    &base,
                    req.parent,
                    request_guidance.clone(),
                )
                .await
                {
                    Ok(context) => context,
                    Err(error) => {
                        let _ = req.reply.send(Err(error));
                        continue;
                    }
                }
            };
            resident_supervisor
                .dispatch_spawn(req, resolved, main_activation, stop_child.clone(), &tasks)
                .await;
        }
    });
    lifecycle
}

/// When true (default), MCP connect runs after the engine is built so HTTP can
/// listen without waiting on child process handshakes. Set `HYA_DEFER_SIDEPLANES=0`
/// to restore the classic await-before-listen path.
fn defer_sideplanes() -> bool {
    match std::env::var("HYA_DEFER_SIDEPLANES") {
        Ok(value) => {
            let text = value.trim();
            !(text.eq_ignore_ascii_case("0")
                || text.eq_ignore_ascii_case("false")
                || text.eq_ignore_ascii_case("off")
                || text.eq_ignore_ascii_case("no"))
        }
        Err(_) => true,
    }
}

#[derive(Clone, Copy)]
struct EngineBuildOptions {
    defer_mcp: bool,
    /// `--pure`: the runtime registry serves builtin skills only and never
    /// reads external skill directories.
    pure: bool,
}

/// Build a fully wired [`SessionEngine`] plus plugin host, MCP, and ask/question channels.
///
/// MCP connect deferral follows process policy (`defer_sideplanes`). Callers must
/// eventually shut down the returned [`BuiltSessionEngine`] (or use
/// [`with_built_session_engine`]).
pub async fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (InvocationPolicy, config::EngineSettings),
) -> anyhow::Result<BuiltSessionEngine> {
    build_session_engine_with_mcp_defer(
        store,
        router,
        agent,
        mcp,
        plugins,
        tool_config,
        EngineBuildOptions {
            defer_mcp: defer_sideplanes(),
            pure: false,
        },
    )
    .await
}

/// [`build_session_engine`] in `--pure` mode: identical wiring except the
/// runtime registry never discovers external skill directories (the builtin
/// embedded catalog is the whole skill surface).
pub async fn build_session_engine_pure(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (InvocationPolicy, config::EngineSettings),
) -> anyhow::Result<BuiltSessionEngine> {
    build_session_engine_with_mcp_defer(
        store,
        router,
        agent,
        mcp,
        plugins,
        tool_config,
        EngineBuildOptions {
            defer_mcp: defer_sideplanes(),
            pure: true,
        },
    )
    .await
}

/// Seed the engine's cross-model failover plane from configured categories.
///
/// Every candidate position seeds its forward suffix chain (`candidate[k..]`),
/// so members resolved onto a servability-picked candidate — not only the
/// configured preference — keep working failover for their whole lifetime.
/// Single-candidate categories contribute nothing (there is no cross-model
/// step). When two categories share a preferred model the first category in
/// canonical key order wins; identical configs collapse to the same chain.
fn category_model_fallbacks(categories: &CategoryRegistry) -> HashMap<ModelRef, Vec<ModelRef>> {
    let mut fallbacks: HashMap<ModelRef, Vec<ModelRef>> = HashMap::new();
    for (_, candidates) in categories.resolution_candidates() {
        for (offset, candidate) in candidates.iter().enumerate() {
            let forward = candidates[offset..].to_vec();
            if forward.len() > 1 {
                fallbacks.entry(candidate.clone()).or_insert(forward);
            }
        }
    }
    fallbacks
}

fn enabled_native_tool_families(
    selection: &hya_bundle::BuiltinBundleSelection,
    include_network: bool,
) -> Vec<&'static str> {
    let candidates: &[&'static str] = if include_network {
        &[
            "hya/hashline",
            "hya/coding-extras",
            "hya/extended-tools",
            "hya/network",
            "hya/channel-tools",
        ]
    } else {
        &[
            "hya/hashline",
            "hya/coding-extras",
            "hya/extended-tools",
            "hya/channel-tools",
        ]
    };
    candidates
        .iter()
        .copied()
        .filter(|id| {
            selection.is_enabled(id)
                && hya_bundle::first_party_bundle_optional(id)
                    .ok()
                    .flatten()
                    .is_some()
        })
        .collect()
}

/// Compose the built-in [`ToolRegistry`] for `selection` (`config.yaml` -> `bundles.builtin`).
///
/// Enabled native bundles load first (`hya/hashline` and `hya/coding-extras`
/// via ABI v2 overriding `hya/bare`'s standard `read`/`write`/`edit`/`grep`,
/// plus any enabled transitional tool families). Any non-kernel-exclusive
/// `hya/bare` standard primitive (`read`, `write`, `edit`, `bash`, `grep`,
/// `skill`) not overridden by an enabled bundle is registered from `hya-core`
/// with origin `"hya/bare"`.
///
/// Kernel-exclusive primitives (`todo`, `rewrite`) are never registered into
/// [`ToolRegistry`] and remain permanently owned by the kernel loop.
///
/// # Errors
/// Returns a diagnostic message if an enabled native bundle cannot be loaded
/// or disagrees with its declared tool policy.
pub fn compose_builtin_tool_registry(
    selection: &hya_bundle::BuiltinBundleSelection,
) -> Result<ToolRegistry, String> {
    compose_builtin_tool_registry_with_runtime(selection, None, true).map(|(registry, _)| registry)
}

fn compose_builtin_tool_registry_with_runtime(
    selection: &hya_bundle::BuiltinBundleSelection,
    host_runtime: Option<Arc<dyn hya_runtime_abi::HostRuntime>>,
    include_network: bool,
) -> Result<(ToolRegistry, hya_tool_loader::NativeRegistrations), String> {
    let enabled_families = enabled_native_tool_families(selection, include_network);
    let (registry, registrations) = match host_runtime {
        Some(runtime) => {
            hya_tool_loader::try_load_tool_families_with_runtime(&enabled_families, runtime)?
        }
        None => (
            hya_tool_loader::try_from_tool_families(&enabled_families)?,
            hya_tool_loader::NativeRegistrations::default(),
        ),
    };
    register_missing_bare_tools(&registry);
    Ok((registry, registrations))
}

async fn build_session_engine_with_mcp_defer(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (InvocationPolicy, config::EngineSettings),
    options: EngineBuildOptions,
) -> anyhow::Result<BuiltSessionEngine> {
    let lifecycle = RuntimeTaskOwner::new();
    let owner_run_id = OwnerRunId::new();
    store
        .claim_runtime_owner(owner_run_id)
        .context("claim runtime owner before startup recovery")?;
    // Crash recovery: close every assistant turn a dead process left open
    // (`MessageFinished { cancelled, cause: interrupted }`, open tool parts
    // errored, member rows cancelled) before any turn of this process runs.
    // Indexed, so it only touches sessions a crash left mid-turn.
    store
        .recover_interrupted_turns(owner_run_id)
        .await
        .context("close turns a stopped process left open")?;
    crate::startup_trace::mark("interrupted_turns_recovered", None);
    let recovered_claims =
        hya_engine::resident::recover_resident_claims(&store, owner_run_id).await?;
    crate::startup_trace::mark("store_recovery", None);
    let (invocation_policy, settings) = tool_config;
    let builtins = settings.builtin_bundles().clone();
    let categories = Arc::new(settings.categories());
    let router = Arc::new(router);
    let app_host_runtime = Arc::new(crate::host_runtime::AppHostRuntime::new(
        store.clone(),
        owner_run_id,
        settings.resolved_config_file(),
        Arc::clone(&router),
        agent.clone(),
        Arc::clone(&categories),
    ));
    let host_runtime: Arc<dyn hya_runtime_abi::HostRuntime> = app_host_runtime.clone();
    let enabled_families = enabled_native_tool_families(&builtins, false);
    let (registry, native_registrations) = compose_builtin_tool_registry_with_runtime(
        &builtins,
        Some(Arc::clone(&host_runtime)),
        false,
    )
    .map_err(|error| anyhow::anyhow!("compose built-in tool registry: {error}"))?;
    let mut started_services: Vec<Arc<dyn hya_runtime_abi::NativeService>> =
        Vec::with_capacity(native_registrations.services.len());
    for service in native_registrations.services {
        if let Err(error) = service.start(Arc::clone(&host_runtime)).await {
            for started in started_services.into_iter().rev() {
                let _ = started.stop().await;
            }
            store.release_runtime_owner(owner_run_id)?;
            return Err(anyhow::anyhow!(
                "start native bundle service `{}`: {error}",
                service.id()
            ));
        }
        started_services.push(service);
    }
    let mut all_native_hooks = native_registrations.hooks;
    let mut loaded_v2_tools = false;
    if builtins.is_enabled("hya/plan")
        && hya_bundle::first_party_bundle_optional("hya/plan")
            .ok()
            .flatten()
            .is_some()
    {
        let regs = hya_tool_loader::load_and_register_v2_bundle(
            &registry,
            "hya/plan",
            Arc::clone(&host_runtime),
        )
        .map_err(anyhow::Error::msg)
        .context("load first-party native bundle hya/plan")?;
        loaded_v2_tools |= !regs.tools.is_empty();
        all_native_hooks.extend(regs.hooks);
    }
    let mut deferred_workflow_regs: Option<hya_tool_loader::NativeRegistrations> = None;
    if builtins.is_enabled("hya/workflow")
        && hya_bundle::first_party_bundle_optional("hya/workflow")
            .ok()
            .flatten()
            .is_some()
    {
        let mut regs = hya_tool_loader::load_and_register_v2_bundle(
            &registry,
            "hya/workflow",
            Arc::clone(&host_runtime),
        )
        .map_err(anyhow::Error::msg)
        .context("load first-party native bundle hya/workflow")?;
        loaded_v2_tools |= !regs.tools.is_empty();
        all_native_hooks.extend(std::mem::take(&mut regs.hooks));
        deferred_workflow_regs = Some(regs);
    }
    let network_enabled = builtins.is_enabled("hya/network")
        && hya_bundle::first_party_bundle_optional("hya/network")
            .ok()
            .flatten()
            .is_some();
    if network_enabled {
        let regs = hya_tool_loader::load_v2("hya/network", Arc::clone(&host_runtime))
            .map_err(|error| anyhow::anyhow!("load native bundle hya/network: {error}"))?;
        for service in &regs.services {
            if let Err(error) = service.start(Arc::clone(&host_runtime)).await {
                for started in started_services.into_iter().rev() {
                    let _ = started.stop().await;
                }
                store.release_runtime_owner(owner_run_id)?;
                return Err(anyhow::anyhow!(
                    "start native service `{}`: {error}",
                    service.id()
                ));
            }
        }
        let advertised: Vec<String> = regs
            .tools
            .iter()
            .map(|tool| tool.name().to_string())
            .collect();
        let mut hidden: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for hook in &regs.hooks {
            if hook
                .hook_points()
                .contains(&hya_runtime_abi::HookPoint::ToolVisibility)
            {
                let outcome = hook
                    .tool_visibility(hya_runtime_abi::ToolVisibilityInput {
                        session: SessionId::new(),
                        agent: None,
                        candidate_tools: advertised.clone(),
                        interactive: true,
                    })
                    .await
                    .map_err(|error| {
                        anyhow::anyhow!("evaluate tool visibility hook `{}`: {error}", hook.id())
                    })?;
                match outcome {
                    hya_runtime_abi::ToolVisibilityOutcome::Preserve => {}
                    hya_runtime_abi::ToolVisibilityOutcome::Hide { tools } => {
                        hidden.extend(tools);
                    }
                    hya_runtime_abi::ToolVisibilityOutcome::AllowOnly { tools } => {
                        for candidate in &advertised {
                            if !tools.contains(candidate) {
                                hidden.insert(candidate.clone());
                            }
                        }
                    }
                }
            }
        }
        let network_preset = hya_tool_loader::tool_bundle_presets()
            .iter()
            .find(|preset| preset.identity() == "hya/network");
        for tool in regs.tools {
            if hidden.contains(tool.name()) {
                continue;
            }
            if let Some(exposure) = network_preset.and_then(|preset| preset.tool(tool.name())) {
                if !exposure.exposed() {
                    continue;
                }
                registry.register_builtin(
                    tool,
                    exposure.permission(),
                    exposure
                        .aliases()
                        .iter()
                        .map(|alias| (alias.name(), alias.visibility())),
                    "hya/network",
                );
            } else {
                registry.register_builtin(tool, ToolPermission::Tool, [], "hya/network");
            }
        }
        started_services.extend(regs.services);
    }
    if builtins.is_enabled("hya/goal")
        && hya_bundle::first_party_bundle_optional("hya/goal")
            .ok()
            .flatten()
            .is_some()
    {
        let regs = hya_tool_loader::load_v2("hya/goal", Arc::clone(&host_runtime))
            .map_err(|error| anyhow::anyhow!("load first-party native bundle hya/goal: {error}"))?;
        for service in &regs.services {
            if let Err(error) = service.start(Arc::clone(&host_runtime)).await {
                for started in started_services.into_iter().rev() {
                    let _ = started.stop().await;
                }
                store.release_runtime_owner(owner_run_id)?;
                return Err(anyhow::anyhow!(
                    "start native service `{}`: {error}",
                    service.id()
                ));
            }
        }
        if let Some(driver) = regs.goal_drivers.into_iter().next() {
            app_host_runtime.attach_goal_driver(driver);
        }
        all_native_hooks.extend(regs.hooks);
        started_services.extend(regs.services);
    }
    if builtins.is_enabled("hya/loop")
        && hya_bundle::first_party_bundle_optional("hya/loop")
            .ok()
            .flatten()
            .is_some()
    {
        let regs = hya_tool_loader::load_v2("hya/loop", Arc::clone(&host_runtime))
            .map_err(|error| anyhow::anyhow!("load first-party native bundle hya/loop: {error}"))?;
        for service in &regs.services {
            if let Err(error) = service.start(Arc::clone(&host_runtime)).await {
                for started in started_services.into_iter().rev() {
                    let _ = started.stop().await;
                }
                store.release_runtime_owner(owner_run_id)?;
                return Err(anyhow::anyhow!(
                    "start native service `{}`: {error}",
                    service.id()
                ));
            }
        }
        started_services.extend(regs.services);
        all_native_hooks.extend(regs.hooks);
        app_host_runtime.set_loop_drivers(regs.loop_drivers);
    }
    register_missing_bare_tools(&registry);

    let project_plugins = if options.pure {
        crate::project_plugins::ProjectPluginSettings::disabled()
    } else {
        let configured = settings.plugin_ids();
        crate::project_plugins::ProjectPluginSettings::new(host_info()).with_configured_ids(
            plugins
                .iter()
                .map(|spec| spec.id.clone())
                .filter(|id| !configured.contains(id)),
        )
    };
    let defer_mcp = options.defer_mcp && !mcp.is_empty();
    let catalog = builtin_agent_catalog_for(&builtins)?;
    // The startup catalog holds only the enabled first-party bundles, so its
    // schema rows come straight from the same prepared document.
    let first_party = crate::installed_bundle_refresh::first_party_catalogs_for(&builtins)?;
    let schema_rows = first_party
        .iter()
        .flat_map(|catalog| catalog.schemas().to_vec())
        .collect::<Vec<_>>();
    let static_sources = crate::installed_bundle_refresh::static_bundle_skill_sources(
        catalog.bundles().as_ref(),
        &schema_rows,
    )?;
    let use_core_skills = builtins.is_enabled("hya/core-skills")
        && hya_bundle::first_party_bundle_optional("hya/core-skills")
            .ok()
            .flatten()
            .is_some();
    let runtime_registry = RuntimeRegistry::new(registry, catalog);
    let runtime_registry = if use_core_skills {
        runtime_registry.with_skill_discovery(
            hya_engine::skill_catalog::discover,
            hya_engine::skill_catalog::builtin_skills,
        )
    } else {
        runtime_registry.with_skill_discovery(hya_engine::skill_catalog::discover_native, Vec::new)
    };
    let runtime = Arc::new(runtime_registry.with_pure_skills(options.pure));
    if !static_sources.is_empty() {
        runtime.refresh(|candidate| {
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources)
        })?;
    }
    let reconciler =
        Arc::new(RuntimeReconciler::new(runtime.clone()).with_io_tasks(lifecycle.io_tasks.clone()));
    let mcp_control = Arc::new(RuntimeMcpControl::new(reconciler.clone()));
    let code_mode_spec = if options.pure {
        None
    } else {
        crate::code_mode::plugin_spec_for_mode(
            settings.code_mode(),
            &crate::config::active_config_path(),
            &builtins,
            &[],
        )
        .context("prepare core code-mode runtime specification")?
    };
    let load_compaction = builtins.is_enabled("hya/compaction");
    let use_tool_family_prompts =
        !enabled_families.is_empty() || loaded_v2_tools || network_enabled;
    let catalog_refresh = Arc::new(
        RuntimeExtensionsRefresh::new(
            crate::ProjectScopeRefresh::new(Arc::new(
                InstalledBundleRefresh::new(bundle_registry_path())
                    .with_builtin_selection(builtins)
                    .with_io_tasks(lifecycle.io_tasks.clone())
                    .with_host_reads(Arc::new(hya_engine::StoreSessionReads::new(store.clone()))),
            ))
            .with_project_plugins(project_plugins),
            reconciler.clone(),
            options.pure,
        )?
        .with_core_plugins(code_mode_spec.iter().cloned().collect()),
    );

    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, asks) = PermissionPlane::new_with_policy(rules, invocation_policy);
    let (interaction, questions) = InteractionPlane::new();
    let subagent_limits = settings.subagents();
    let spawn_queue_capacity = usize::try_from(subagent_limits.per_run_budget)
        .unwrap_or(tokio::sync::Semaphore::MAX_PERMITS)
        .clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
    let (spawn_sender, spawn_rx) = BoundSpawnSender::with_capacity(spawn_queue_capacity);
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let (lifecycle_plane, lifecycle_rx) = LifecyclePlane::new();

    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    let sidecar_environment = Arc::new(BundleSidecarEnvironment::production(
        lifecycle.io_tasks.clone(),
    ));
    let context_settings = settings.context();
    let engine = if load_compaction {
        match SessionEngine::try_new_with_host_runtime(
            store.clone(),
            router,
            runtime,
            permission,
            bus,
            Arc::clone(&host_runtime),
        ) {
            Ok(engine) => engine,
            Err(error) => {
                store.release_runtime_owner(owner_run_id)?;
                return Err(error).context("load internal native rewriters at startup");
            }
        }
    } else {
        SessionEngine::new_bare(store.clone(), router, runtime, permission, bus)
    };
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::for_engine(&engine, agent.model.clone()));
    let engine_builder = engine
        .with_base_system_prompt(agent.system_prompt.clone())
        .with_pure_context(options.pure)
        .with_tool_family_prompts(use_tool_family_prompts)
        .with_native_hook_list(all_native_hooks)
        // Stop/start records use this store's runtime-owner claim.
        .with_runtime_owner(owner_run_id)
        .with_global_reasoning(settings.reasoning())
        .with_catalog_scope_cache(settings.scopes())
        .with_catalog_refresh(catalog_refresh)
        .with_sidecar_environment(sidecar_environment.clone())
        .with_model_categories(categories.clone())
        // Route `categories:` failover chains into the engine's cross-model
        // plane so turn-time pre-stream failures advance through the ordered
        // candidates instead of failing the whole turn.
        .with_model_fallbacks(category_model_fallbacks(&categories))
        .with_terminal_handoff(
            summarizer,
            TerminalHandoffOptions {
                max_output_tokens: context_settings.compaction.summary_max_tokens,
            },
        )
        .with_rewriter_keep_recent(context_settings.compaction.keep_recent)
        .with_token_accounting(TokenAccounting::new(context_settings.token_accounting))
        .with_formatter(settings.formatter())
        .with_lsp(settings.lsp()?)
        .with_exclusive_tool(
            (settings.code_mode() == config::CodeMode::Only)
                .then(|| config::CodeMode::TOOL.to_string()),
        )
        .with_interaction(interaction)
        // Explicit cancellation closes intake: the engine owns each sender while
        // its receiver service retains the engine.
        .with_spawn_sender(spawn_sender)
        .with_mailbox(mailbox)
        .with_lifecycle(lifecycle_plane)
        .with_governor(governor);

    let engine = Arc::new(engine_builder);
    app_host_runtime.attach_engine(&engine);
    let (agent_configuration, agent_efforts) = settings.agents()?;
    let agent_model_control = PersistentAgentModelControl::load(
        engine.store().clone(),
        owner_run_id,
        engine.runtime_registry(),
        engine.clone(),
    )
    .await
    .context("load Agent model preferences before engine readiness")?
    .with_categories(categories.clone())
    .with_configuration_snapshot(
        crate::agent_model_config::AgentModelConfigFiles::new(crate::config::active_config_path()),
        agent_configuration,
    )
    .await;
    engine
        .runtime_registry()
        .publish_agent_effort_configuration(agent_efforts);
    let mut plugin_desired = plugins
        .into_iter()
        .map(|spec| DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec))
        .collect::<Vec<_>>();
    if let Some(spec) = code_mode_spec {
        let id = SourceId::plugin(spec.id.clone());
        if !plugin_desired.iter().any(|existing| existing.id() == &id) {
            plugin_desired.push(DesiredSource::plugin(id, spec));
        }
    }
    let mcp_desired = mcp
        .iter()
        .map(|(name, config)| DesiredSource::mcp(SourceId::mcp(name.clone()), config.clone()))
        .collect::<Vec<_>>();

    if defer_mcp {
        let plugin_plan = reconciler
            .replace_desired(plugin_desired.clone())
            .context("plan startup plugin reconciliation")?;
        if let Err(error) = reconciler
            .reconcile_plan(plugin_plan, lifecycle.stop.clone())
            .await
        {
            eprintln!("hya: plugin reconciliation rejected ({error})");
        }

        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let deferred_plan = reconciler
            .replace_desired(desired)
            .context("plan deferred MCP reconciliation")?;
        let reconciler_bg = reconciler.clone();
        let stop = lifecycle.stop.clone();
        lifecycle.tasks.spawn(async move {
            if let Err(error) = reconciler_bg.reconcile_plan(deferred_plan, stop).await {
                eprintln!("hya: MCP runtime refresh rejected ({error})");
            }
        });
    } else {
        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let plan = reconciler
            .replace_desired(desired)
            .context("plan startup runtime reconciliation")?;
        if let Err(error) = reconciler
            .reconcile_plan(plan, lifecycle.stop.clone())
            .await
        {
            eprintln!("hya: startup runtime reconciliation rejected ({error})");
        }
    }
    crate::startup_trace::mark("engine_runtime", None);
    // Drive resident (long-lived actor) subagents + quiescence (ADR-0002). Started
    // before the team supervisor so its bus subscription is live for the first mail.
    let resident_supervisor = ResidentSupervisor::start_with_recovery(
        engine.clone(),
        owner_run_id,
        agent,
        recovered_claims,
    )
    .await?;
    app_host_runtime.attach_resident_supervisor(&resident_supervisor);
    app_host_runtime.attach_sidecar_environment(Arc::clone(&sidecar_environment));
    if let Some(regs) = deferred_workflow_regs {
        engine.register_native_projection_reducers(regs.projection_reducers);
        engine
            .runtime_registry()
            .register_native_bundle_apis("hya/workflow", regs.api_handlers)
            .map_err(|error| anyhow::anyhow!("register hya/workflow native APIs: {error}"))?;
        for service in &regs.services {
            if let Err(error) = service.start(Arc::clone(&host_runtime)).await {
                for started in started_services.into_iter().rev() {
                    let _ = started.stop().await;
                }
                store.release_runtime_owner(owner_run_id)?;
                return Err(anyhow::anyhow!(
                    "start native service `{}`: {error}",
                    service.id()
                ));
            }
        }
        started_services.extend(regs.services);
    }
    engine.register_native_services(started_services.clone());
    let native_services: Arc<[Arc<dyn hya_runtime_abi::NativeService>]> = started_services.into();
    let lifecycle = spawn_team_supervisor_with_environment(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
        lifecycle,
    );
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    // Release idle Project scopes (and their plugin/bundle processes) even
    // when nothing binds.
    lifecycle.tasks.spawn(run_catalog_scope_sweeper(
        Arc::downgrade(&engine),
        lifecycle.stop.clone(),
        CATALOG_SCOPE_SWEEP_PERIOD,
    ));
    lifecycle.tasks.spawn(run_mailbox_service(
        engine.clone(),
        mailbox_rx,
        lifecycle.stop.clone(),
    ));
    lifecycle.tasks.spawn(run_lifecycle_service(
        engine.clone(),
        resident_supervisor.clone(),
        lifecycle_rx,
        lifecycle.stop.clone(),
    ));
    Ok(BuiltSessionEngine {
        engine,
        resident_supervisor,
        asks: Some(asks),
        questions: Some(questions),
        mcp_control,
        host_runtime,
        native_services,
        lifecycle,
        agent_model_control,
        runtime_owner: Some(owner_run_id),
    })
}

/// How often idle catalog scopes are swept.
const CATALOG_SCOPE_SWEEP_PERIOD: std::time::Duration = std::time::Duration::from_secs(60);

/// Periodically evict idle and over-capacity catalog scopes until `stop`
/// fires or the engine is gone.
async fn run_catalog_scope_sweeper(
    engine: std::sync::Weak<SessionEngine>,
    stop: tokio_util::sync::CancellationToken,
    period: std::time::Duration,
) {
    let mut ticks = tokio::time::interval(period);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick completes immediately; start sweeping one period in.
    ticks.tick().await;
    loop {
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            _ = ticks.tick() => {}
        }
        let Some(engine) = engine.upgrade() else {
            break;
        };
        engine.sweep_catalog_scopes();
    }
}

/// Engine staged for a server host. A host can prepare its discovery metadata
/// before consuming this value to attach the server controls and interaction bridges.
#[must_use]
pub struct ServerEngine {
    built: BuiltSessionEngine,
    agent: Arc<AgentSpec>,
    default_agent: Option<String>,
    pure: bool,
    builtin_selection: hya_bundle::BuiltinBundleSelection,
    use_core_agents: bool,
    use_core_commands: bool,
    pending_discovery: Vec<config::PendingCatalogDiscovery>,
}

impl ServerEngine {
    /// Build the server engine from its resolved provider and tool policy.
    pub async fn build(store: SessionStore, runtime: RuntimeConfig) -> anyhow::Result<Self> {
        // The server retains the base. Per-turn guidance is resolved by the harness.
        let builtin_selection = runtime.settings.builtin_bundles().clone();
        let use_core_agents = core_agents_available(&builtin_selection);
        let use_core_commands = builtin_selection.is_enabled("hya/core-commands")
            && hya_bundle::first_party_bundle_optional("hya/core-commands")
                .ok()
                .flatten()
                .is_some();
        let agent = Arc::new(agent_base_for_runtime(&runtime, None));
        let tool_config = (runtime.permission, runtime.settings);
        let built = build_session_engine_with_mcp_defer(
            store,
            runtime.router,
            &agent,
            runtime.mcp,
            runtime.plugins,
            tool_config,
            EngineBuildOptions {
                defer_mcp: defer_sideplanes(),
                pure: runtime.pure,
            },
        )
        .await?;
        Ok(Self {
            built,
            agent,
            default_agent: runtime.default_agent,
            pure: runtime.pure,
            builtin_selection,
            use_core_agents,
            use_core_commands,
            pending_discovery: runtime.pending_discovery,
        })
    }

    /// Claim the interaction receivers and attach the application control services.
    pub fn assemble(mut self, hosts: hya_server::HostPolicy) -> anyhow::Result<ServerRuntime> {
        let asks = self.built.take_asks().context("asks receiver missing")?;
        let questions = self
            .built
            .take_questions()
            .context("questions receiver missing")?;
        let engine = self.built.engine();
        let provider_manager = crate::ProviderManager::new(Arc::clone(&engine));
        let state = hya_server::AppState::new(engine, self.agent)
            .with_provider_control(Arc::new(provider_manager.clone()))
            .with_bundle_control(Arc::new(
                crate::bundle_admin::BundleManager::with_selection(self.builtin_selection),
            ))
            .with_question_requests(questions)
            .with_mcp_control(self.built.mcp_control())
            .with_agent_model_control(Arc::new(self.built.agent_model_control()))
            .with_default_agent(self.default_agent)
            .with_pure_guidance(self.pure)
            .with_auto_title(self.use_core_agents)
            .with_core_commands(self.use_core_commands)
            .with_allowed_hosts(hosts)
            .with_permission_requests(asks);
        Ok(ServerRuntime {
            built: self.built,
            state,
            provider_manager,
            pending_discovery: self.pending_discovery,
        })
    }
}

/// Full application runtime shared by the daemon and in-process servers.
#[must_use]
pub struct ServerRuntime {
    built: BuiltSessionEngine,
    state: hya_server::AppState,
    provider_manager: crate::ProviderManager,
    pending_discovery: Vec<config::PendingCatalogDiscovery>,
}

impl ServerRuntime {
    /// State shared by the server's HTTP and gRPC bindings.
    pub fn app_state(&self) -> &hya_server::AppState {
        &self.state
    }

    /// Attach the host's relay before building a server from this state.
    pub fn with_relay_host(mut self, relay: hya_server::RelayHost) -> Self {
        self.state = self.state.with_relay_host(relay);
        self
    }

    /// Start deferred provider discovery once, under the live control's mutation lock.
    pub fn spawn_provider_catalog_refresh(
        &mut self,
        report_error: impl FnOnce(anyhow::Error) + Send + 'static,
    ) {
        let pending = std::mem::take(&mut self.pending_discovery);
        if pending.is_empty() {
            return;
        }
        let manager = self.provider_manager.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            match manager.refresh_pending(pending).await {
                Ok(false) => {}
                Ok(true) => state.notify_catalog_updated(),
                Err(error) => report_error(error),
            }
        });
    }

    /// Admit server restores before starting recovered resident episodes.
    pub async fn resume_interrupted(&self) -> Result<Vec<SessionId>, CoreError> {
        self.built.resume_interrupted(&self.state).await
    }

    /// End streams and fence admission, then drain composition, residents, and
    /// server services. Call `on_fenced` immediately after closing server admission
    /// so the host can signal any secondary listeners to stop accepting.
    pub async fn drain(&self, reason: hya_server::ShutdownReason, on_fenced: impl FnOnce()) {
        self.state.streams().close(reason);
        self.state.begin_shutdown();
        on_fenced();
        self.built.drain(FinishCause::Shutdown).await;
        self.state.shutdown().await;
    }

    /// Report the store exit drain and release database ownership. Hosts call
    /// this after `drain`, once their listeners, relay, and discovery are gone.
    pub async fn finish_shutdown(&mut self) -> Result<(), CoreError> {
        self.built.finish_shutdown().await
    }
}

/// Inputs for [`HyaRuntime::start`] (store path, model, and safety flags).
pub struct RuntimeOptions {
    /// Override default model; `None` uses config / offline default.
    pub model: Option<String>,
    /// SQLite path, or empty string for an in-memory store.
    pub db: String,
    /// When true, auto-approve every tool action (Danger permission model).
    pub yolo: bool,
    /// Override the preferred primary agent id for new sessions.
    pub default_agent: Option<String>,
    /// When true, skip live config and always use the offline dev provider.
    pub force_offline: bool,
}

/// Holds the built engine for the process lifetime so side
/// planes stay connected until the runtime is dropped.
pub struct HyaRuntime {
    router: axum::Router,
    runtime: ServerRuntime,
}

impl HyaRuntime {
    /// Open the store, resolve providers, build the engine, and install the HTTP router.
    ///
    /// When `force_offline` is set, uses only [`offline_router`]. Otherwise loads
    /// config via [`resolve_runtime`]. `yolo` forces Danger permissions and logs
    /// a stderr warning. Returns a runtime whose [`HyaRuntime::router`] can be
    /// served with axum (or inspected by embedders).
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self> {
        let store = open_store(&opts.db).await?;
        let exit_store = store.clone();
        match Self::start_with_store(store, opts).await {
            Ok(runtime) => Ok(runtime),
            Err(error) => {
                exit_store
                    .drain_and_report_exit(hya_store::EXIT_DRAIN_TIMEOUT)
                    .await;
                Err(error)
            }
        }
    }

    async fn start_with_store(store: SessionStore, opts: RuntimeOptions) -> anyhow::Result<Self> {
        let runtime = if opts.force_offline {
            let mut runtime = offline_runtime(opts.model, false);
            runtime.default_agent = opts.default_agent;
            runtime
        } else {
            let mut runtime = resolve_runtime(opts.model).await;
            if opts.default_agent.is_some() {
                runtime.default_agent = opts.default_agent;
            }
            runtime
        }
        .with_yolo(opts.yolo);
        if opts.yolo {
            eprintln!("hya: --yolo auto-approves ALL tool actions for the hya frontend (RCE risk)");
        }
        let runtime = ServerEngine::build(store, runtime)
            .await?
            .assemble(hya_server::HostPolicy::loopback())?;
        if let Err(error) = runtime.app_state().restore_saved_permissions().await {
            tracing::warn!(%error, "failed to restore saved permission grants");
        }
        runtime.resume_interrupted().await?;
        let router = hya_server::router(runtime.app_state().clone());
        Ok(Self { router, runtime })
    }

    /// Shut down server admission, runtime services, and the database owner.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.runtime
            .drain(hya_server::ShutdownReason::Stop, || {})
            .await;
        self.runtime.finish_shutdown().await
    }

    /// Axum router for the v1 HTTP surface (from [`hya_server::router`]).
    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    /// Shared session engine for in-process callers (same instance the router uses).
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.runtime.built.engine()
    }

    /// Clone of the HTTP `AppState` wrapped by the router.
    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.runtime.app_state().clone()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod open_store_tests {
    use super::{HyaRuntime, RuntimeOptions, open_store, open_store_or_memory};

    #[tokio::test]
    async fn open_store_is_strict_while_open_store_or_memory_falls_back() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "hya-open-store-split-{}-{}",
            std::process::id(),
            nanos
        ));
        let blocker = dir.join("not-a-dir");
        std::fs::create_dir_all(&blocker).unwrap();
        let path = blocker.to_string_lossy().into_owned();

        let strict_err = match open_store(&path).await {
            Ok(_) => panic!("strict open_store must fail on directory db"),
            Err(err) => err,
        };
        assert!(
            strict_err.to_string().contains("open store"),
            "{strict_err}"
        );

        let runtime_err = match HyaRuntime::start(RuntimeOptions {
            db: path.clone(),
            model: None,
            default_agent: None,
            yolo: false,
            force_offline: true,
        })
        .await
        {
            Ok(_) => panic!("HyaRuntime::start must fail on directory db"),
            Err(err) => err,
        };
        assert!(
            runtime_err.to_string().contains("open store"),
            "{runtime_err}"
        );

        let fallback = match open_store_or_memory(&path).await {
            Ok(store) => store,
            Err(err) => panic!("open_store_or_memory must fall back on directory db: {err}"),
        };
        assert!(fallback.memory_fallback().is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
