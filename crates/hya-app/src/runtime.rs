//! Backend runtime bootstrap: config → store → session engine → optional HTTP router.
//!
//! Call [`resolve_runtime`] (or build a [`RuntimeConfig`] offline), open a store with
//! [`open_store`], assemble a [`BuiltSessionEngine`] via [`build_session_engine`], or
//! use [`HyaRuntime::start`] for the full server-ready process path.

// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use hya_bundle::BundleCatalog;
use hya_core::agent_catalog::AgentCatalog;
use hya_core::resident::{
    ResolveSpawnMemberCtx, resolve_main_activation_context, resolve_spawn_member,
};
use hya_core::{
    AgentSpec, BoundSidecarFactory, BoundSpawnRequest, BoundSpawnSender, BoundWorkflowRequest,
    BoundWorkflowSender, CategoryRegistry, CoreError, DRAIN_DEADLINE, EventBus, ModelSummarizer,
    PromptEnv, ResidentSupervisor, RuntimeRegistry, RuntimeSourceKind, SessionEngine,
    SidecarEnvironment, SidecarHandle, SidecarLifecycle, SidecarStart, SubagentGovernor,
    Summarizer, TokenAccounting, TurnBinding, TurnDrainReport, build_system_prompt,
    run_lifecycle_service, run_mailbox_service,
};

// Single discovery/date implementation lives in hya-core; re-export for callers.
pub use hya_core::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::activation::{
    bind_bundle_sidecar_tools, materialize_bundle_sidecar_resources, validate_bundle_sidecar_hooks,
};
use hya_plugin::config::{PluginSpec, ProcessLaunch};
use hya_plugin::messages::{ActivationLifecycle, ActivationMetadata, PluginKindWire};
use hya_plugin::{HostInfo, PluginContributionSet};
use hya_proto::{AgentName, FinishCause, ModelRef, OwnerRunId, SessionId, WorkflowDelivery};
use hya_provider::{DevProvider, ProviderCatalogSnapshot, ProviderRouter, ReasoningEffort};
use hya_store::SessionStore;
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, LifecyclePlane, MailboxPlane, Mode,
    PermissionModel, PermissionPlane, PermissionRules, QuestionRequest, Rule, SpawnError,
    ToolRegistry, WebSearchConfig, WebSearchPlane,
};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

use crate::agent_model_control::PersistentAgentModelControl;
use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, RuntimeExtensionsRefresh, RuntimeMcpControl, RuntimeReconciler, SourceId,
    adapt_prepared_bundle_skills,
};
use crate::{InstalledBundleRefresh, bundle_registry_path, plugins};

/// Agent catalog for a process before installed bundles are refreshed.
///
/// Rust-native built-in agents and the build-prepared first-party WorkflowBundle
/// are published together. Installed rows are merged later by the root refresh.
/// The first-party payload is decoded and verified before the runtime starts.
pub fn builtin_agent_catalog() -> anyhow::Result<Arc<AgentCatalog>> {
    let first_party = crate::installed_bundle_refresh::first_party_catalogs()
        .context("decode embedded first-party bundles")?;
    let first_party_refs = first_party.iter().collect::<Vec<_>>();
    let bundles = BundleCatalog::from_verified_catalogs(&first_party_refs)
        .context("build first-party bundle catalog")?;
    let catalog =
        AgentCatalog::new(Arc::new(bundles)).context("build agent catalog over built-ins")?;
    Ok(Arc::new(catalog))
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

pub(crate) fn process_owner_run_id() -> OwnerRunId {
    static OWNER_RUN_ID: OnceLock<OwnerRunId> = OnceLock::new();
    *OWNER_RUN_ID.get_or_init(OwnerRunId::new)
}

/// Canonical Harness agent base string (no Environment / AGENTS).
pub const HARNESS_AGENT_BASE: &str = "You are hya, a coding agent.";

struct BundleSidecarEnvironment {
    io_tasks: Option<tokio_util::task::TaskTracker>,
    command: Option<Vec<String>>,
    staging_root: PathBuf,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

impl BundleSidecarEnvironment {
    #[cfg(test)]
    fn from_command(command: Vec<String>, staging_root: PathBuf) -> Self {
        Self {
            io_tasks: hya_mcp::io_tasks::current(),
            command: Some(command),
            staging_root,
            terminate_notify: None,
        }
    }

    fn production() -> Self {
        let registry_path = bundle_registry_path();
        let staging_root = registry_path.parent().map_or_else(
            || PathBuf::from("activations"),
            |parent| parent.join("activations"),
        );
        Self {
            io_tasks: hya_mcp::io_tasks::current(),
            command: plugins::bundle_sidecar_command(),
            staging_root,
            #[cfg(test)]
            terminate_notify: None,
        }
    }
}

impl SidecarEnvironment for BundleSidecarEnvironment {
    fn factory_for(
        &self,
        binding: &TurnBinding,
        stable_agent_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError> {
        // The agent must exist, but it need not own a bundle: a built-in owns no
        // bundle resources and therefore never has a sidecar.
        let definition = binding.resolve_agent(stable_agent_id).ok_or_else(|| {
            CoreError::AgentDefinitionMissing {
                agent_id: stable_agent_id.to_string(),
            }
        })?;
        if definition.origin.is_builtin() {
            return Ok(None);
        }
        let has_selected_sidecar_capability =
            binding.has_selected_bundle_sidecar_capability(stable_agent_id)?;
        if !has_selected_sidecar_capability {
            return Ok(None);
        }
        let Some(command) = &self.command else {
            return Err(CoreError::Invalid(
                "Bun is required for executable Bundle sidecars".to_string(),
            ));
        };
        Ok(Some(Arc::new(BundleSidecarFactory {
            io_tasks: self.io_tasks.clone(),
            binding: binding.clone(),
            stable_agent_id: stable_agent_id.to_string(),
            command: command.clone(),
            staging_root: self.staging_root.clone(),
            #[cfg(test)]
            terminate_notify: self.terminate_notify.clone(),
        })))
    }
}

struct BundleSidecarFactory {
    io_tasks: Option<tokio_util::task::TaskTracker>,
    binding: TurnBinding,
    stable_agent_id: String,
    command: Vec<String>,
    staging_root: PathBuf,
    #[cfg(test)]
    terminate_notify: Option<Arc<tokio::sync::Notify>>,
}

#[async_trait::async_trait]
impl BoundSidecarFactory for BundleSidecarFactory {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError> {
        let handle = hya_plugin::activation::start_activation_sidecar(
            start, &self.command, &self.staging_root, host_info(), self.io_tasks.clone(),
            |start, activation_dir| {
                let extension_paths = materialize_bundle_sidecar_resources(
                    &self.binding,
                    &self.stable_agent_id,
                    activation_dir,
                )?;

                let mut command = self.command.clone();
                if !extension_paths.is_empty() {
                    command.push("--".to_string());
                    for path in extension_paths {
                        if !path.is_absolute() {
                            return Err(CoreError::Invalid(format!(
                                "bundle extension path must be absolute: `{}`",
                                path.display()
                            )));
                        }
                        let Some(path) = path.to_str() else {
                            return Err(CoreError::Invalid(
                                "bundle extension path is not valid UTF-8".to_string(),
                            ));
                        };
                        command.push("--bundle-extension".to_string());
                        command.push(path.to_string());
                    }
                }

                let definition = self
                    .binding
                    .resolve_agent(&self.stable_agent_id)
                    .ok_or_else(|| CoreError::AgentDefinitionMissing {
                        agent_id: self.stable_agent_id.clone(),
                    })?;
                let files = crate::agent_model_config::AgentModelConfigFiles::new(
                    crate::config::active_config_path(),
                );
                let config_file = files
                    .path_for_binding(&self.binding, definition.origin)
                    .and_then(|path| std::path::absolute(path).map_err(Into::into))
                    .map_err(|error| {
                        CoreError::Invalid(format!("resolve bundle configuration path: {error}"))
                    })?;
                let config_dir = config_file.parent().ok_or_else(|| {
                    CoreError::Invalid("bundle configuration path has no parent".to_string())
                })?;
                let environment = BTreeMap::from([
                    (
                        crate::bundle_config::HYA_BUNDLE_CONFIG_DIR.to_string(),
                        config_dir.to_string_lossy().into_owned(),
                    ),
                    (
                        crate::bundle_config::HYA_BUNDLE_CONFIG_FILE.to_string(),
                        config_file.to_string_lossy().into_owned(),
                    ),
                ]);
                let lifecycle = match start.lifecycle {
                    SidecarLifecycle::Transient => ActivationLifecycle::Transient,
                    SidecarLifecycle::Resident => ActivationLifecycle::Resident,
                };
                Ok(PluginSpec {
                    id: self.stable_agent_id.clone(),
                    kind: PluginKindWire::Bun,
                    command,
                    timeout_ms: None,
                    env: environment,
                    plugin_dir: None,
                    launch: ProcessLaunch::Bundle {
                        root: activation_dir.to_path_buf(),
                        activation: Some(ActivationMetadata {
                            activation_id: start.activation_id.clone(),
                            lifecycle,
                        }),
                    },
                })
            },
            |plugin, host| {
                let contributions = plugin.contributions();
                validate_bundle_sidecar_hooks(
                    &self.binding,
                    &self.stable_agent_id,
                    &contributions.hooks,
                )?;
                validate_bundle_sidecar_skills(&self.binding, &self.stable_agent_id, contributions)?;
                let tools = bind_bundle_sidecar_tools(&self.binding, &self.stable_agent_id, plugin)?;
                if !contributions.apis.is_empty() {
                    return Err(CoreError::Invalid(
                        "bundle sidecar has undeclared API exports".into(),
                    ));
                }
                if !contributions.workspace_adapters.is_empty() {
                    tracing::warn!(agent = %self.stable_agent_id, "ignoring unsupported bundle sidecar workspace adapters");
                }
                let hooks = self
                    .binding
                    .sidecar_hook_view(&self.stable_agent_id, host.clone())?;
                Ok(hya_plugin::activation::ActivationBindings { tools, hooks })
            },
        ).await?;
        #[cfg(test)]
        let handle: Box<dyn SidecarHandle> = match &self.terminate_notify {
            Some(notify) => Box::new(tests::NotifySidecarHandle {
                inner: handle,
                notify: notify.clone(),
            }),
            None => handle,
        };
        Ok(handle)
    }
}

/// Require a bundle sidecar's Skill declaration set to match signed prepared bytes.
fn validate_bundle_sidecar_skills(
    binding: &TurnBinding,
    stable_agent_id: &str,
    contributions: &PluginContributionSet,
) -> Result<(), CoreError> {
    let policy = binding.agent_resource_policy(stable_agent_id)?;
    let bundle_id = policy.bundle_id().ok_or_else(|| {
        CoreError::Invalid(format!(
            "bundle sidecar agent `{stable_agent_id}` has no owning bundle"
        ))
    })?;
    let resources = binding
        .bundle_catalog()
        .bundle_resources(bundle_id, hya_bundle::ExportKind::Skill)
        .ok_or_else(|| {
            CoreError::Invalid(format!(
                "bundle `{bundle_id}` has no prepared Skill namespace"
            ))
        })?;
    adapt_prepared_bundle_skills(bundle_id, resources, contributions)
        .map(|_| ())
        .map_err(|error| CoreError::Invalid(format!("bundle sidecar Skill declaration: {error}")))
}

/// Built-in root-session agent id used when neither an explicit override nor
/// config `default_agent` selects one. Matches `serve`'s own fallback
/// (`ServerState.default_agent`, then this same built-in agent).
pub const BUILTIN_DEFAULT_AGENT_ID: &str = hya_core::MAIN_AGENT_ID;

/// Agent base only — for HTTP/SSE server and interactive TUI AppState assembly.
///
/// Bundle `prompt=None` keeps this base; per-turn server discovery appends
/// Environment + current workdir AGENTS + references. Baking those at startup
/// would duplicate AGENTS when guidance is also layered.
///
/// The workdir is empty: `hya serve` has no working directory (ADR-0024).
/// Every turn's AgentSpec takes its workdir from its session.
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

/// Direct-mode agent (exec/RPC/goal): base + Environment + caller-cwd AGENTS.
///
/// These paths call `run_turn` without a separate guidance layer, so context
/// must remain composed into `system_prompt` here.
pub fn agent_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = caller_workdir();
    let env = PromptEnv {
        cwd: workdir.to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(&workdir);
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &context);
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
}

/// `--pure` variant of [`agent_with_model`]: same Environment block, but no
/// external AGENTS/context files are discovered or baked into the prompt.
pub fn agent_with_model_pure(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = caller_workdir();
    let env = PromptEnv {
        cwd: workdir.to_string_lossy().into_owned(),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &[]);
    AgentSpec {
        name: AgentName::new(BUILTIN_DEFAULT_AGENT_ID),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
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
    let candidate = explicit
        .or(default_agent)
        .unwrap_or(BUILTIN_DEFAULT_AGENT_ID);
    let binding = engine.bind_root_runtime(workdir).await?;
    binding
        .resolve_agent(candidate)
        .map(|definition| AgentName::new(definition.stable_id))
        .ok_or_else(|| {
            CoreError::from(hya_bundle::BundleError::UnknownAgentId {
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
    /// Web-search plane configuration.
    pub websearch: WebSearchConfig,
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
        websearch: WebSearchConfig::default(),
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
        websearch: cfg.websearch,
        pending_discovery: cfg.pending_discovery,
        settings: cfg.settings,
        pure: false,
    })
}

/// Reserve ownership before opening a SQLite runtime store, or use memory when `db` is empty.
pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    if db.is_empty() {
        SessionStore::connect_memory()
            .await
            .context("open in-memory store")
    } else {
        SessionStore::connect_runtime(db)
            .await
            .with_context(|| format!("open store at {db}"))
    }
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
    workflow_control: crate::WorkflowControl,
    agent_model_control: PersistentAgentModelControl,
    asks: Option<tokio::sync::mpsc::UnboundedReceiver<AskRequest>>,
    questions: Option<tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>>,
    mcp_control: Arc<dyn hya_server::McpControl>,
    lifecycle: RuntimeTaskOwner,
}

impl BuiltSessionEngine {
    /// Shared session engine handle.
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        Arc::clone(&self.engine)
    }

    /// Shared resident scheduling owner used by Workflow actor activations.
    #[must_use]
    pub fn resident_supervisor(&self) -> Arc<ResidentSupervisor> {
        Arc::clone(&self.resident_supervisor)
    }

    /// Shared durable Workflow control adapter for direct and server surfaces.
    #[must_use]
    pub fn workflow_control(&self) -> crate::WorkflowControl {
        self.workflow_control.clone()
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
        let shutdown = self.lifecycle.shutdown();
        let io_tasks = self.lifecycle.io_tasks.clone();
        async move {
            engine.begin_drain(cause);
            shutdown.await;
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
        Ok(())
    }
}

/// Adapt the app-owned Workflow control to the dependency-inverted server port.
impl hya_server::WorkflowControl for crate::WorkflowControl {
    fn execute(
        &self,
        session: SessionId,
        command: hya_proto::WorkflowCommand,
        delivery: WorkflowDelivery,
    ) -> hya_server::WorkflowControlFuture<'_> {
        Box::pin(async move {
            crate::WorkflowControl::execute(
                self,
                session,
                crate::WorkflowInvocation {
                    delivery,
                    ..crate::WorkflowInvocation::default()
                },
                command,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .map_err(|error| hya_server::WorkflowControlError::new(error.code(), error.to_string()))
        })
    }

    fn decorate(
        &self,
        session: SessionId,
        state: hya_proto::WorkflowProjection,
    ) -> hya_server::WorkflowDecorationFuture<'_> {
        Box::pin(async move {
            crate::WorkflowControl::decorate(self, session, state)
                .await
                .map_err(|error| {
                    hya_server::WorkflowControlError::new(error.code(), error.to_string())
                })
        })
    }

    fn list(
        &self,
        scope: Option<std::path::PathBuf>,
    ) -> futures::future::BoxFuture<
        '_,
        Result<Vec<hya_proto::WorkflowSummary>, hya_server::WorkflowControlError>,
    > {
        Box::pin(async move {
            crate::WorkflowControl::list(self, scope.as_deref())
                .await
                .map_err(|error| {
                    hya_server::WorkflowControlError::new(error.code(), error.to_string())
                })
        })
    }

    fn active_run(&self, session: SessionId) -> Option<hya_proto::WorkflowRunId> {
        crate::WorkflowControl::active_run(self, session)
            .ok()
            .flatten()
    }

    fn wait_idle(
        &self,
        session: SessionId,
    ) -> futures::future::BoxFuture<'_, Result<(), hya_server::WorkflowControlError>> {
        Box::pin(async move {
            self.wait_run_idle(session).await.map_err(|error| {
                hya_server::WorkflowControlError::new(error.code(), error.to_string())
            })
        })
    }

    fn cancel(&self, session: SessionId) -> bool {
        self.cancel_run(session)
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
    let sidecar_environment = Arc::new(BundleSidecarEnvironment {
        io_tasks: Some(owner.io_tasks.clone()),
        ..BundleSidecarEnvironment::production()
    });
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

/// Receive Workflow requests without waiting for another Session's run.
fn spawn_workflow_supervisor(
    mut rx: tokio::sync::mpsc::Receiver<BoundWorkflowRequest>,
    control: crate::WorkflowControl,
    stop: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let tasks = control.tasks.clone();
    let slots = Arc::new(tokio::sync::Semaphore::new(rx.max_capacity()));
    tasks.spawn(async move {
        loop {
            let bound_request = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                request = rx.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            let (binding, request) = bound_request.into_parts();
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                let _ = request.reply.send(Err(hya_tool::WorkflowHostError::new(
                    "WORKFLOW_OVERLOADED",
                    "workflow host queue is full; retry shortly",
                )));
                continue;
            };
            let control = control.clone();
            control.tasks.clone().spawn(async move {
                handle_workflow_request(&control, binding, request, slot).await;
            });
        }
    })
}

/// Execute one framed workflow request through the app-owned control seam.
async fn handle_workflow_request(
    control: &crate::WorkflowControl,
    binding: TurnBinding,
    request: hya_tool::WorkflowRequest,
    slot: tokio::sync::OwnedSemaphorePermit,
) {
    let hya_tool::WorkflowRequest {
        parent,
        operation,
        command,
        cancel,
        reply,
    } = request;
    let result = control
        .execute_with_admission(
            parent,
            crate::WorkflowInvocation {
                caller: None,
                operation: Some(operation),
                binding: Some(binding),
                delivery: WorkflowDelivery::Finished,
            },
            command,
            cancel,
            Some(slot),
        )
        .await
        .map_err(|error| hya_tool::WorkflowHostError::new(error.code(), error.to_string()));
    let _ = reply.send(result);
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
            let router = engine.provider_router();
            let is_servable = |model: &ModelRef| router.resolve(model).is_some();
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
                    .map(|member| resolve_spawn_member(&resolve_ctx, member))
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
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
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
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
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

async fn build_session_engine_with_mcp_defer(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy, config::EngineSettings),
    options: EngineBuildOptions,
) -> anyhow::Result<BuiltSessionEngine> {
    let lifecycle = RuntimeTaskOwner::new();
    let owner_run_id = process_owner_run_id();
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
    store
        .recover_nonterminal_workflows(owner_run_id, "backend startup recovery")
        .await
        .context("recover nonterminal Workflows before spawn readiness")?;
    let recovered_claims = hya_core::resident::recover_resident_claims(&store, owner_run_id)
        .await
        .context("fence resident actors before startup recovery")?;
    crate::startup_trace::mark("store_recovery", None);
    let (websearch, invocation_policy, settings) = tool_config;
    let router = Arc::new(router);
    let registry = ToolRegistry::builtins();
    if !websearch.enabled {
        registry.remove("websearch");
    }

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
    let catalog = builtin_agent_catalog()?;
    // The startup catalog holds only the first-party bundle, so its schema
    // rows come straight from the same prepared document.
    let first_party = crate::installed_bundle_refresh::first_party_catalogs()?;
    let schema_rows = first_party
        .iter()
        .flat_map(|catalog| catalog.schemas().to_vec())
        .collect::<Vec<_>>();
    let static_sources = crate::installed_bundle_refresh::static_bundle_skill_sources(
        catalog.bundles().as_ref(),
        &schema_rows,
    )?;
    let runtime = Arc::new(RuntimeRegistry::new(registry, catalog).with_pure_skills(options.pure));
    if !static_sources.is_empty() {
        runtime.refresh(|candidate| {
            candidate.replace_sources_of_kind(RuntimeSourceKind::Bundle, static_sources)
        })?;
    }
    let categories = Arc::new(settings.categories());
    let reconciler =
        Arc::new(RuntimeReconciler::new(runtime.clone()).with_io_tasks(lifecycle.io_tasks.clone()));
    let mcp_control = Arc::new(RuntimeMcpControl::new(reconciler.clone()));
    let catalog_refresh = Arc::new(RuntimeExtensionsRefresh::new(
        crate::ProjectScopeRefresh::new(Arc::new(
            InstalledBundleRefresh::new(bundle_registry_path())
                .with_io_tasks(lifecycle.io_tasks.clone())
                .with_host_reads(Arc::new(hya_core::StoreSessionReads::new(store.clone()))),
        ))
        .with_project_plugins(project_plugins),
        reconciler.clone(),
        options.pure,
    )?);

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
    let (workflow_sender, workflow_rx) = BoundWorkflowSender::with_capacity(spawn_queue_capacity);
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let (lifecycle_plane, lifecycle_rx) = LifecyclePlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), agent.model.clone()));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    let sidecar_environment = Arc::new(BundleSidecarEnvironment {
        io_tasks: Some(lifecycle.io_tasks.clone()),
        ..BundleSidecarEnvironment::production()
    });
    let context_settings = settings.context();
    let engine_builder = SessionEngine::new(store, router, runtime, permission, bus)
        .with_base_system_prompt(agent.system_prompt.clone())
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
        .with_compaction(summarizer, context_settings.compaction)
        .with_token_accounting(TokenAccounting::new(context_settings.token_accounting))
        .with_formatter(settings.formatter())
        .with_lsp(settings.lsp()?)
        .with_websearch(WebSearchPlane::configured(websearch))
        .with_interaction(interaction)
        // Explicit cancellation closes intake: the engine owns each sender while
        // its receiver service retains the engine.
        .with_spawn_sender(spawn_sender)
        // INVARIANT (workflow-intake liveness): mirrors `with_spawn_sender` --
        // the engine owns this sender and the workflow worker owns an
        // `Arc<SessionEngine>`, so queued requests always find a live executor.
        .with_workflow_sender(workflow_sender)
        .with_mailbox(mailbox)
        .with_lifecycle(lifecycle_plane)
        .with_governor(governor);

    let engine = Arc::new(engine_builder);
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
    let plugin_desired = plugins
        .into_iter()
        .map(|spec| DesiredSource::plugin(SourceId::plugin(spec.id.clone()), spec))
        .collect::<Vec<_>>();
    let mcp_desired = mcp
        .iter()
        .map(|(name, config)| DesiredSource::mcp(SourceId::mcp(name.clone()), config.clone()))
        .collect::<Vec<_>>();

    if defer_mcp {
        let plugin_plan = reconciler
            .replace_desired(plugin_desired.clone())
            .context("plan startup plugin reconciliation")?;
        if let Err(error) = mcp_control
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
        let control_bg = mcp_control.clone();
        let stop = lifecycle.stop.clone();
        lifecycle.tasks.spawn(async move {
            if let Err(error) = control_bg.reconcile_plan(deferred_plan, stop).await {
                eprintln!("hya: MCP runtime refresh rejected ({error})");
            }
        });
    } else {
        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let plan = reconciler
            .replace_desired(desired)
            .context("plan startup runtime reconciliation")?;
        if let Err(error) = mcp_control
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
    let workflow_control = crate::WorkflowControl::new_with_routing(
        engine.clone(),
        agent.clone(),
        resident_supervisor.clone(),
        owner_run_id,
        categories.clone(),
        &lifecycle,
    );
    let lifecycle = spawn_team_supervisor_with_environment(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        categories,
        resident_supervisor.clone(),
        sidecar_environment,
        lifecycle,
    );
    // Serve `workflow` tool requests from a dedicated worker loop sharing the
    // team supervisor's stop token; runs go through the governed core executor.
    spawn_workflow_supervisor(
        workflow_rx,
        workflow_control.clone(),
        lifecycle.stop.clone(),
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
        workflow_control,
        asks: Some(asks),
        questions: Some(questions),
        mcp_control,
        lifecycle,
        agent_model_control,
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
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
    _built: BuiltSessionEngine,
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
        // Server/TUI AppState: agent base only. Per-turn guidance layers
        // Environment + AGENTS + references once.
        let agent = Arc::new(agent_base_with_model(&runtime.model, None));
        let mut built = build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission, runtime.settings),
        )
        .await?;
        let engine = built.engine();
        let questions = built.take_questions().ok_or_else(|| {
            anyhow::anyhow!("BuiltSessionEngine questions receiver already taken")
        })?;
        let asks = built
            .take_asks()
            .ok_or_else(|| anyhow::anyhow!("BuiltSessionEngine asks receiver already taken"))?;
        let mcp_control = built.mcp_control();
        let workflow_control = Arc::new(built.workflow_control());
        let agent_model_control = Arc::new(built.agent_model_control());
        let mut state = hya_server::AppState::new(engine.clone(), agent)
            .with_provider_control(Arc::new(crate::ProviderManager::new(engine.clone())))
            .with_bundle_control(Arc::new(crate::bundle_admin::BundleManager))
            .with_question_requests(questions)
            .with_mcp_control(mcp_control)
            .with_workflow_control(workflow_control)
            .with_agent_model_control(agent_model_control)
            .with_default_agent(runtime.default_agent.clone())
            .with_auto_title(true);
        state = state.with_permission_requests(asks);
        // Remembered "allow always" grants survive restarts: reload them into
        // the process permission plane before serving.
        if let Err(error) = state.restore_saved_permissions().await {
            tracing::warn!(%error, "failed to restore saved permission grants");
        }
        built.resume_interrupted(&state).await?;
        let app_state = state.clone();
        let router = hya_server::router(state);
        Ok(Self {
            router,
            engine,
            app_state,
            _built: built,
        })
    }

    /// Fence server admission, drain composition and residents, then join server services.
    pub async fn shutdown(&mut self) -> Result<(), CoreError> {
        self.app_state.begin_shutdown();
        let result = self._built.shutdown().await;
        self.app_state.shutdown().await;
        result
    }

    /// Axum router with native and Compat routes (from [`hya_server::router`]).
    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    /// Shared session engine for in-process callers (same instance the router uses).
    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.engine.clone()
    }

    /// Clone of the HTTP `AppState` wrapped by the router.
    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.app_state.clone()
    }
}

#[cfg(test)]
mod tests;
