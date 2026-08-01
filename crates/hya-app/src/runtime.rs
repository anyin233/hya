// allow: SIZE_OK — reviewed Phase 1 keeps backend bootstrap glue in this public API module.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use hya_bundle::{BundleCatalog, PreparedCatalog, SpawnLifecycle};
use hya_core::{
    AgentResourcePolicy, AgentSpec, CategoryRegistry, CompactionConfig, CoreError, CreateSession,
    EventBus, MemberSpec, MemberStatus, ModelSummarizer, PromptEnv, ResidentSupervisor,
    RuntimeRegistry, SessionEngine, SpawnAdmissionOutcome, SubagentGovernor, Summarizer,
    TeamEvidenceEnvelope, TurnBinding, build_system_prompt, project_envelope,
    project_envelope_for_actor, run_mailbox_service, run_pre_admitted_team,
    run_pre_admitted_team_for_actor,
};

// Single discovery/date implementation lives in hya-core; re-export for callers.
pub use hya_core::{discover_context_files, today};
use hya_mcp::McpServerConfig;
use hya_plugin::HostInfo;
use hya_plugin::config::PluginSpec;
use hya_proto::{AgentName, MemberId, ModelRef, OwnerRunId, SessionId, SubagentMode};
use hya_provider::{DevProvider, ProviderRouter, ReasoningEffort};
use hya_store::{AdmissionTerminal, SessionStore, StoreError};
use hya_tool::{
    Action, AskRequest, InteractionPlane, InvocationPolicy, MailboxPlane, MemberOutcome, Mode,
    PermissionModel, PermissionPlane, PermissionRules, QuestionRequest, Rule, SpawnError,
    SpawnMember, SpawnRequest, SpawnerPlane, ToolRegistry, WebSearchConfig, WebSearchPlane,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use crate::config;
use crate::runtime_reconcile::{
    DesiredSource, PreparedFailure, PreparedResult, RuntimeMcpControl, RuntimeReconciler, SourceId,
    prepare_desired_source, prepared_plugin_source,
};
use crate::{formatter_config, plugins};

const BUILTIN_BUNDLES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/builtin-bundles.json"));
const BUILTIN_BUNDLES_DIGEST: &str =
    include_str!(concat!(env!("OUT_DIR"), "/builtin-bundles.sha256"));

/// Injectable decode path for invalid/tamper unit tests. Production bootstrap
/// uses [`builtin_catalog`], which caches the embedded artifact once.
#[cfg(test)]
fn builtin_catalog_from(bytes: &[u8], expected_digest: &str) -> anyhow::Result<Arc<BundleCatalog>> {
    let prepared = PreparedCatalog::decode(bytes, expected_digest)
        .context("decode embedded built-in AgentBundle catalog")?;
    let catalog = BundleCatalog::from_prepared(prepared.bundles())
        .context("validate embedded built-in AgentBundle catalog")?;
    Ok(Arc::new(catalog))
}

/// Decode and validate the build-embedded prepared catalog exactly once.
///
/// Success and failure are both cached so a corrupt embedded artifact stays
/// fail-closed for the process lifetime without silent retry or substitution.
/// Read-only public accessor for consumers (e.g. `hya-backend` agent list) that
/// must share the same process-wide `Arc` — no second embed, decode, or cache.
/// Injectable [`builtin_catalog_from`] remains uncached for tamper tests.
pub fn builtin_catalog() -> anyhow::Result<Arc<BundleCatalog>> {
    use hya_bundle::BundleError;

    static EMBEDDED_CATALOG: OnceLock<Result<Arc<BundleCatalog>, BundleError>> = OnceLock::new();
    match EMBEDDED_CATALOG.get_or_init(|| {
        PreparedCatalog::decode(BUILTIN_BUNDLES, BUILTIN_BUNDLES_DIGEST)
            .and_then(|prepared| BundleCatalog::from_prepared(prepared.bundles()))
            .map(Arc::new)
    }) {
        Ok(catalog) => Ok(Arc::clone(catalog)),
        Err(error) => {
            Err(anyhow::Error::new(error.clone())
                .context("load embedded built-in AgentBundle catalog"))
        }
    }
}

pub fn host_info() -> HostInfo {
    HostInfo {
        name: "hya".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

pub fn offline_router(model_override: Option<String>) -> (ProviderRouter, String) {
    let router = ProviderRouter::new().with(Arc::new(DevProvider::new()));
    (
        router,
        model_override.unwrap_or_else(|| "offline".to_string()),
    )
}

fn process_owner_run_id() -> OwnerRunId {
    static OWNER_RUN_ID: OnceLock<OwnerRunId> = OnceLock::new();
    *OWNER_RUN_ID.get_or_init(OwnerRunId::new)
}

pub fn compaction_config() -> CompactionConfig {
    let default = CompactionConfig::default();
    let token_threshold = std::env::var("HYA_COMPACTION_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.token_threshold);
    let keep_recent = std::env::var("HYA_COMPACTION_KEEP_RECENT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default.keep_recent);
    CompactionConfig {
        token_threshold,
        keep_recent,
    }
}

/// Canonical Harness agent base string (no Environment / AGENTS).
pub const HARNESS_AGENT_BASE: &str = "You are hya, a coding agent.";

/// Agent base only — for HTTP/SSE server and interactive TUI AppState assembly.
///
/// Bundle `prompt=None` keeps this base; per-turn server discovery appends
/// Environment + current workdir AGENTS + references. Baking those at startup
/// would duplicate AGENTS when guidance is also layered.
pub fn agent_base_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt: HARNESS_AGENT_BASE.to_string(),
        workdir: PathBuf::from("."),
        reasoning,
    }
}

/// Direct-mode agent (exec/RPC/goal): base + Environment + process-cwd AGENTS.
///
/// These paths call `run_turn` without a separate guidance layer, so context
/// must remain composed into `system_prompt` here.
pub fn agent_with_model(model: &str, reasoning: Option<ReasoningEffort>) -> AgentSpec {
    let workdir = PathBuf::from(".");
    let env = PromptEnv {
        cwd: std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        platform: std::env::consts::OS.to_string(),
        date: today(),
    };
    let context = discover_context_files(&workdir);
    let system_prompt = build_system_prompt(HARNESS_AGENT_BASE, &env, &context);
    AgentSpec {
        name: AgentName::new("build"),
        model: ModelRef::new(model),
        system_prompt,
        workdir,
        reasoning,
    }
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

pub struct RuntimeConfig {
    pub router: ProviderRouter,
    pub model: String,
    pub reasoning: Option<ReasoningEffort>,
    pub models: Vec<config::ModelEntry>,
    pub mcp: BTreeMap<String, McpServerConfig>,
    pub plugins: Vec<PluginSpec>,
    pub default_agent: Option<String>,
    /// Logical model categories the runtime resolves at subagent spawn time.
    pub categories: CategoryRegistry,
    /// Set when no usable config was found and the offline provider was chosen.
    /// Interactive startup emits it; headless/machine-readable modes ignore it.
    pub offline_notice: Option<OfflineNotice>,
    pub permission: InvocationPolicy,
    pub websearch: WebSearchConfig,
}

impl RuntimeConfig {
    #[must_use]
    pub fn with_yolo(mut self, yolo: bool) -> Self {
        if yolo {
            self.permission = self.permission.with_model(PermissionModel::Danger);
        }
        self
    }
}

/// Resolve a provider router + active model from hya's config, falling back
/// to the offline echo provider when no usable config is present.
pub fn resolve_runtime(model_override: Option<String>) -> RuntimeConfig {
    match config::load() {
        Ok(Some(cfg)) => {
            let (fallback_router, fallback_model) = offline_router(model_override.clone());
            let default_model = if cfg.default_model.is_empty() {
                fallback_model
            } else {
                cfg.default_model
            };

            let model = model_override
                .or_else(|| std::env::var("HYA_MODEL").ok())
                .unwrap_or(default_model);
            let reasoning = cfg
                .models
                .iter()
                .find(|entry| entry.matches_model_ref(&model))
                .and_then(|entry| entry.reasoning_default);
            RuntimeConfig {
                router: if cfg.has_providers {
                    cfg.router
                } else {
                    fallback_router
                },
                model,
                reasoning,
                models: cfg.models,
                mcp: cfg.mcp,
                plugins: plugins::resolve(cfg.plugins, plugins::plugins_dir().as_deref()),
                default_agent: cfg.default_agent,
                categories: cfg.categories,
                offline_notice: None,
                permission: cfg.permission,
                websearch: cfg.websearch,
            }
        }
        Ok(None) => {
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: None,
                categories: CategoryRegistry::default(),
                offline_notice: Some(OfflineNotice {
                    config_path: config::expected_config_path(),
                }),
                permission: InvocationPolicy::default(),
                websearch: WebSearchConfig::default(),
            }
        }
        Err(e) => {
            eprintln!("hya: config error ({e:#}); using the offline provider");
            let (router, model) = offline_router(model_override);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: None,
                categories: CategoryRegistry::default(),
                offline_notice: None,
                permission: InvocationPolicy::default().with_model(PermissionModel::Strict),
                websearch: WebSearchConfig::default(),
            }
        }
    }
}

pub async fn open_store(db: &str) -> anyhow::Result<SessionStore> {
    if db.is_empty() {
        SessionStore::connect_memory()
            .await
            .context("open in-memory store")
    } else {
        SessionStore::connect(db)
            .await
            .with_context(|| format!("open store at {db}"))
    }
}

#[derive(Serialize)]
struct SpawnRequestFingerprint<'a> {
    domain: &'static str,
    parent: SessionId,
    background: bool,
    members: &'a [SpawnMember],
}

fn spawn_request_fingerprint(req: &SpawnRequest) -> Result<[u8; 32], serde_json::Error> {
    let canonical = serde_json::to_vec(&SpawnRequestFingerprint {
        domain: "hya.spawn-admission.v1",
        parent: req.parent,
        background: req.background,
        members: &req.members,
    })?;
    Ok(Sha256::digest(canonical).into())
}

struct ResolvedSpawnMember {
    request: SpawnMember,
    authorized_target: AgentName,
    agent: AgentSpec,
    agents: Arc<[hya_tool::AgentDef]>,
    resources: AgentResourcePolicy,
    resident: bool,
    /// Immutable guidance Arc cloned from the spawn request (not from disk).
    guidance: Option<Arc<str>>,
}

/// Pre-admission team-root context for main-as-actor synthesis on resident batches.
///
/// Bound from the spawn TurnBinding + root projection stable AgentName — never from
/// a nested caller's roster/resource policy.
struct MainActivationContext {
    root: SessionId,
    agent: AgentSpec,
    agents: Arc<[hya_tool::AgentDef]>,
    resources: AgentResourcePolicy,
    guidance: Option<Arc<str>>,
}

/// Resolve exact team root + root definition/roster/resource policy before durable
/// spawn admission. Fail-closed; no parent-fallback and no catalog/base synthesis.
async fn resolve_main_activation_context(
    engine: &SessionEngine,
    binding: &TurnBinding,
    base: &AgentSpec,
    parent: SessionId,
    guidance: Option<Arc<str>>,
) -> Result<MainActivationContext, SpawnError> {
    let (root, _) = engine
        .session_lineage(parent)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_projection = engine
        .read_projection(root)
        .await
        .map_err(|_| SpawnError::Unavailable)?;
    let root_agent_name = root_projection
        .session
        .agent
        .as_ref()
        .ok_or(SpawnError::Unavailable)?;
    let root_stable = root_agent_name.as_str();
    // Exact catalog lookup for the root stable id; map missing definition through
    // the existing typed UnknownAgentId seam (no general/base fallback).
    if binding.resolve_agent(root_stable).is_none() {
        return Err(SpawnError::UnknownAgentId {
            agent_id: root_stable.to_string(),
        });
    }
    let agent = engine
        .agent_spec_for_binding(binding, base, root_stable)
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = engine
        .agent_roster_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    let resources = engine
        .agent_resource_policy_for_binding(binding, root_stable)
        .map_err(|err| match err {
            CoreError::Bundle(hya_bundle::BundleError::UnknownAgentId { agent_id })
            | CoreError::AgentDefinitionMissing { agent_id } => {
                SpawnError::UnknownAgentId { agent_id }
            }
            _ => SpawnError::Unavailable,
        })?;
    Ok(MainActivationContext {
        root,
        agent,
        agents,
        resources,
        guidance,
    })
}

/// Batch-scoped shared inputs for [`resolve_spawn_member`].
///
/// One captured [`TurnBinding`] and request guidance Arc per batch; [`SpawnMember`]
/// remains the only per-member argument.
struct ResolveSpawnMemberCtx<'a> {
    engine: &'a SessionEngine,
    binding: &'a TurnBinding,
    base: &'a AgentSpec,
    caller: &'a str,
    allowed_agents: &'a [hya_tool::AgentDef],
    categories: &'a CategoryRegistry,
    is_servable: &'a dyn Fn(&ModelRef) -> bool,
    guidance: Option<Arc<str>>,
}

fn resolve_spawn_member(
    ctx: &ResolveSpawnMemberCtx<'_>,
    member: SpawnMember,
) -> Result<ResolvedSpawnMember, SpawnError> {
    let requested = member.subagent_type.trim();
    let requested = if requested.is_empty() {
        "general"
    } else {
        requested
    };
    let definition =
        ctx.binding
            .resolve_agent(requested)
            .ok_or_else(|| SpawnError::UnknownAgentId {
                agent_id: requested.to_string(),
            })?;
    if !ctx
        .allowed_agents
        .iter()
        .any(|allowed| allowed.name == definition.stable_id.as_str())
    {
        return Err(SpawnError::AgentSpawnNotAllowed {
            caller: ctx.caller.to_string(),
            agent_id: definition.stable_id.as_str().to_string(),
        });
    }
    let authorized_target = definition.stable_id.clone();
    let mut agent = ctx
        .engine
        .agent_spec_for_binding(ctx.binding, ctx.base, definition.stable_id.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let agents = ctx
        .engine
        .agent_roster_for_binding(ctx.binding, definition.stable_id.as_str())
        .map_err(|_| SpawnError::Unavailable)?;
    let resources = ctx
        .engine
        .agent_resource_policy_for_binding(ctx.binding, definition.stable_id.as_str())
        .map_err(|_| SpawnError::Unavailable)?;

    let resolve_category = |name: &str| {
        ctx.categories
            .resolve_servable(name, ctx.is_servable)
            .map(|resolved| resolved.model)
    };
    if let Some(model) = definition
        .model_policy
        .category
        .as_deref()
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.category.as_deref())
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = definition.model_policy.model.as_deref() {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .inline_agent
        .as_ref()
        .and_then(|inline| inline.model.as_deref())
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }
    if let Some(model) = member
        .category
        .as_deref()
        .map(str::trim)
        .filter(|category| !category.is_empty())
        .and_then(&resolve_category)
    {
        agent.model = model;
    }
    if let Some(model) = member
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        agent.model = ModelRef::new(model);
    }

    let mut resident = definition.spawn_lifecycle == SpawnLifecycle::Resident || member.resident;
    if let Some(inline) = member.inline_agent.as_ref() {
        if inline.description.is_some() {
            return Err(SpawnError::UnsupportedInlineAgentField {
                field: "description",
            });
        }
        resident |= inline.resident.unwrap_or(false);
        if !inline.prompt.trim().is_empty() {
            agent.system_prompt = inline.prompt.clone();
        }
        if !inline.name.trim().is_empty() {
            agent.name = AgentName::new(&inline.name);
        }
    }

    Ok(ResolvedSpawnMember {
        request: member,
        authorized_target,
        agent,
        agents,
        resources,
        resident,
        guidance: ctx.guidance.clone(),
    })
}

pub fn spawn_team_supervisor(
    mut rx: tokio::sync::mpsc::Receiver<SpawnRequest>,
    engine: Arc<SessionEngine>,
    base: AgentSpec,
    router: Arc<ProviderRouter>,
    categories: Arc<CategoryRegistry>,
    resident_supervisor: Arc<ResidentSupervisor>,
) {
    tokio::spawn(async move {
        while let Some(req) = rx.recv().await {
            let parent = match engine.read_projection(req.parent).await {
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
            let workdir = parent
                .session
                .workdir
                .as_deref()
                .map(Path::new)
                .unwrap_or(&base.workdir);
            let binding = match engine.bind_runtime(workdir) {
                Ok(binding) => binding,
                Err(error) => {
                    eprintln!("hya: failed to bind spawn catalog ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            };
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
            // Resident batches need team-root main activation context resolved from
            // the same captured TurnBinding before any durable admission. Nested
            // callers must not supply their own roster/resource policy for main.
            // Transient-only batches skip this lookup entirely.
            let batch_has_resident = resolved.iter().any(|entry| entry.resident);
            let main_activation = if batch_has_resident {
                match resolve_main_activation_context(
                    &engine,
                    &binding,
                    &base,
                    req.parent,
                    request_guidance.clone(),
                )
                .await
                {
                    Ok(context) => Some(context),
                    Err(error) => {
                        let _ = req.reply.send(Err(error));
                        continue;
                    }
                }
            } else {
                None
            };
            let fingerprint = match spawn_request_fingerprint(&req) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    eprintln!("hya: failed to fingerprint spawn request ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            };
            let admission_units = u32::try_from(req.members.len()).unwrap_or(u32::MAX);
            let operation_id = req.operation.operation_id();
            let actor_claim = req.operation.actor_claim();
            let admission = engine
                .begin_spawn_admission(
                    req.parent,
                    req.operation,
                    fingerprint,
                    admission_units,
                    actor_claim,
                    req.cancel.clone(),
                )
                .await;
            match admission {
                Ok(SpawnAdmissionOutcome::Started) => {}
                Ok(SpawnAdmissionOutcome::Overloaded | SpawnAdmissionOutcome::MaxDepth) => {
                    let _ = req.reply.send(Err(SpawnError::Overloaded));
                    continue;
                }
                Ok(SpawnAdmissionOutcome::Existing(_) | SpawnAdmissionOutcome::Cancelled) => {
                    let _ = req.reply.send(Err(SpawnError::OperationAlreadyHandled));
                    continue;
                }
                Err(CoreError::Store(StoreError::OperationIdConflict { .. })) => {
                    let _ = req.reply.send(Err(SpawnError::OperationIdConflict));
                    continue;
                }
                Err(error) => {
                    eprintln!("hya: durable spawn admission failed ({error})");
                    let _ = req.reply.send(Err(SpawnError::Unavailable));
                    continue;
                }
            }
            let engine = engine.clone();
            let resident_supervisor = resident_supervisor.clone();
            tokio::spawn(async move {
                let parent = req.parent;
                let operation_cancel = req.cancel;
                let cancel = operation_cancel.clone();
                let background = req.background;
                let mut reply = Some(req.reply);
                let mut spawn_failed = false;
                let mut resident_members = Vec::new();
                let mut transient_members = Vec::new();
                for entry in resolved {
                    if entry.resident {
                        resident_members.push(entry);
                    } else {
                        transient_members.push(entry);
                    }
                }

                // Resident members are NON-BLOCKING: register each as a long-lived
                // turn is not held on their work.
                let mut resident_outcomes = Vec::new();
                if !resident_members.is_empty() {
                    // Register the team root as the main actor so child mail +
                    // quiescence can wake it. Only done when the team actually has
                    // residents, so pure-transient teams keep their old behavior.
                    // Synthesis is an ordinary activation of this main slot through
                    // the same resident_task path, seeded with pre-admission root
                    // context (first ensure_main wins on re-entry).
                    if let Some(MainActivationContext {
                        root,
                        agent: main_agent,
                        agents: main_agents,
                        resources: main_resources,
                        guidance: main_guidance,
                    }) = main_activation
                        && let Err(err) = resident_supervisor
                            .ensure_main(
                                root,
                                main_agent,
                                actor_claim.as_ref(),
                                main_agents,
                                main_resources,
                                main_guidance,
                            )
                            .await
                    {
                        eprintln!("hya: ensure_main failed ({err})");
                    }
                    for resolved in resident_members {
                        let ResolvedSpawnMember {
                            request: member,
                            authorized_target,
                            agent,
                            agents,
                            resources,
                            guidance,
                            ..
                        } = resolved;
                        let _authorized_target = authorized_target;
                        match resident_supervisor
                            .spawn_resident(
                                parent,
                                agent,
                                (agents, resources),
                                member.prompt,
                                actor_claim.as_ref(),
                                guidance,
                            )
                            .await
                        {
                            Ok((session, handle)) => resident_outcomes.push(MemberOutcome {
                                member: handle.clone(),
                                session: session.to_string(),
                                status: "running".to_string(),
                                summary: format!(
                                    "Resident {handle} is live and will act on inbound mail."
                                ),
                            }),
                            Err(err) => resident_outcomes.push(MemberOutcome {
                                member: "-".to_string(),
                                session: "-".to_string(),
                                status: "failed".to_string(),
                                summary: {
                                    spawn_failed = true;
                                    err.to_string()
                                },
                            }),
                        }
                    }
                }

                // Transient members keep the historical blocking-join semantics.
                let specs: Vec<MemberSpec> = if background {
                    let mut specs = Vec::new();
                    let mut started = resident_outcomes.clone();
                    for resolved in transient_members {
                        let ResolvedSpawnMember {
                            request: member,
                            authorized_target,
                            agent,
                            agents,
                            resources,
                            guidance,
                            ..
                        } = resolved;
                        let _authorized_target = authorized_target;
                        let id = MemberId::new();
                        let session = match member
                            .task_id
                            .as_deref()
                            .and_then(|task_id| task_id.parse::<SessionId>().ok())
                        {
                            Some(session) => session,
                            None => {
                                let create = CreateSession {
                                    parent: Some(parent),
                                    agent: agent.name.clone(),
                                    model: agent.model.clone(),
                                    workdir: agent.workdir.to_string_lossy().into_owned(),
                                };
                                let created = match actor_claim.as_ref() {
                                    Some(claim) => engine.create_for_actor(claim, create).await,
                                    None => engine.create(create).await,
                                };
                                match created {
                                    Ok(session) => session,
                                    Err(err) => {
                                        spawn_failed = true;
                                        started.push(MemberOutcome {
                                            member: id.to_string(),
                                            session: "-".to_string(),
                                            status: "failed".to_string(),
                                            summary: err.to_string(),
                                        });
                                        continue;
                                    }
                                }
                            }
                        };
                        started.push(MemberOutcome {
                            member: id.to_string(),
                            session: session.to_string(),
                            status: "running".to_string(),
                            summary: "The task is working in the background.".to_string(),
                        });
                        specs.push(MemberSpec {
                            id,
                            agent,
                            agents,
                            resources: Some(resources),
                            guidance,
                            directive: member.prompt,
                            description: member.description,
                            session: Some(session),
                        });
                    }
                    if let Some(reply) = reply.take() {
                        let _ = reply.send(Ok(started));
                    }
                    specs
                } else {
                    transient_members
                        .into_iter()
                        .map(|resolved| {
                            let ResolvedSpawnMember {
                                request,
                                authorized_target,
                                agent,
                                agents,
                                resources,
                                guidance,
                                ..
                            } = resolved;
                            let _authorized_target = authorized_target;
                            MemberSpec {
                                id: MemberId::new(),
                                agent,
                                agents,
                                resources: Some(resources),
                                guidance,
                                directive: request.prompt,
                                description: request.description,
                                session: request
                                    .task_id
                                    .as_deref()
                                    .and_then(|task_id| task_id.parse::<SessionId>().ok()),
                            }
                        })
                        .collect()
                };

                // Only run the blocking join when there is transient work; a pure
                // resident spawn replies immediately with the resident handles.
                let mut outcomes = resident_outcomes;
                if !specs.is_empty() {
                    let evidence = match actor_claim {
                        Some(claim) => {
                            run_pre_admitted_team_for_actor(
                                engine.clone(),
                                parent,
                                specs,
                                cancel,
                                claim,
                            )
                            .await
                        }
                        None => run_pre_admitted_team(engine.clone(), parent, specs, cancel).await,
                    };
                    spawn_failed |= evidence
                        .iter()
                        .any(|member| member.status == MemberStatus::Failed);
                    let envelope = TeamEvidenceEnvelope {
                        members: evidence.clone(),
                    };
                    let _ = match actor_claim.as_ref() {
                        Some(claim) => {
                            project_envelope_for_actor(&engine, parent, &envelope, claim).await
                        }
                        None => project_envelope(&engine, parent, &envelope).await,
                    };
                    outcomes.extend(evidence.into_iter().map(|e| MemberOutcome {
                        member: e.member,
                        session: e.session,
                        status: match e.status {
                            MemberStatus::Done => "done".to_string(),
                            MemberStatus::Failed => "failed".to_string(),
                        },
                        summary: e.summary,
                    }));
                }
                let (terminal, reason) = if operation_cancel.is_cancelled() {
                    (AdmissionTerminal::Cancelled, "spawn operation cancelled")
                } else if spawn_failed {
                    (AdmissionTerminal::Aborted, "spawn operation failed")
                } else {
                    (AdmissionTerminal::Completed, "spawn operation completed")
                };
                if let Err(error) = engine
                    .finalize_spawn_admission(operation_id, terminal, reason, actor_claim.as_ref())
                    .await
                {
                    eprintln!("hya: failed to finalize spawn admission ({error})");
                    if !background && let Some(reply) = reply.take() {
                        let _ = reply.send(Err(SpawnError::Unavailable));
                    }
                    return;
                }
                if !background && let Some(reply) = reply.take() {
                    let _ = reply.send(Ok(outcomes));
                }
            });
        }
    });
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
}

pub async fn build_session_engine(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
) -> anyhow::Result<(
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    Arc<dyn hya_server::McpControl>,
    Arc<hya_plugin::PluginHost>,
)> {
    build_session_engine_with_mcp_defer(
        store,
        router,
        agent,
        mcp,
        plugins,
        tool_config,
        EngineBuildOptions {
            defer_mcp: defer_sideplanes(),
        },
    )
    .await
}

async fn build_session_engine_with_mcp_defer(
    store: SessionStore,
    router: ProviderRouter,
    agent: &AgentSpec,
    mcp: BTreeMap<String, McpServerConfig>,
    plugins: Vec<PluginSpec>,
    tool_config: (WebSearchConfig, InvocationPolicy),
    options: EngineBuildOptions,
) -> anyhow::Result<(
    Arc<SessionEngine>,
    tokio::sync::mpsc::UnboundedReceiver<AskRequest>,
    tokio::sync::mpsc::UnboundedReceiver<QuestionRequest>,
    Arc<dyn hya_server::McpControl>,
    Arc<hya_plugin::PluginHost>,
)> {
    let owner_run_id = process_owner_run_id();
    let mut recovered_claims = Vec::new();
    for actor_id in store
        .active_actor_ids()
        .await
        .context("list resident actors before startup recovery")?
    {
        recovered_claims.push(
            store
                .recover_claim(actor_id, owner_run_id)
                .await
                .context("fence resident actor before startup recovery")?,
        );
    }
    store
        .abort_nonterminal_admissions("startup recovery")
        .await
        .context("abort nonterminal admissions before spawn readiness")?;
    let (websearch, invocation_policy) = tool_config;
    let router = Arc::new(router);
    let registry = ToolRegistry::builtins();
    if !websearch.enabled {
        registry.remove("websearch");
    }

    // Plugin hooks remain startup-bound. Their tool declarations are prepared
    // here but become effective only through RuntimeReconciler publication.
    let plugin_specs = plugins.clone();
    let (plugin_host, plugin_failures) =
        hya_plugin::PluginHost::connect_all_observed(plugins, host_info()).await;
    let plugin_host = Arc::new(plugin_host);
    let prepared_plugins = plugin_host
        .prepared_plugins()
        .into_iter()
        .map(|plugin| (plugin.id().to_string(), plugin))
        .collect::<BTreeMap<_, _>>();
    let defer_mcp = options.defer_mcp && !mcp.is_empty();
    let runtime = Arc::new(RuntimeRegistry::new(registry, builtin_catalog()?));

    let rules = PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Glob, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]);
    let (permission, asks) = PermissionPlane::new_with_policy(rules, invocation_policy);
    let permission = if plugin_host.is_empty() {
        permission
    } else {
        permission.with_interceptor(Arc::new(hya_plugin::PermissionBridge::new(
            plugin_host.clone(),
        )))
    };
    let (interaction, questions) = InteractionPlane::new();
    let subagent_limits = crate::config::load_subagent_limits();
    let spawn_queue_capacity = usize::try_from(subagent_limits.per_run_budget)
        .unwrap_or(tokio::sync::Semaphore::MAX_PERMITS)
        .clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
    let (spawner, spawn_rx) = SpawnerPlane::with_capacity(spawn_queue_capacity);
    let (mailbox, mailbox_rx) = MailboxPlane::new();
    let summarizer: Arc<dyn Summarizer> =
        Arc::new(ModelSummarizer::new(router.clone(), agent.model.clone()));
    let bus = EventBus::new(crate::config::resolve_event_bus_capacity());
    let governor = SubagentGovernor::new(subagent_limits);
    // Clone the router before it is moved into the engine so the team supervisor
    // can test category-candidate servability against the same live providers.
    let spawn_router = router.clone();
    let categories = Arc::new(crate::config::load_categories());
    let mut engine_builder = SessionEngine::new(store, router, runtime, permission, bus)
        .with_compaction(summarizer, compaction_config())
        .with_formatter(formatter_config::load_plane())
        .with_websearch(WebSearchPlane::configured(websearch))
        .with_interaction(interaction)
        .with_spawner(spawner)
        .with_mailbox(mailbox)
        .with_governor(governor);
    if !plugin_host.is_empty() {
        engine_builder = engine_builder.with_hooks(plugin_host.clone());
    }
    let engine = Arc::new(engine_builder);
    let reconciler = Arc::new(RuntimeReconciler::new(engine.runtime_registry()));
    let mcp_control = Arc::new(RuntimeMcpControl::new(reconciler.clone()));
    let plugin_desired = plugin_specs
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
        let plugin_results =
            prepared_plugin_results(plugin_plan.sources(), &prepared_plugins, &plugin_failures);
        if let Err(error) = reconciler.finish_revision(&plugin_plan, plugin_results) {
            eprintln!("hya: plugin tool reconciliation rejected ({error})");
        }

        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let deferred_plan = reconciler
            .replace_desired(desired)
            .context("plan deferred MCP reconciliation")?;
        let control_bg = mcp_control.clone();
        tokio::spawn(async move {
            if let Err(error) = control_bg.reconcile_plan(deferred_plan).await {
                eprintln!("hya: MCP runtime refresh rejected ({error})");
            }
        });
    } else {
        let mut desired = plugin_desired;
        desired.extend(mcp_desired);
        let plan = reconciler
            .replace_desired(desired)
            .context("plan startup runtime reconciliation")?;
        let mut results =
            prepared_plugin_results(plan.sources(), &prepared_plugins, &plugin_failures);
        results.extend(
            prepare_mcp_results(plan.sources())
                .await
                .context("prepare startup MCP reconciliation")?,
        );
        if let Err(error) = reconciler.finish_revision(&plan, results) {
            eprintln!("hya: startup runtime reconciliation rejected ({error})");
        }
    }
    // Drive resident (long-lived actor) subagents + quiescence (ADR-0002). Started
    // before the team supervisor so its bus subscription is live for the first mail.
    let resident_supervisor = ResidentSupervisor::start_with_owner(engine.clone(), owner_run_id);
    for recovered in recovered_claims {
        let actor_id = recovered.claim.actor_id;
        let (root, _) = engine
            .session_lineage(actor_id)
            .await
            .context("resolve recovered resident root")?;
        let root_projection = engine
            .read_projection(root)
            .await
            .context("replay recovered resident roster")?;
        let entry = root_projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == actor_id && entry.mode == SubagentMode::Resident)
            .cloned()
            .context("active resident claim has no durable roster entry")?;
        let report = engine
            .recover_resident_actor(&recovered, root, &entry.handle)
            .await
            .context("terminalize recovered resident work")?;
        let actor_projection = engine
            .read_projection(actor_id)
            .await
            .context("replay recovered resident session")?;
        let workdir = actor_projection
            .session
            .workdir
            .map(PathBuf::from)
            .unwrap_or_else(|| agent.workdir.clone());
        // Resume is not a new spawn: exact catalog lookup only (no can_spawn,
        // no legacy general/base synthesis). Missing or inline-only identity fails.
        let recorded = actor_projection
            .session
            .agent
            .as_ref()
            .unwrap_or(&entry.agent_type);
        let recovered_agent = resolve_recovered_resident_agent(&engine, agent, recorded, &workdir)
            .with_context(|| {
                format!(
                    "resolve recovered resident agent `{}` from current catalog",
                    recorded.as_str()
                )
            })?;
        resident_supervisor
            .register_recovered_resident(
                root,
                entry.handle,
                recovered_agent,
                recovered,
                report.work,
            )
            .await
            .context("recreate recovered resident runtime owner")?;
    }
    spawn_team_supervisor(
        spawn_rx,
        engine.clone(),
        agent.clone(),
        spawn_router,
        categories,
        resident_supervisor,
    );
    // Drive the event-sourced mailbox: append MailSent/Channel*/AgentRegistered to
    // the team-root log and serve roster/channel reads (ADR-0001).
    tokio::spawn(run_mailbox_service(engine.clone(), mailbox_rx));
    Ok((engine, asks, questions, mcp_control, plugin_host))
}

/// Exact-resolve a process-loss recovered resident from the current RuntimeSnapshot.
///
/// Binds once from the recorded session workdir and uses the same production
/// TurnBinding catalog projection as live turns. Resume is definition resolution,
/// not a new spawn: no `can_spawn`, no AgentSpec synthesis, no general/base fallback.
fn resolve_recovered_resident_agent(
    engine: &SessionEngine,
    base: &AgentSpec,
    recorded_agent: &AgentName,
    session_workdir: &Path,
) -> Result<AgentSpec, CoreError> {
    let binding = engine.bind_runtime(session_workdir)?;
    engine.agent_spec_for_binding(&binding, base, recorded_agent.as_str())
}

fn prepared_plugin_results(
    desired: &[DesiredSource],
    prepared: &BTreeMap<String, hya_plugin::PreparedPlugin>,
    failures: &BTreeMap<String, hya_plugin::PluginError>,
) -> Vec<PreparedResult> {
    desired
        .iter()
        .filter(|source| source.id().kind() == hya_core::RuntimeSourceKind::Plugin)
        .map(|source| {
            let id = source.id().configured_id();
            if let Some(plugin) = prepared.get(id) {
                PreparedResult::from(prepared_plugin_source(plugin.clone()))
            } else {
                let error = failures.get(id).map_or_else(
                    || "PLUGIN_START_FAILED: no observed result".to_string(),
                    |error| format!("PLUGIN_START_FAILED: {error}"),
                );
                PreparedResult::from(PreparedFailure::new(source.id().clone(), error))
            }
        })
        .collect()
}

async fn prepare_mcp_results(
    desired: &[DesiredSource],
) -> Result<Vec<PreparedResult>, crate::runtime_reconcile::ReconcileError> {
    let mut set = tokio::task::JoinSet::new();
    let mut tasks = BTreeMap::new();
    for source in desired
        .iter()
        .filter(|source| source.id().kind() == hya_core::RuntimeSourceKind::Mcp)
        .cloned()
    {
        let id = source.id().clone();
        let handle = set.spawn(prepare_desired_source(source));
        tasks.insert(handle.id(), id);
    }
    let mut results = Vec::new();
    while let Some(joined) = set.join_next_with_id().await {
        match joined {
            Ok((id, result)) => {
                tasks.remove(&id);
                results.push(result);
            }
            Err(error) => {
                let Some(source) = tasks.remove(&error.id()) else {
                    return Err(crate::runtime_reconcile::ReconcileError::InvalidPrepared(
                        format!("MCP preparation task {} had no source ticket", error.id()),
                    ));
                };
                results.push(PreparedResult::from(PreparedFailure::new(
                    source,
                    format!("MCP_PREPARE_TASK_FAILED: {error}"),
                )));
            }
        }
    }
    Ok(results)
}

pub struct RuntimeOptions {
    pub model: Option<String>,
    pub db: String,
    pub yolo: bool,
    pub default_agent: Option<String>,
    pub force_offline: bool,
}

pub struct HyaRuntime {
    router: axum::Router,
    engine: Arc<SessionEngine>,
    app_state: hya_server::AppState,
    _plugin_host: Arc<hya_plugin::PluginHost>,
}

impl HyaRuntime {
    pub async fn start(opts: RuntimeOptions) -> anyhow::Result<Self> {
        let store = open_store(&opts.db).await?;
        let runtime = if opts.force_offline {
            let (router, model) = offline_router(opts.model);
            RuntimeConfig {
                router,
                model,
                reasoning: None,
                models: Vec::new(),
                mcp: BTreeMap::new(),
                plugins: Vec::new(),
                default_agent: opts.default_agent,
                categories: CategoryRegistry::default(),
                offline_notice: None,
                permission: InvocationPolicy::default(),
                websearch: WebSearchConfig::default(),
            }
        } else {
            let mut runtime = resolve_runtime(opts.model);
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
        // Environment + current workdir AGENTS + references once.
        let agent = Arc::new(agent_base_with_model(&runtime.model, runtime.reasoning));
        let (engine, asks, questions, mcp_control, plugin_host) = build_session_engine(
            store,
            runtime.router,
            agent.as_ref(),
            runtime.mcp,
            runtime.plugins,
            (runtime.websearch, runtime.permission),
        )
        .await?;
        let mut state = hya_server::AppState::new(engine.clone(), agent)
            .with_question_requests(questions)
            .with_mcp_control(mcp_control)
            .with_workspace_adapters(plugin_host.workspace_adapters())
            .with_default_agent(runtime.default_agent.clone());
        state = state.with_permission_requests(asks);
        let app_state = state.clone();
        let router = hya_server::router(state);
        Ok(Self {
            router,
            engine,
            app_state,
            _plugin_host: plugin_host,
        })
    }

    pub fn router(&self) -> &axum::Router {
        &self.router
    }

    #[must_use]
    pub fn engine(&self) -> Arc<SessionEngine> {
        self.engine.clone()
    }

    #[must_use]
    pub fn app_state(&self) -> hya_server::AppState {
        self.app_state.clone()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::{
        AgentRole, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy, PreparedAgent,
        PreparedBundle, ResourceView,
    };
    use hya_core::CategoryEntry;
    use hya_proto::{
        Event, MailEndpoint, MailKind, OwnerRunId, RosterStatus, SubagentMode, ToolName, ToolSchema,
    };
    use hya_tool::{
        AgentDef, InlineAgent, PermissionModel, Tool, ToolCtx, ToolError, ToolPermission,
        ToolRegistry,
    };
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resident_owner_run_id_is_stable_for_the_process() {
        assert_eq!(process_owner_run_id(), process_owner_run_id());
    }

    fn hex_digest(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        let mut encoded = String::with_capacity(digest.len() * 2);
        for byte in digest {
            use std::fmt::Write as _;
            assert!(write!(encoded, "{byte:02x}").is_ok());
        }
        encoded
    }

    #[test]
    fn zero_bundle_prepared_document_cannot_bootstrap_registry_catalog() {
        let bytes = br#"{"format_version":1,"bundles":[],"index":[]}"#;
        let digest = hex_digest(bytes);
        match builtin_catalog_from(bytes, &digest) {
            Ok(catalog) => panic!(
                "zero-bundle prepared must not bootstrap a catalog for RuntimeRegistry, got {} bundles",
                catalog.bundles().len()
            ),
            Err(err) => {
                let message = format!("{err:#}");
                assert!(
                    message.contains("validate embedded built-in AgentBundle catalog"),
                    "empty prepared must surface validate context, got: {message}"
                );
                assert!(
                    message.contains("no bundles") || message.contains("empty"),
                    "empty prepared must fail closed as empty-catalog integrity, got: {message}"
                );
            }
        }
    }

    #[test]
    fn corrupted_prepared_bytes_or_digest_fail_closed_with_decode_context() {
        let wrong_digest =
            builtin_catalog_from(BUILTIN_BUNDLES, "0".repeat(64).as_str()).expect_err("digest");
        let wrong_digest_message = format!("{wrong_digest:#}");
        assert!(
            wrong_digest_message.contains("decode embedded built-in AgentBundle catalog"),
            "digest mismatch must keep decode context, got: {wrong_digest_message}"
        );
        assert!(
            wrong_digest.chain().any(|cause| cause
                .downcast_ref::<hya_bundle::BundleError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        hya_bundle::BundleError::PreparedDigestMismatch { .. }
                    )
                })),
            "digest mismatch must remain typed BundleError, got: {wrong_digest_message}"
        );

        let garbage = b"not-a-prepared-catalog";
        let garbage_digest = hex_digest(garbage);
        let decode_err = builtin_catalog_from(garbage, &garbage_digest).expect_err("corrupt bytes");
        let decode_message = format!("{decode_err:#}");
        assert!(
            decode_message.contains("decode embedded built-in AgentBundle catalog"),
            "corrupt bytes must keep decode context, got: {decode_message}"
        );
        assert!(
            decode_err.chain().any(|cause| {
                cause
                    .downcast_ref::<hya_bundle::BundleError>()
                    .is_some_and(|error| {
                        matches!(error, hya_bundle::BundleError::PreparedDecode { .. })
                    })
            }),
            "corrupt bytes must remain typed PreparedDecode, got: {decode_message}"
        );
    }

    #[test]
    fn builtin_catalog_initializes_once_and_shares_arc() {
        let first = builtin_catalog().expect("embedded catalog must load");
        let second = builtin_catalog().expect("embedded catalog must load");
        assert!(
            Arc::ptr_eq(&first, &second),
            "embedded prepared catalog must initialize once and be shared"
        );
        assert!(
            !first.bundles().is_empty(),
            "shared embedded catalog must not be empty"
        );
    }

    struct RuntimeMarker(&'static str);

    fn mcp_fixture() -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if "id" not in req:
        continue
    if req["method"] == "initialize":
        result = {"capabilities": {}}
    elif req["method"] == "tools/list":
        result = {"tools": [{"name":"ping","description":"Ping","inputSchema":{"type":"object"}}]}
    else:
        result = {"content":{"ok":True},"isError":False}
    print(json.dumps({"jsonrpc":"2.0","id":req["id"],"result":result}), flush=True)
"#
            .to_string(),
        ]
    }

    fn plugin_fixture(id: &str) -> Vec<String> {
        vec![
            "python3".to_string(),
            "-c".to_string(),
            format!(
                r#"
import json, sys
for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "initialize":
        result = {{
            "protocol_version":1,
            "plugin":{{"id":"{id}","version":"1","kind":"rust"}},
            "hooks":[],
            "tools":[{{"name":"plugin_ping","description":"Ping","inputSchema":{{"type":"object"}}}}]
        }}
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":result}}), flush=True)
    elif req.get("method") == "shutdown":
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{}}}}), flush=True)
        sys.exit(0)
    elif "id" in req:
        print(json.dumps({{"jsonrpc":"2.0","id":req["id"],"result":{{"ok":True,"output":{{}}}}}}), flush=True)
"#
            ),
        ]
    }

    #[async_trait]
    impl Tool for RuntimeMarker {
        fn name(&self) -> &str {
            self.0
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new(self.0),
                description: format!("{} runtime marker", self.0),
                input_schema: json!({ "type": "object" }),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({ "ok": true }))
        }
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        home: Option<std::ffi::OsString>,
        xdg_config_home: Option<std::ffi::OsString>,
        current_dir: PathBuf,
    }

    impl EnvGuard {
        fn set(home: &Path, cwd: &Path) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let guard = Self {
                _lock: lock,
                home: std::env::var_os("HOME"),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                current_dir: std::env::current_dir().unwrap(),
            };
            std::fs::create_dir_all(home).unwrap();
            std::fs::create_dir_all(cwd).unwrap();
            unsafe {
                std::env::set_var("HOME", home);
                std::env::set_var("XDG_CONFIG_HOME", home);
            }
            std::env::set_current_dir(cwd).unwrap();
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.current_dir);
            unsafe {
                if let Some(home) = &self.home {
                    std::env::set_var("HOME", home);
                } else {
                    std::env::remove_var("HOME");
                }
                if let Some(xdg_config_home) = &self.xdg_config_home {
                    std::env::set_var("XDG_CONFIG_HOME", xdg_config_home);
                } else {
                    std::env::remove_var("XDG_CONFIG_HOME");
                }
            }
        }
    }

    fn tempdir() -> PathBuf {
        static NEXT_TEMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_TEMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "hya-app-runtime-test-{nanos}-{serial}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn offline_notice_names_path_offline_mode_and_the_fix() {
        let notice = OfflineNotice {
            config_path: PathBuf::from("/home/u/.config/hya/config.yaml"),
        };
        let text = notice.render();
        // (a) where the missing config is expected,
        assert!(text.contains("/home/u/.config/hya/config.yaml"));
        // (b) that we are in offline/echo mode,
        assert!(text.contains("OFFLINE"));
        assert!(text.contains("echoes"));
        // (c) how to fix it.
        assert!(text.contains("hya login"));
        assert!(text.contains("docs/configuration.md"));
    }

    #[test]
    fn resolve_runtime_without_config_carries_but_does_not_print_the_notice() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        let _ = std::fs::remove_file(&config_path);

        let runtime = resolve_runtime(None);

        // Offline fallback selected: the built-in echo provider + "offline" model.
        assert_eq!(runtime.model, "offline");
        // The guidance is returned as DATA — resolve_runtime itself prints
        // nothing, so headless/RPC/serve callers (which never call `emit`) keep
        // a clean machine-readable stdout. Only interactive startup emits it.
        let notice = runtime
            .offline_notice
            .expect("missing-config path must carry an offline notice");
        assert!(notice.config_path.ends_with("hya/config.yaml"));
        assert!(notice.render().contains("OFFLINE"));
    }

    #[test]
    fn permission_only_config_is_kept_and_config_errors_fall_back_to_strict() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "permission:\n  model: allow\n").unwrap();

        let runtime = resolve_runtime(None);
        assert_eq!(runtime.model, "offline");
        assert_eq!(runtime.permission.model(), PermissionModel::Allow);
        assert!(runtime.offline_notice.is_none());
        assert_eq!(
            runtime.with_yolo(true).permission.model(),
            PermissionModel::Danger
        );

        std::fs::write(
            &config_path,
            "permission:\n  rules:\n    - target: tool\n      selector: '('\n      permission: Allow\n",
        )
        .unwrap();
        let fallback = resolve_runtime(None);
        assert_eq!(fallback.permission.model(), PermissionModel::Strict);
    }

    #[test]
    fn websearch_only_config_reaches_runtime() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "tools:\n  websearch:\n    provider: parallel\n    endpoint: https://search.example.test/mcp\n    key: secret\n    enabled: false\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None);

        assert_eq!(
            runtime.websearch.provider,
            hya_tool::WebSearchProvider::Parallel
        );
        assert_eq!(
            runtime.websearch.endpoint.as_deref(),
            Some("https://search.example.test/mcp")
        );
        assert_eq!(runtime.websearch.key.as_deref(), Some("secret"));
        assert!(!runtime.websearch.enabled);
        assert!(runtime.offline_notice.is_none());
    }

    #[tokio::test]
    async fn disabled_websearch_is_not_exposed_by_engine() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let (engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
            store,
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (
                WebSearchConfig {
                    enabled: false,
                    ..WebSearchConfig::default()
                },
                InvocationPolicy::default(),
            ),
        )
        .await
        .unwrap();

        assert!(
            engine
                .tool_schemas()
                .iter()
                .all(|schema| schema.name.as_str() != "websearch")
        );
    }

    #[tokio::test]
    async fn engine_snapshot_rejects_builder_bypass_and_publishes_deferred_set_atomically() {
        let (router, _model) = offline_router(None);
        let builder = Arc::new(ToolRegistry::builtins());
        let (permission, _asks) = PermissionPlane::new(PermissionRules::default());
        let engine = Arc::new(SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(router),
            Arc::new(RuntimeRegistry::from_snapshot(
                builder.snapshot(),
                builtin_catalog().unwrap(),
            )),
            permission,
            EventBus::default(),
        ));

        builder
            .register(Arc::new(RuntimeMarker("builder_bypass")))
            .unwrap();
        assert!(
            engine
                .tool_schemas()
                .iter()
                .all(|schema| schema.name.as_str() != "builder_bypass"),
            "mutating the retained candidate builder must not change the effective snapshot"
        );

        let saw_complete_old_view = Arc::new(AtomicBool::new(false));
        let observed = saw_complete_old_view.clone();
        let inspect_engine = engine.clone();
        let mut next = 0;
        let deferred_tools = std::iter::from_fn(move || {
            let item = match next {
                0 => Some(Arc::new(RuntimeMarker("mcp__deferred__first")) as Arc<dyn Tool>),
                1 => {
                    let visible = inspect_engine
                        .tool_schemas()
                        .into_iter()
                        .map(|schema| schema.name.to_string())
                        .collect::<Vec<_>>();
                    observed.store(
                        !visible.iter().any(|name| name == "mcp__deferred__first")
                            && !visible.iter().any(|name| name == "mcp__deferred__second"),
                        Ordering::SeqCst,
                    );
                    Some(Arc::new(RuntimeMarker("mcp__deferred__second")) as Arc<dyn Tool>)
                }
                _ => None,
            };
            next += 1;
            item
        });

        engine
            .refresh_runtime(|candidate| {
                for tool in deferred_tools {
                    candidate.register_tool_with_permission(tool, ToolPermission::Mcp)?;
                }
                Ok(())
            })
            .unwrap();

        assert!(
            saw_complete_old_view.load(Ordering::SeqCst),
            "the first candidate member became visible before atomic publication"
        );
        let visible = engine
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.to_string())
            .collect::<Vec<_>>();
        assert!(visible.iter().any(|name| name == "mcp__deferred__first"));
        assert!(visible.iter().any(|name| name == "mcp__deferred__second"));
        assert!(!visible.iter().any(|name| name == "builder_bypass"));
    }

    #[tokio::test]
    async fn engine_build_aborts_nonterminal_admissions_before_spawn_readiness() {
        let database = tempdir().join("admission-recovery.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let source_tool_call_id = hya_proto::ToolCallId::new();
        let operation_id = hya_proto::OperationId::from_tool_call(source_tool_call_id);
        let root_session = SessionId::new();
        store
            .claim_admission(&hya_store::AdmissionClaim {
                operation_id,
                source_tool_call_id,
                root_session,
                request_fingerprint: [17; 32],
                admission_units: 1,
                actor_claim: None,
            })
            .await
            .unwrap();
        store.start_admission(operation_id, None).await.unwrap();
        drop(store);
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);

        let _ = build_session_engine(
            store.clone(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();

        let recovered = store.admission(operation_id).await.unwrap().unwrap();
        assert_eq!(recovered.state, hya_store::AdmissionState::Aborted);
        assert!(recovered.logical_released);
        assert!(store.replay(root_session).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn engine_build_fences_running_resident_and_resumes_queued_mail_before_readiness() {
        let database = tempdir().join("resident-recovery.db");
        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let queued_root = SessionId::new();
        let queued_actor = SessionId::new();
        let running_root = SessionId::new();
        let running_actor = SessionId::new();

        for (root, actor) in [(queued_root, queued_actor), (running_root, running_actor)] {
            store
                .append_event(
                    root,
                    &Event::SessionCreated {
                        session: root,
                        parent: None,
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                    },
                )
                .await
                .unwrap();
            store
                .append_event(
                    actor,
                    &Event::SessionCreated {
                        session: actor,
                        parent: Some(root),
                        agent: agent.name.clone(),
                        model: agent.model.clone(),
                        workdir: agent.workdir.to_string_lossy().into_owned(),
                    },
                )
                .await
                .unwrap();
        }

        let queued_claim = store
            .try_claim_new(queued_actor, OwnerRunId::new())
            .await
            .unwrap();
        store
            .commit_resident_mutation(
                &queued_claim,
                queued_root,
                &[Event::AgentRegistered {
                    session: queued_root,
                    agent_session: queued_actor,
                    handle: "queued-1".to_string(),
                    agent_type: agent.name.clone(),
                    mode: SubagentMode::Resident,
                }],
            )
            .await
            .unwrap();
        store
            .append_event(
                queued_root,
                &Event::MailSent {
                    session: queued_root,
                    from: "main".to_string(),
                    to: MailEndpoint::Handle("queued-1".to_string()),
                    kind: MailKind::Message,
                    body: "resume me".to_string(),
                },
            )
            .await
            .unwrap();

        let running_claim = store
            .try_claim_new(running_actor, OwnerRunId::new())
            .await
            .unwrap();
        store
            .commit_resident_mutation(
                &running_claim,
                running_root,
                &[
                    Event::AgentRegistered {
                        session: running_root,
                        agent_session: running_actor,
                        handle: "running-1".to_string(),
                        agent_type: agent.name.clone(),
                        mode: SubagentMode::Resident,
                    },
                    Event::ResidentWorkStarted {
                        session: running_root,
                        actor_session: running_actor,
                        handle: "running-1".to_string(),
                        epoch: running_claim.epoch,
                        inbox_through: 0,
                    },
                ],
            )
            .await
            .unwrap();
        drop(store);

        let store = SessionStore::connect(database.to_str().unwrap())
            .await
            .unwrap();
        let (_engine, _asks, _questions, _mcp, _plugins) = build_session_engine(
            store.clone(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();

        let running = store.read_projection(running_root).await.unwrap();
        let running_entry = running.team.roster.get("running-1").unwrap();
        assert_eq!(running_entry.status, RosterStatus::Failed);
        assert!(running_entry.resident_work.is_none());

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let queued = store.read_projection(queued_root).await.unwrap();
                let entry = queued.team.roster.get("queued-1").unwrap();
                if entry.resident_cursor == 1 || entry.resident_work.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deferred_mcp_returns_before_slow_child_handshake() {
        let store = SessionStore::connect_memory().await.unwrap();
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "slow".to_string(),
            hya_mcp::McpServerConfig {
                // Sleep longer than the assert budget so classic await-before-listen would fail.
                command: vec!["sleep".into(), "30".into()],
                ..hya_mcp::McpServerConfig::default()
            },
        );
        let started = std::time::Instant::now();
        let result = build_session_engine_with_mcp_defer(
            store,
            router,
            &agent,
            mcp,
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
            EngineBuildOptions { defer_mcp: true },
        )
        .await;
        let (_engine, _asks, _questions, mcp_control, _plugins) = result.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "build_session_engine blocked on MCP for {elapsed:?}"
        );
        assert_eq!(
            mcp_control.status().await.get("slow"),
            Some(&hya_mcp::McpStatus::Connecting)
        );
    }

    #[tokio::test]
    async fn startup_mixed_mcp_plugin_publishes_one_complete_generation() {
        let mut mcp = BTreeMap::new();
        mcp.insert(
            "mixed".to_string(),
            McpServerConfig {
                command: mcp_fixture(),
                timeout_ms: Some(1_000),
                ..McpServerConfig::default()
            },
        );
        let plugins = vec![PluginSpec {
            id: "mixed-plugin".to_string(),
            kind: hya_plugin::messages::PluginKindWire::Rust,
            command: plugin_fixture("mixed-plugin"),
            timeout_ms: Some(1_000),
            env: BTreeMap::new(),
            posture_overrides: BTreeMap::new(),
        }];
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let result = build_session_engine_with_mcp_defer(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            mcp,
            plugins,
            (WebSearchConfig::default(), InvocationPolicy::default()),
            EngineBuildOptions { defer_mcp: false },
        )
        .await;
        let (engine, _, _, _, _) = result.unwrap();
        let manifest = engine.runtime_registry().effective_manifest();
        assert_eq!(
            manifest.generation.get(),
            hya_proto::ConfigGeneration::INITIAL.get() + 1
        );
        assert!(manifest.sources.contains_key(&SourceId::mcp("mixed")));
        assert!(
            manifest
                .sources
                .contains_key(&SourceId::plugin("mixed-plugin"))
        );
        let names = engine
            .tool_schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__mixed__ping".to_string()));
        assert!(names.contains(&"plugin_ping".to_string()));
    }

    #[tokio::test]
    async fn compat_mcp_control_publishes_and_removes_through_one_runtime_registry() {
        let (router, model) = offline_router(None);
        let agent = agent_with_model(&model, None);
        let (engine, _, _, control, _) = build_session_engine(
            SessionStore::connect_memory().await.unwrap(),
            router,
            &agent,
            BTreeMap::new(),
            Vec::new(),
            (WebSearchConfig::default(), InvocationPolicy::default()),
        )
        .await
        .unwrap();
        let workdir = tempdir();
        let before = engine.runtime_registry().bind_turn(&workdir).unwrap();
        control
            .upsert(
                "dynamic".to_string(),
                McpServerConfig {
                    command: mcp_fixture(),
                    timeout_ms: Some(1_000),
                    ..McpServerConfig::default()
                },
            )
            .await
            .unwrap();
        let connected = engine.runtime_registry().bind_turn(&workdir).unwrap();
        assert!(before.resolve_tool("mcp__dynamic__ping").is_none());
        assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
        assert_eq!(
            control.status().await.get("dynamic"),
            Some(&hya_mcp::McpStatus::Connected)
        );

        assert!(
            control
                .set_enabled("dynamic".to_string(), false)
                .await
                .unwrap()
        );
        let removed = engine.runtime_registry().bind_turn(&workdir).unwrap();
        assert!(removed.resolve_tool("mcp__dynamic__ping").is_none());
        assert!(connected.resolve_tool("mcp__dynamic__ping").is_some());
        assert_eq!(
            control.status().await.get("dynamic"),
            Some(&hya_mcp::McpStatus::Disabled)
        );
    }

    #[test]
    fn selected_model_reasoning_default_reaches_first_agent() {
        let dir = tempdir();
        let _env = EnvGuard::set(&dir, &dir);
        let config_path = dir.join("hya/config.yaml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "default_model: gateway/gpt-5.6-sol\nproviders:\n  gateway:\n    kind: openai-response\n    base_url: https://example.test/v1\n    api_key: test\n    models:\n      - id: gpt-5.6-sol\n        reasoning:\n          default: medium\n          variants: [low, medium]\n",
        )
        .unwrap();

        let runtime = resolve_runtime(None);

        assert_eq!(
            runtime.reasoning,
            Some(hya_provider::ReasoningEffort::Medium)
        );
        assert_eq!(
            runtime.router.catalog()[0].reasoning_variants,
            ["low", "medium"]
        );
        let agent = agent_with_model(&runtime.model, runtime.reasoning);
        assert_eq!(agent.reasoning, Some(hya_provider::ReasoningEffort::Medium));
    }

    #[test]
    fn agent_with_model_omits_process_cwd_skill_index() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        write_skill(
            &workdir.join(".hya/skills/baseline"),
            "baseline-skill",
            "Baseline skill",
            "baseline body",
        );

        let agent = agent_with_model("fake", None);

        assert!(!agent.system_prompt.contains("Available skills"));
        assert!(
            !agent
                .system_prompt
                .contains("These skills are available on demand")
        );
        assert!(!agent.system_prompt.contains("baseline-skill"));
        assert!(!agent.system_prompt.contains("Baseline skill"));
    }

    /// Direct exec/RPC/goal construction still bakes Environment + AGENTS.
    #[test]
    fn agent_with_model_retains_environment_and_agents_context() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let agents_marker = "DIRECT_MODE_AGENTS_CONTEXT_MARKER";
        std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

        let agent = agent_with_model("fake", None);

        assert!(
            agent.system_prompt.contains(HARNESS_AGENT_BASE),
            "direct agent must keep harness base: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains("## Environment"),
            "direct agent must bake Environment: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains(agents_marker),
            "direct agent must bake process-cwd AGENTS: {}",
            agent.system_prompt
        );
        assert!(
            agent.system_prompt.contains("## Project context:"),
            "direct agent must use project-context separators: {}",
            agent.system_prompt
        );
    }

    /// Server/TUI AppState agent slot is base-only (no pre-baked AGENTS).
    #[test]
    fn agent_base_with_model_excludes_prebaked_agents_and_environment() {
        let home = tempdir();
        let workdir = tempdir();
        let _env = EnvGuard::set(&home, &workdir);
        let agents_marker = "SERVER_BASE_MUST_NOT_BAKE_AGENTS";
        std::fs::write(workdir.join("AGENTS.md"), agents_marker).unwrap();

        let agent = agent_base_with_model("fake", None);

        assert_eq!(agent.system_prompt, HARNESS_AGENT_BASE);
        assert!(
            !agent.system_prompt.contains("## Environment"),
            "server base must not bake Environment"
        );
        assert!(
            !agent.system_prompt.contains(agents_marker),
            "server base must not bake AGENTS (layered per turn)"
        );
        assert!(!agent.system_prompt.contains("## Project context:"));
    }

    /// Minimal engine whose catalog deliberately omits a recorded historical id.
    async fn engine_with_catalog(catalog: Arc<BundleCatalog>) -> SessionEngine {
        let runtime = Arc::new(RuntimeRegistry::from_snapshot(
            ToolRegistry::builtins().snapshot(),
            catalog,
        ));
        let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
        SessionEngine::new(
            SessionStore::connect_memory().await.unwrap(),
            Arc::new(ProviderRouter::new().with(Arc::new(DevProvider::new()))),
            runtime,
            permission,
            EventBus::default(),
        )
    }

    fn catalog_with_agents(stable_ids: &[&str]) -> Arc<BundleCatalog> {
        let bundle = PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/recovery-resolution".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: "test-only".to_string(),
            agents: stable_ids
                .iter()
                .map(|stable_id| PreparedAgent {
                    local_id: (*stable_id).to_string(),
                    stable_id: AgentName::new(*stable_id),
                    description: None,
                    role: AgentRole::Main,
                    color: None,
                    prompt: Some(format!("{stable_id} recovery prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: SpawnLifecycle::Transient,
                    harness_access: HarnessAccess::Full,
                    resource_view: ResourceView::default(),
                    // Deliberately empty: recovery must not depend on can_spawn.
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                })
                .collect(),
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid recovery catalog"))
    }

    #[tokio::test]
    async fn recovered_resident_missing_definition_fails_closed_without_synthesis() {
        let workdir = tempdir();
        let engine = engine_with_catalog(catalog_with_agents(&["build", "general"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("legacy-resident-only");

        let err = match resolve_recovered_resident_agent(&engine, &base, &recorded, &workdir) {
            Ok(_) => panic!("absent recorded id must fail typed definition missing"),
            Err(err) => err,
        };
        assert!(
            matches!(
                &err,
                CoreError::AgentDefinitionMissing { agent_id } if agent_id == "legacy-resident-only"
            ),
            "expected AgentDefinitionMissing for recorded id, got {err}"
        );
        assert!(
            err.to_string().contains("AGENT_DEFINITION_MISSING"),
            "typed surface must remain AGENT_DEFINITION_MISSING, got {err}"
        );
        // No general/base rewrite of the recorded identity.
        assert!(
            !err.to_string().contains("`general`") && !err.to_string().contains("`build`"),
            "error must name the recorded id only, got {err}"
        );
    }

    #[tokio::test]
    async fn recovered_resident_exact_lookup_preserves_recorded_id_without_can_spawn() {
        let workdir = tempdir();
        // Agent present for exact lookup but not in anyone's can_spawn (empty lists).
        let engine = engine_with_catalog(catalog_with_agents(&["build", "resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let resolved = resolve_recovered_resident_agent(&engine, &base, &recorded, &workdir)
            .expect("exact catalog hit must resolve without can_spawn");
        assert_eq!(
            resolved.name.as_str(),
            "resident-helper",
            "recorded AgentName must remain stable"
        );
        assert_eq!(
            resolved.workdir, workdir,
            "session workdir from bind must be preserved"
        );
        assert!(
            resolved
                .system_prompt
                .contains("resident-helper recovery prompt"),
            "exact definition prompt must apply: {}",
            resolved.system_prompt
        );
        assert_ne!(
            resolved.name.as_str(),
            "general",
            "must not fall back to general"
        );
        assert_ne!(
            resolved.name.as_str(),
            "build",
            "must not rewrite to base/lead"
        );
    }

    #[tokio::test]
    async fn recovered_resident_uses_session_workdir_not_base_workdir() {
        let session_dir = tempdir();
        let base_dir = tempdir();
        let engine = engine_with_catalog(catalog_with_agents(&["build", "resident-helper"])).await;
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base-model"),
            system_prompt: "lead base".to_string(),
            workdir: base_dir.clone(),
            reasoning: None,
        };
        let recorded = AgentName::new("resident-helper");

        let resolved =
            resolve_recovered_resident_agent(&engine, &base, &recorded, &session_dir).unwrap();
        assert_eq!(resolved.workdir, session_dir);
        assert_ne!(resolved.workdir, base_dir);
    }

    /// Catalog with one spawnable worker whose Bundle model_policy is explicit.
    fn catalog_with_worker_policy(model_policy: ModelPolicy) -> Arc<BundleCatalog> {
        let agent = |stable_id: &str, role: AgentRole, can_spawn: &[&str], policy: ModelPolicy| {
            PreparedAgent {
                local_id: stable_id.to_string(),
                stable_id: AgentName::new(stable_id),
                description: None,
                role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: policy,
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn.iter().map(|id| AgentName::new(*id)).collect(),
                hook_refs: Vec::new(),
            }
        };
        let bundle = PreparedBundle {
            format_version: 1,
            identity: BundleIdentity {
                id: "hya/spawn-model-precedence".to_string(),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            origin: BundleOrigin::Builtin,
            immutable: true,
            digest: "test-only".to_string(),
            agents: vec![
                agent(
                    "build",
                    AgentRole::Main,
                    &["worker"],
                    ModelPolicy::default(),
                ),
                agent("worker", AgentRole::Subagent, &[], model_policy),
            ],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
        };
        Arc::new(BundleCatalog::from_prepared(&[bundle]).expect("valid precedence catalog"))
    }

    fn precedence_categories() -> CategoryRegistry {
        let mut entries = HashMap::new();
        entries.insert(
            "bundle-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/bundle-model".to_string()]).unwrap(),
        );
        entries.insert(
            "inline-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/inline-model".to_string()]).unwrap(),
        );
        entries.insert(
            "spawn-cat".to_string(),
            CategoryEntry::from_candidates(&["cat/spawn-model".to_string()]).unwrap(),
        );
        CategoryRegistry::from_entries(entries)
    }

    /// Highest-to-lowest spawn model chain, each row selecting the first set layer
    /// while lower layers remain present so the winner is unambiguous.
    #[tokio::test]
    async fn resolve_spawn_member_model_precedence_highest_to_lowest() {
        let workdir = tempdir();
        let categories = precedence_categories();
        let is_servable = |_: &ModelRef| true;
        let allowed = [AgentDef {
            name: "worker".to_string(),
            description: None,
            category: None,
            mode: "subagent".to_string(),
        }];
        let base = AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("base/model"),
            system_prompt: "lead base".to_string(),
            workdir: workdir.clone(),
            reasoning: None,
        };

        #[derive(Clone, Copy)]
        struct Case {
            label: &'static str,
            bundle_model: Option<&'static str>,
            bundle_category: Option<&'static str>,
            inline_model: Option<&'static str>,
            inline_category: Option<&'static str>,
            spawn_model: Option<&'static str>,
            spawn_category: Option<&'static str>,
            expected: &'static str,
        }

        // Cumulative lower layers prove exact order:
        // spawn model > spawn category > inline model > Bundle model >
        // inline category > Bundle category > base model.
        let cases = [
            Case {
                label: "base model",
                bundle_model: None,
                bundle_category: None,
                inline_model: None,
                inline_category: None,
                spawn_model: None,
                spawn_category: None,
                expected: "base/model",
            },
            Case {
                label: "Bundle category",
                bundle_model: None,
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: None,
                spawn_model: None,
                spawn_category: None,
                expected: "cat/bundle-model",
            },
            Case {
                label: "inline category over Bundle category",
                bundle_model: None,
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "cat/inline-model",
            },
            Case {
                label: "Bundle model over category layers",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: None,
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "bundle/model",
            },
            Case {
                label: "inline model over Bundle model",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: None,
                expected: "inline/model",
            },
            Case {
                label: "spawn category over inline model",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: None,
                spawn_category: Some("spawn-cat"),
                expected: "cat/spawn-model",
            },
            Case {
                label: "spawn explicit model highest",
                bundle_model: Some("bundle/model"),
                bundle_category: Some("bundle-cat"),
                inline_model: Some("inline/model"),
                inline_category: Some("inline-cat"),
                spawn_model: Some("spawn/model"),
                spawn_category: Some("spawn-cat"),
                expected: "spawn/model",
            },
        ];

        for case in cases {
            let catalog = catalog_with_worker_policy(ModelPolicy {
                model: case.bundle_model.map(str::to_string),
                category: case.bundle_category.map(str::to_string),
                reasoning: None,
            });
            let engine = engine_with_catalog(catalog).await;
            let binding = engine
                .bind_runtime(&workdir)
                .expect("bind prepared catalog");
            assert!(
                binding.resolve_agent("worker").is_some(),
                "{}: worker must resolve from prepared BundleCatalog",
                case.label
            );

            let has_inline = case.inline_model.is_some() || case.inline_category.is_some();
            let member = SpawnMember {
                description: case.label.to_string(),
                prompt: "resolve model only".to_string(),
                subagent_type: "worker".to_string(),
                model: case.spawn_model.map(str::to_string),
                category: case.spawn_category.map(str::to_string),
                inline_agent: has_inline.then(|| InlineAgent {
                    name: "overlay".to_string(),
                    prompt: "overlay prompt".to_string(),
                    model: case.inline_model.map(str::to_string),
                    category: case.inline_category.map(str::to_string),
                    ..InlineAgent::default()
                }),
                ..SpawnMember::default()
            };

            let resolve_ctx = ResolveSpawnMemberCtx {
                engine: &engine,
                binding: &binding,
                base: &base,
                caller: "build",
                allowed_agents: &allowed,
                categories: &categories,
                is_servable: &is_servable,
                guidance: None,
            };
            let resolved = resolve_spawn_member(&resolve_ctx, member)
                .unwrap_or_else(|err| panic!("{}: resolve failed: {err}", case.label));

            assert_eq!(
                resolved.agent.model.as_str(),
                case.expected,
                "{}: expected model {}",
                case.label,
                case.expected
            );
            assert_eq!(
                resolved.authorized_target.as_str(),
                "worker",
                "{}: authorized target must remain the Bundle agent",
                case.label
            );
        }
    }
}
