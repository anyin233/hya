use std::path::PathBuf;
use std::sync::Arc;

use hya_bundle::PreparedAgent;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, Projection, SessionId, ToolCallId,
    ToolSchema, now_millis,
};
use hya_provider::{ProviderModel, ProviderRouter, ReasoningEffort};
use hya_store::{ActorClaim, SessionStore};
use hya_tool::{
    AgentDef, FormatterPlane, InteractionPlane, LspPlane, MailboxPlane, PermissionPlane,
    PermissionRules, ResolvedTool, SpawnerPlane, TodoPlane, ToolError, WebSearchPlane,
};
use serde_json::Value;

use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, SummarizeOptions, Summarizer};
use crate::error::CoreError;
use crate::hooks::HookDispatcher;
use crate::runtime_registry::CompiledResourceView;
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

pub struct CreateSession {
    pub parent: Option<SessionId>,
    pub agent: AgentName,
    pub model: ModelRef,
    pub workdir: String,
}

#[derive(Clone)]
pub struct AgentSpec {
    pub name: AgentName,
    pub model: ModelRef,
    pub system_prompt: String,
    pub workdir: PathBuf,
    pub reasoning: Option<ReasoningEffort>,
}

pub struct SessionEngine {
    store: SessionStore,
    providers: Arc<ProviderRouter>,
    runtime: Arc<RuntimeRegistry>,
    permission: PermissionPlane,
    interaction: InteractionPlane,
    spawner: SpawnerPlane,
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
}

impl SessionEngine {
    #[must_use]
    pub fn new(
        store: SessionStore,
        providers: Arc<ProviderRouter>,
        runtime: Arc<RuntimeRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        let (interaction, _rx) = InteractionPlane::new();
        let (spawner, _srx) = SpawnerPlane::new();
        let mailbox = MailboxPlane::disconnected();
        let todo = TodoPlane::default();
        let websearch = WebSearchPlane::default();
        let formatter = FormatterPlane::default();
        let lsp = LspPlane::default();
        Self {
            store,
            providers,
            runtime,
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
        }
    }

    #[must_use]
    pub fn with_hooks(mut self, hooks: Arc<dyn HookDispatcher>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    #[must_use]
    pub fn with_interaction(mut self, interaction: InteractionPlane) -> Self {
        self.interaction = interaction;
        self
    }

    #[must_use]
    pub fn with_spawner(mut self, spawner: SpawnerPlane) -> Self {
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

    #[must_use]
    pub fn governor(&self) -> Option<&crate::orchestrator::SubagentGovernor> {
        self.governor.as_ref()
    }

    #[must_use]
    pub fn with_lsp(mut self, lsp: LspPlane) -> Self {
        self.lsp = lsp;
        self
    }

    #[must_use]
    pub fn with_formatter(mut self, formatter: FormatterPlane) -> Self {
        self.formatter = formatter;
        self
    }

    #[must_use]
    pub fn with_websearch(mut self, websearch: WebSearchPlane) -> Self {
        self.websearch = websearch;
        self
    }

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

    #[must_use]
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    #[must_use]
    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    #[must_use]
    pub fn lsp(&self) -> &LspPlane {
        &self.lsp
    }

    #[must_use]
    pub fn permission_rules(&self) -> PermissionRules {
        self.permission.snapshot_rules()
    }

    #[must_use]
    pub fn formatter(&self) -> &FormatterPlane {
        &self.formatter
    }

    #[must_use]
    pub fn provider_catalog(&self) -> Vec<ProviderModel> {
        self.providers.catalog()
    }

    #[must_use]
    pub fn tool_schemas(&self) -> Vec<ToolSchema> {
        self.runtime.tool_schemas()
    }

    #[must_use]
    pub fn runtime_registry(&self) -> Arc<RuntimeRegistry> {
        self.runtime.clone()
    }

    pub fn bind_runtime(&self, workdir: &std::path::Path) -> Result<TurnBinding, CoreError> {
        Ok(self.runtime.bind_turn(workdir)?)
    }

    pub fn agent_spec_for_binding(
        &self,
        binding: &TurnBinding,
        base: &AgentSpec,
        stable_id: &str,
    ) -> Result<AgentSpec, CoreError> {
        agent_from_definition(base, stable_id, binding)
    }

    pub fn agent_roster_for_binding(
        &self,
        binding: &TurnBinding,
        caller: &str,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        agent_roster(binding, caller)
    }

    pub fn agent_resource_policy_for_binding(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, CoreError> {
        Ok(binding.agent_resource_policy(stable_id)?)
    }

    pub fn refresh_runtime(
        &self,
        build: impl FnOnce(&mut RuntimeCandidate) -> Result<(), RuntimeRefreshError>,
    ) -> Result<hya_proto::ConfigGeneration, RuntimeRefreshError> {
        self.runtime.refresh(build)
    }

    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, CoreError> {
        Ok(self.store.replay(session).await?)
    }

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
        self.bus.publish(envelope);
    }

    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError> {
        self.create_with_id(None, spec).await
    }

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
    // One composition seam: agent_base (Bundle Some replaces / None keeps
    // Harness base) → nonempty guidance → skill prompt material.
    let effective = agent_from_definition(agent, stable_id, binding)?;
    let effective = agent_with_guidance_layer(effective, guidance);
    let policy = binding.agent_resource_policy(stable_id)?;
    let resources = binding.compile_agent_resources(&policy)?;
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
