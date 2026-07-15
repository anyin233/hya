use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{
    AgentName, Envelope, Event, EventSeq, MessageId, ModelRef, Projection, SessionId, ToolCallId,
    ToolSchema, now_millis,
};
use hya_provider::{ProviderModel, ProviderRouter, ReasoningEffort};
use hya_store::SessionStore;
use hya_tool::{
    AgentCatalogPlane, FormatterPlane, InteractionPlane, LspPlane, MailboxPlane, PermissionPlane,
    PermissionRules, ResolvedTool, SkillPlane, SpawnerPlane, TodoPlane, ToolError, ToolRegistry,
    WebSearchPlane, discover_skills, skills_section,
};
use serde_json::Value;

use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, Summarizer};
use crate::error::CoreError;
use crate::hooks::HookDispatcher;

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
    tools: Arc<ToolRegistry>,
    permission: PermissionPlane,
    interaction: InteractionPlane,
    spawner: SpawnerPlane,
    mailbox: MailboxPlane,
    todo: TodoPlane,
    skills: SkillPlane,
    agents: AgentCatalogPlane,
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
        tools: Arc<ToolRegistry>,
        permission: PermissionPlane,
        bus: EventBus,
    ) -> Self {
        let (interaction, _rx) = InteractionPlane::new();
        let (spawner, _srx) = SpawnerPlane::new();
        let mailbox = MailboxPlane::disconnected();
        let todo = TodoPlane::default();
        let skills = SkillPlane::default();
        let agents = AgentCatalogPlane::default();
        let websearch = WebSearchPlane::default();
        let formatter = FormatterPlane::default();
        let lsp = LspPlane::default();
        Self {
            store,
            providers,
            tools,
            permission,
            interaction,
            spawner,
            mailbox,
            todo,
            skills,
            agents,
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

    /// Inject the agent catalog resolver used by the model-facing `list_agents`
    /// tool. Wired from the app layer (which owns the `hya-server` catalog) to
    /// avoid a `hya-tool → hya-server` circular dependency.
    #[must_use]
    pub fn with_agents(mut self, agents: AgentCatalogPlane) -> Self {
        self.agents = agents;
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
        self.tools.schemas()
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

pub(crate) fn effective_agent_for_projection(
    agent: &AgentSpec,
    projection: &Projection,
) -> AgentSpec {
    let mut effective = agent.clone();
    effective.workdir = session_workdir(agent, projection);
    if let Some(section) = skills_section(&discover_skills(&effective.workdir)) {
        let prompt = effective.system_prompt.trim_end();
        effective.system_prompt = if prompt.is_empty() {
            section
        } else {
            format!("{prompt}\n\n{section}")
        };
    }
    effective
}

pub(crate) fn session_workdir(agent: &AgentSpec, projection: &Projection) -> PathBuf {
    projection
        .session
        .workdir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| agent.workdir.clone())
}
