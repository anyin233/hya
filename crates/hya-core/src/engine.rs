use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::PreparedAgent;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, OperationId, Projection, SessionId,
    ToolCallId, ToolSchema, now_millis,
};
use hya_provider::{ProviderModel, ProviderRouter, ReasoningEffort};
use hya_store::{ActorClaim, SessionStore};
use hya_tool::{
    AgentDef, FormatterPlane, InteractionPlane, LspPlane, MailboxPlane, PermissionPlane,
    PermissionRules, ResolvedTool, SpawnRequest, SpawnRequestSendError, SpawnRequestSink,
    SpawnerPlane, TodoPlane, ToolError, WebSearchPlane,
};
use serde_json::Value;

#[cfg(test)]
use tokio::sync::Notify;

use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, SummarizeOptions, Summarizer};
use crate::error::CoreError;
use crate::hooks::{HookDispatcher, dispatch_activation_event};
use crate::runtime_registry::CompiledResourceView;
use crate::sidecar::SidecarEnvironment;
use crate::{
    AgentResourcePolicy, RuntimeCandidate, RuntimeRefreshError, RuntimeRegistry, TurnBinding,
};

/// Closed set of fixed Harness system-operation agents.
///
/// Exact catalog lookup only — not spawn, not roster, and not an arbitrary-ID
/// bypass. Callers cannot pass an open string; only these three operations exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FixedSystemAgent {
    Compaction,
    Title,
    Summary,
}

impl FixedSystemAgent {
    /// Stable catalog id for this fixed system operation.
    const fn stable_id(self) -> &'static str {
        match self {
            Self::Compaction => "compaction",
            Self::Title => "title",
            Self::Summary => "summary",
        }
    }
}

mod admission;
mod fork;
mod mailbox;
mod members;
mod session_cleanup;
mod session_state;
mod session_title;
mod shell;
mod stream_round;
mod summary;
mod text_complete;
mod todos;
mod tool_error;
mod turn;

async fn authorize_tool_call(
    resolved: &ResolvedTool,
    input: &Value,
    permission: PermissionPlane,
    message: MessageId,
    call: ToolCallId,
) -> Result<PermissionPlane, ToolError> {
    let invocation = resolved.invocation(input)?;
    permission
        .for_tool_call(message, call)
        .authorize(&invocation)
        .await
        .map_err(ToolError::from)
}

pub use admission::SpawnAdmissionOutcome;

/// Parameters for creating a new session event log.
pub struct CreateSession {
    /// Parent session for subagents; `None` for a root/interactive session.
    pub parent: Option<SessionId>,
    /// Agent name recorded on `SessionCreated`.
    pub agent: AgentName,
    /// Initial model for the session.
    pub model: ModelRef,
    /// Working directory string stored on the session.
    pub workdir: String,
}

/// Turn-time agent identity: name, model, prompt, workdir, and reasoning effort.
#[derive(Clone)]
pub struct AgentSpec {
    /// Agent display / catalog name.
    pub name: AgentName,
    /// Model route for completions.
    pub model: ModelRef,
    /// System prompt base before guidance/skills composition.
    pub system_prompt: String,
    /// Filesystem workdir for tools and path resolution.
    pub workdir: PathBuf,
    /// Optional reasoning effort for capable models.
    pub reasoning: Option<ReasoningEffort>,
}

/// Process-local identity for one admitted member in a parent orchestration turn.
///
/// This value is only an in-process binding between admission and nested spawn
/// observation. It is never persisted, serialized onto the wire, or used as
/// session or event authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdmissionMemberIdentity {
    /// Operation id of the parent tool call that admitted the member.
    pub operation_id: OperationId,
    /// Zero-based ordinal within that admission batch.
    pub member_ordinal: u32,
}

tokio::task_local! {
    static CURRENT_ADMISSION_MEMBER: Option<AdmissionMemberIdentity>;
}

pub(crate) async fn scope_admission_member<F, T>(
    admission: Option<AdmissionMemberIdentity>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    CURRENT_ADMISSION_MEMBER.scope(admission, future).await
}

pub(crate) fn current_admission_member() -> Option<AdmissionMemberIdentity> {
    CURRENT_ADMISSION_MEMBER
        .try_with(|admission| *admission)
        .ok()
        .flatten()
}

#[cfg(test)]
pub(crate) struct DirectMailPreAppendGate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
impl DirectMailPreAppendGate {
    pub(crate) fn new(entered: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self { entered, release }
    }
}

/// Optional app-owned hook to refresh the runtime catalog before a root bind.
///
/// **Contract:** Called from [`SessionEngine::bind_root_runtime`]. Return
/// `Ok(true)` when a new generation was published, `Ok(false)` when nothing
/// changed. Errors abort the bind. Implementors own MCP/plugin discovery I/O;
/// the engine only rebinds after a successful refresh.
#[async_trait]
pub trait RuntimeCatalogRefresh: Send + Sync {
    /// Refresh `runtime` if external sources changed.
    ///
    /// # Errors
    /// Propagate discovery or publication failures as [`CoreError`].
    async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError>;
}

/// One spawn request bound to the immutable runtime snapshot of its parent turn.
///
/// This value is process-local orchestration state. It is never persisted or
/// exposed on the wire; dropping it naturally releases the retained snapshot.
pub struct BoundSpawnRequest {
    binding: TurnBinding,
    request: SpawnRequest,
    admission: Option<AdmissionMemberIdentity>,
}

impl BoundSpawnRequest {
    /// Return the process-local identity of the admitted parent member, if any.
    #[must_use]
    pub fn parent_admission(&self) -> Option<AdmissionMemberIdentity> {
        self.admission
    }

    /// Consume into the retained turn binding and the raw tool-plane spawn request.
    #[must_use]
    pub fn into_parts(self) -> (TurnBinding, SpawnRequest) {
        (self.binding, self.request)
    }
}

/// Core-owned sender for parent-turn-bound spawn requests.
#[derive(Clone)]
pub struct BoundSpawnSender {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
}

impl BoundSpawnSender {
    /// Create a bounded channel pair for the app's spawn worker loop.
    #[must_use]
    pub fn with_capacity(
        capacity: usize,
    ) -> (Self, tokio::sync::mpsc::Receiver<BoundSpawnRequest>) {
        let capacity = capacity.clamp(1, tokio::sync::Semaphore::MAX_PERMITS);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        (Self { tx }, rx)
    }

    fn disconnected() -> Self {
        let (sender, receiver) = Self::with_capacity(1);
        drop(receiver);
        sender
    }

    /// Derive a raw tool-plane facade closed over one already-admitted turn.
    #[must_use]
    pub fn for_binding(&self, binding: &TurnBinding) -> SpawnerPlane {
        SpawnerPlane::from_sink(Arc::new(BoundSpawnRequestSink {
            tx: self.tx.clone(),
            binding: binding.clone(),
        }))
    }
}

struct BoundSpawnRequestSink {
    tx: tokio::sync::mpsc::Sender<BoundSpawnRequest>,
    binding: TurnBinding,
}

impl SpawnRequestSink for BoundSpawnRequestSink {
    fn try_send(&self, request: SpawnRequest) -> Result<(), SpawnRequestSendError> {
        self.tx
            .try_send(BoundSpawnRequest {
                binding: self.binding.clone(),
                request,
                admission: current_admission_member(),
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => SpawnRequestSendError::Full,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => SpawnRequestSendError::Closed,
            })
    }
}

/// Central session runtime: persistence, providers, tools, and turn execution.
///
/// Construct with [`SessionEngine::new`], then chain `with_*` builders for
/// interaction, spawn, mailbox, hooks, compaction, and governors. Turns append
/// events to the store and publish on the bus; observers never write the log.
pub struct SessionEngine {
    store: SessionStore,
    providers: Arc<ProviderRouter>,
    runtime: Arc<RuntimeRegistry>,
    catalog_refresh: Option<Arc<dyn RuntimeCatalogRefresh>>,
    permission: PermissionPlane,
    interaction: InteractionPlane,
    spawner: BoundSpawnSender,
    mailbox: MailboxPlane,
    todo: TodoPlane,
    websearch: WebSearchPlane,
    formatter: FormatterPlane,
    lsp: LspPlane,
    bus: EventBus,
    summarizer: Option<Arc<dyn Summarizer>>,
    compaction: CompactionConfig,
    hooks: Option<Arc<dyn HookDispatcher>>,
    governor: Option<crate::orchestrator::SubagentGovernor>,
    sidecar_environment: Option<Arc<dyn SidecarEnvironment>>,
    #[cfg(test)]
    direct_mail_pre_append_gate: Option<Arc<DirectMailPreAppendGate>>,
}

impl SessionEngine {
    /// Build an engine with disconnected mailbox/spawner and default tool planes.
    ///
    /// Wire product planes with `with_*` before serving interactive traffic.
    #[must_use]
    pub fn new(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        let (interaction, _rx) = InteractionPlane::new();
        let spawner = BoundSpawnSender::disconnected();
        let mailbox = MailboxPlane::disconnected();
        let todo = TodoPlane::default();
        let websearch = WebSearchPlane::default();
        let formatter = FormatterPlane::default();
        let lsp = LspPlane::default();
        Self {
            store,
            providers,
            runtime,
            catalog_refresh: None,
            permission,
            interaction,
            spawner,
            mailbox,
            todo,
            websearch,
            formatter,
            lsp,
            bus,
            summarizer: None,
            compaction: CompactionConfig::default(),
            hooks: None,
            governor: None,
            sidecar_environment: None,
            #[cfg(test)]
            direct_mail_pre_append_gate: None,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_direct_mail_pre_append_gate(
        mut self,
        gate: DirectMailPreAppendGate,
    ) -> Self {
        self.direct_mail_pre_append_gate = Some(Arc::new(gate));
        self
    }

    /// Install plugin/host hooks for command/tool/chat interception and events.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn HookDispatcher>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Install the environment used to start Bundle sidecars for public packages.
    #[must_use]
    pub fn with_sidecar_environment(mut self, environment: Arc<dyn SidecarEnvironment>) -> Self {
        self.sidecar_environment = Some(environment);
        self
    }

    /// Install optional catalog refresh before root runtime binds.
    #[must_use]
    pub fn with_catalog_refresh(mut self, refresh: Arc<dyn RuntimeCatalogRefresh>) -> Self {
        self.catalog_refresh = Some(refresh);
        self
    }

    /// Replace the default disconnected interaction plane.
    #[must_use]
    pub fn with_interaction(mut self, interaction: InteractionPlane) -> Self {
        self.interaction = interaction;
        self
    }

    /// Install the bound spawn sender used by the `task` tool plane.
    #[must_use]
    pub fn with_spawn_sender(mut self, spawner: BoundSpawnSender) -> Self {
        self.spawner = spawner;
        self
    }

    /// Inject the mailbox plane whose service loop this engine drives (see
    /// [`run_mailbox_service`](crate::mailbox::run_mailbox_service)). Wired from
    /// the app layer alongside the spawner, mirroring the established plane
    /// pattern so `hya-tool` stays free of a `hya-core` dependency.
    #[must_use]
    pub fn with_mailbox(mut self, mailbox: MailboxPlane) -> Self {
        self.mailbox = mailbox;
        self
    }

    /// Install the [`SubagentGovernor`] that bounds nested/parallel subagent
    /// streaming concurrency and per-run budget.
    #[must_use]
    pub fn with_governor(mut self, governor: crate::orchestrator::SubagentGovernor) -> Self {
        self.governor = Some(governor);
        self
    }

    /// Borrow the installed subagent governor, if any.
    #[must_use]
    pub fn governor(&self) -> Option<&crate::orchestrator::SubagentGovernor> {
        self.governor.as_ref()
    }

    /// Replace the default disconnected LSP plane.
    #[must_use]
    pub fn with_lsp(mut self, lsp: LspPlane) -> Self {
        self.lsp = lsp;
        self
    }

    /// Replace the default disconnected formatter plane.
    #[must_use]
    pub fn with_formatter(mut self, formatter: FormatterPlane) -> Self {
        self.formatter = formatter;
        self
    }

    /// Replace the default web-search plane configuration.
    #[must_use]
    pub fn with_websearch(mut self, websearch: WebSearchPlane) -> Self {
        self.websearch = websearch;
        self
    }

    /// Enable compaction with a summarizer implementation and thresholds.
    #[must_use]
    pub fn with_compaction(
        mut self,
        summarizer: Arc<dyn Summarizer>,
        config: CompactionConfig,
    ) -> Self {
        self.summarizer = Some(summarizer);
        self.compaction = config;
        self
    }

    /// Live event bus for this engine.
    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Session event store.
    #[must_use]
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    /// LSP plane used by tools and post-edit hooks.
    #[must_use]
    pub fn lsp(&self) -> &LspPlane {
        &self.lsp
    }

    /// Snapshot of resource permission rules currently active on the plane.
    #[must_use]
    pub fn permission_rules(&self) -> PermissionRules {
        self.permission.snapshot_rules()
    }

    /// Formatter plane used after write/edit/patch.
    #[must_use]
    pub fn formatter(&self) -> &FormatterPlane {
        &self.formatter
    }

    /// Provider catalog models exposed to the UI/API.
    #[must_use]
    pub fn provider_catalog(&self) -> Vec<ProviderModel> {
        self.providers.catalog()
    }

    /// Tool schemas from the current effective runtime snapshot.
    #[must_use]
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.runtime.tool_schemas()
    }

    /// Shared runtime registry handle.
    #[must_use]
    pub fn runtime_registry(&self) -> Arc<RuntimeRegistry> {
        self.runtime.clone()
    }

    /// Semantic fingerprint for a bound turn (tools + permissions + sources).
    #[must_use]
    pub fn runtime_semantic_fingerprint_v1(&self, binding: &TurnBinding) -> Option<[u8; 32]> {
        binding.semantic_fingerprint_v1(&self.permission)
    }

    /// Bind a turn against the current registry without refreshing catalogs.
    ///
    /// # Errors
    /// Returns [`CoreError::RuntimeRefresh`] when binding fails.
    pub fn bind_runtime(&self, workdir: &std::path::Path) -> Result<TurnBinding, CoreError> {
        Ok(self.runtime.bind_turn(workdir)?)
    }

    /// Optionally refresh external catalogs, then bind a root turn for `workdir`.
    ///
    /// # Errors
    /// Propagates catalog refresh or bind failures.
    pub async fn bind_root_runtime(
        &self,
        workdir: &std::path::Path,
    ) -> Result<TurnBinding, CoreError> {
        if let Some(refresh) = &self.catalog_refresh {
            let _ = refresh.refresh_if_changed(self.runtime.as_ref()).await?;
        }
        Ok(self.runtime.bind_turn(workdir)?)
    }

    /// Resolve a catalog agent into an [`AgentSpec`] using `binding`.
    ///
    /// # Errors
    /// Returns [`CoreError::AgentDefinitionMissing`] or bundle errors.
    pub fn agent_spec_for_binding(
        &self,
        binding: &TurnBinding,
        base: &AgentSpec,
        stable_id: &str,
    ) -> Result<AgentSpec, CoreError> {
        agent_from_definition(base, stable_id, binding)
    }

    /// Build the caller's authorized spawn roster for tools.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_roster_for_binding(
        &self,
        binding: &TurnBinding,
        caller: &str,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        agent_roster(binding, caller)
    }

    /// Resource/tool policy for `stable_id` under `binding`.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_resource_policy_for_binding(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, CoreError> {
        Ok(binding.agent_resource_policy(stable_id)?)
    }

    /// Publish a new runtime candidate via the registry builder callback.
    ///
    /// # Errors
    /// Returns [`RuntimeRefreshError`] when the candidate is rejected.
    pub fn refresh_runtime(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<hya_proto::ConfigGeneration, RuntimeRefreshError> {
        self.runtime.refresh(build)
    }

    /// Replay the full ordered event log for `session`.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, CoreError> {
        Ok(self.store.replay(session).await?)
    }

    /// Fold the session log into a projection.
    ///
    /// # Errors
    /// Returns store failures as [`CoreError::Store`].
    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, CoreError> {
        Ok(self.store.read_projection(session).await?)
    }

    /// Walk the `SessionCreated{parent}` chain to the top ancestor, returning the
    /// root session and this session's depth (0 = no parent / interactive lead,
    /// 1 = a direct subagent, and so on). Depth is derived from the replayed
    /// projection so there is no separate stored value that can drift. Bounded by a
    /// generous iteration cap as a cycle/runaway guard.
    pub async fn session_lineage(&self, session: SessionId) -> Result<(SessionId, u32), CoreError> {
        let mut current = session;
        let mut depth = 0u32;
        for _ in 0..1024 {
            let projection = self.read_projection(current).await?;
            match projection.session.parent {
                Some(parent) => {
                    current = parent;
                    depth = depth.saturating_add(1);
                }
                None => break,
            }
        }
        Ok((current, depth))
    }

    async fn emit(&self, session: SessionId, event: Event) -> Result<(), CoreError> {
        let seq = self.store.append_event(session, &event).await?;
        self.publish_envelope(Envelope {
            seq,
            ts_millis: now_millis(),
            event,
        });
        Ok(())
    }

    pub(crate) async fn emit_for_actor(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        event: Event,
    ) -> Result<(), CoreError> {
        match actor_claim {
            Some(claim) => {
                self.commit_resident_mutation(claim, session, vec![event])
                    .await
            }
            None => self.emit(session, event).await,
        }
    }

    pub(crate) async fn validate_actor_claim(
        &self,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<(), CoreError> {
        if let Some(claim) = actor_claim {
            self.store.validate_actor_claim(claim).await?;
        }
        Ok(())
    }

    /// Commit resident-owned canonical events only while the supplied actor
    /// capability is current. Publication happens after the SQLite transaction,
    /// so a stale completion cannot advance live observers or replay state.
    pub async fn commit_resident_mutation(
        &self,
        claim: &ActorClaim,
        session: SessionId,
        events: Vec<Event>,
    ) -> Result<(), CoreError> {
        let envelopes = self
            .store
            .commit_resident_mutation(claim, session, &events)
            .await?;
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        Ok(())
    }

    fn publish_live(&self, event: Event) {
        self.publish_envelope(Envelope {
            seq: EventSeq(0),
            ts_millis: now_millis(),
            event,
        });
    }

    fn publish_envelope(&self, envelope: Envelope) {
        if let Some(hooks) = &self.hooks {
            hooks.dispatch_event(&envelope);
        }
        dispatch_activation_event(&envelope);
        self.bus.publish(envelope);
    }

    /// Create a new session id and append `SessionCreated`.
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError> {
        self.create_with_id(None, spec).await
    }

    /// Create a session under a resident actor claim (fenced append).
    ///
    /// # Errors
    /// Returns claim validation or store failures.
    #[doc(hidden)]
    pub async fn create_for_actor(
        &self,
        claim: &ActorClaim,
        spec: CreateSession,
    ) -> Result<SessionId, CoreError> {
        let id = SessionId::new();
        self.commit_resident_mutation(
            claim,
            id,
            vec![Event::SessionCreated {
                session: id,
                parent: spec.parent,
                agent: spec.agent,
                model: spec.model,
                workdir: spec.workdir,
            }],
        )
        .await?;
        Ok(id)
    }

    /// Create or re-open a session with an optional fixed id (idempotent if log non-empty).
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create_with_id(
        &self,
        id: Option<SessionId>,
        spec: CreateSession,
    ) -> Result<SessionId, CoreError> {
        let id = id.unwrap_or_default();
        if !self.replay(id).await?.is_empty() {
            return Ok(id);
        }
        self.emit(
            id,
            Event::SessionCreated {
                session: id,
                parent: spec.parent,
                agent: spec.agent,
                model: spec.model,
                workdir: spec.workdir,
            },
        )
        .await?;
        Ok(id)
    }

    /// Delete a session log from the store.
    ///
    /// # Errors
    /// Returns store failures.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, CoreError> {
        Ok(self.store.delete_session(session).await?)
    }
}

pub(crate) fn effective_agent_for_binding(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    effective_agent_for_binding_with_sidecar_tools(agent, stable_id, binding, guidance, &[])
}

pub(crate) fn effective_agent_for_binding_with_sidecar_tools(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
    sidecar_tools: &[ResolvedTool],
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    // One composition seam: agent_base (Bundle Some replaces / None keeps
    // Harness base) → nonempty guidance → skill prompt material.
    let effective = agent_from_definition(agent, stable_id, binding)?;
    let effective = agent_with_guidance_layer(effective, guidance);
    let policy = binding.agent_resource_policy(stable_id)?;
    let resources = if sidecar_tools.is_empty() {
        binding.compile_agent_resources(&policy)?
    } else {
        binding.compile_agent_resources_with_sidecar_tools(&policy, sidecar_tools)?
    };
    Ok((
        agent_with_bound_skills(effective, resources.as_ref()),
        resources,
    ))
}

fn agent_from_definition(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
) -> Result<AgentSpec, CoreError> {
    let definition =
        binding
            .resolve_agent(stable_id)
            .ok_or_else(|| CoreError::AgentDefinitionMissing {
                agent_id: stable_id.to_string(),
            })?;
    let mut effective = agent.clone();
    effective.name = definition.stable_id.clone();
    effective.workdir = binding.workdir().to_path_buf();
    // Bundle prompt Some replaces only agent_base; None preserves Harness base.
    if let Some(prompt) = definition.prompt.as_ref() {
        effective.system_prompt = prompt.clone();
    }
    if let Some(reasoning) = definition
        .model_policy
        .reasoning
        .as_deref()
        .and_then(hya_provider::ReasoningEffort::parse)
    {
        effective.reasoning = Some(reasoning);
    }
    Ok(effective)
}

/// Append nonempty request-scoped guidance after agent_base resolution.
///
/// Absence or empty text is an empty layer (no error). Callers pre-render once
/// per turn; this does not discover files.
pub(crate) fn agent_with_guidance_layer(mut agent: AgentSpec, guidance: Option<&str>) -> AgentSpec {
    let Some(guidance) = guidance.map(str::trim).filter(|text| !text.is_empty()) else {
        return agent;
    };
    let base = agent.system_prompt.trim_end();
    agent.system_prompt = if base.is_empty() {
        guidance.to_string()
    } else {
        format!("{base}\n\n{guidance}")
    };
    agent
}

/// Exact-lookup a fixed Harness system agent from a captured TurnBinding.
///
/// Accepts only [`FixedSystemAgent`] — callers cannot pass an arbitrary ID.
/// Not agent spawn and not a generic bypass surface.
fn fixed_system_agent(
    binding: &TurnBinding,
    agent: FixedSystemAgent,
) -> Result<&PreparedAgent, CoreError> {
    let stable_id = agent.stable_id();
    binding
        .resolve_agent(stable_id)
        .ok_or_else(|| CoreError::AgentDefinitionMissing {
            agent_id: stable_id.to_string(),
        })
}

/// Build summarize options from a fixed system definition.
///
/// Prepared prompt and explicit reasoning apply when present. Absent Bundle
/// model leaves `model` unset so the caller/summarizer fallback is preserved.
pub(crate) fn summarize_options_from_definition(definition: &PreparedAgent) -> SummarizeOptions {
    SummarizeOptions {
        system: definition.prompt.clone(),
        model: definition.model_policy.model.as_deref().map(ModelRef::new),
        reasoning: definition
            .model_policy
            .reasoning
            .as_deref()
            .and_then(ReasoningEffort::parse),
    }
}

pub(crate) fn projection_workdir(projection: &Projection) -> Option<PathBuf> {
    projection.session.workdir.as_ref().map(PathBuf::from)
}

pub(crate) fn agent_with_bound_skills(
    mut effective: AgentSpec,
    resources: &CompiledResourceView,
) -> AgentSpec {
    if let Some(section) = resources.skills_prompt_section() {
        let prompt = effective.system_prompt.trim_end();
        effective.system_prompt = if prompt.is_empty() {
            section
        } else {
            format!("{prompt}\n\n{section}")
        };
    }
    effective
}

fn agent_roster(binding: &TurnBinding, caller: &str) -> Result<Arc<[AgentDef]>, CoreError> {
    Ok(binding
        .spawnable_agents(caller)?
        .into_iter()
        .map(agent_definition)
        .collect::<Vec<_>>()
        .into())
}

fn agent_definition(agent: &PreparedAgent) -> AgentDef {
    AgentDef {
        name: agent.stable_id.as_str().to_string(),
        description: agent.description.clone(),
        category: agent.model_policy.category.clone(),
        mode: agent.role.selector_mode().to_string(),
    }
}

pub(crate) fn session_workdir(agent: &AgentSpec, projection: &Projection) -> PathBuf {
    projection
        .session
        .workdir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| agent.workdir.clone())
}
