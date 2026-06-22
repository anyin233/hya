use std::path::PathBuf;
use std::sync::Arc;

use yaca_proto::{
    AgentName, Envelope, Event, MessageId, ModelRef, PartId, Projection, SessionId, ToolPartState,
    now_millis,
};
use yaca_provider::{ProviderRouter, ReasoningEffort};
use yaca_store::SessionStore;
use yaca_tool::{
    InteractionPlane, LspPlane, PermissionPlane, SkillPlane, SpawnerPlane, TodoItem, TodoPlane,
    ToolRegistry, WebSearchPlane,
};

use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, Summarizer};
use crate::error::CoreError;
use crate::hooks::HookDispatcher;

mod admission;
mod shell;
mod stream_round;
mod text_complete;
mod turn;

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
    todo: TodoPlane,
    skills: SkillPlane,
    websearch: WebSearchPlane,
    lsp: LspPlane,
    bus: EventBus,
    summarizer: Option<Arc<dyn Summarizer>>,
    compaction: CompactionConfig,
    hooks: Option<Arc<dyn HookDispatcher>>,
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
        let todo = TodoPlane::default();
        let skills = SkillPlane::default();
        let websearch = WebSearchPlane::default();
        let lsp = LspPlane::default();
        Self {
            store,
            providers,
            tools,
            permission,
            interaction,
            spawner,
            todo,
            skills,
            websearch,
            lsp,
            bus,
            summarizer: None,
            compaction: CompactionConfig::default(),
            hooks: None,
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

    #[must_use]
    pub fn with_lsp(mut self, lsp: LspPlane) -> Self {
        self.lsp = lsp;
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

    pub async fn replay(&self, session: SessionId) -> Result<Vec<Envelope>, CoreError> {
        Ok(self.store.replay(session).await?)
    }

    pub async fn read_projection(&self, session: SessionId) -> Result<Projection, CoreError> {
        Ok(self.store.read_projection(session).await?)
    }

    pub async fn todos(&self, session: SessionId) -> Vec<TodoItem> {
        self.todo.get(session).await
    }

    async fn emit(&self, session: SessionId, event: Event) -> Result<(), CoreError> {
        let seq = self.store.append_event(session, &event).await?;
        let envelope = Envelope {
            seq,
            ts_millis: now_millis(),
            event,
        };
        if let Some(hooks) = &self.hooks {
            hooks.dispatch_event(&envelope);
        }
        self.bus.publish(envelope);
        Ok(())
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

    pub async fn switch_agent(
        &self,
        session: SessionId,
        agent: AgentName,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::AgentSwitched { session, agent })
            .await
    }

    pub async fn switch_model(&self, session: SessionId, model: ModelRef) -> Result<(), CoreError> {
        self.emit(session, Event::ModelSwitched { session, model })
            .await
    }

    pub async fn set_title(&self, session: SessionId, title: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionTitled { session, title })
            .await
    }

    pub async fn set_metadata(
        &self,
        session: SessionId,
        metadata: serde_json::Value,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::SessionMetadataSet { session, metadata })
            .await
    }

    pub async fn set_permission(
        &self,
        session: SessionId,
        permission: Vec<serde_json::Value>,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::SessionPermissionSet {
                session,
                permission,
            },
        )
        .await
    }

    pub async fn set_archived(
        &self,
        session: SessionId,
        archived: serde_json::Number,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::SessionArchived { session, archived })
            .await
    }

    pub async fn delete_message(
        &self,
        session: SessionId,
        message: MessageId,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::MessageDeleted { session, message })
            .await
    }

    pub async fn delete_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::PartDeleted {
                session,
                message,
                part,
            },
        )
        .await
    }

    pub async fn replace_text_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::TextReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    pub async fn replace_reasoning_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ReasoningReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    pub async fn update_tool_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        state: ToolPartState,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ToolPartUpdated {
                session,
                message,
                part,
                state,
            },
        )
        .await
    }
}
