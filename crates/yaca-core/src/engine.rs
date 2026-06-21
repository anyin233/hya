use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use yaca_proto::{
    AgentName, Envelope, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId,
    PartProjection, Projection, Role, SessionId, now_millis,
};
use yaca_provider::{CompletionRequest, ProviderRouter, ReasoningEffort};
use yaca_store::SessionStore;
use yaca_tool::{
    InteractionPlane, PermissionPlane, SpawnerPlane, ToolCtx, ToolError, ToolRegistry,
};

use crate::bus::EventBus;
use crate::compaction::{CompactionConfig, Summarizer, compact_with};
use crate::error::CoreError;

const COMPACT_CONTEXT_MARKER: &str = "YACA_COMPACTED_CONTEXT";

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
    bus: EventBus,
    summarizer: Option<Arc<dyn Summarizer>>,
    compaction: CompactionConfig,
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
        Self {
            store,
            providers,
            tools,
            permission,
            interaction,
            spawner,
            bus,
            summarizer: None,
            compaction: CompactionConfig::default(),
        }
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

    pub async fn inject_system_message(
        &self,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::System,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextStart {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextDelta {
                session,
                message,
                part,
                delta: content,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextEnd {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                finish: FinishReason::Stop,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn compact_context(
        &self,
        session: SessionId,
        summary: String,
    ) -> Result<MessageId, CoreError> {
        self.inject_system_message(session, format!("{COMPACT_CONTEXT_MARKER}\n{summary}"))
            .await
    }

    async fn emit(&self, session: SessionId, event: Event) -> Result<(), CoreError> {
        let seq = self.store.append_event(session, &event).await?;
        self.bus.publish(Envelope {
            seq,
            ts_millis: now_millis(),
            event,
        });
        Ok(())
    }

    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError> {
        let id = SessionId::new();
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

    pub async fn admit_user_prompt(
        &self,
        session: SessionId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextStart {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextDelta {
                session,
                message,
                part,
                delta: text,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextEnd {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                finish: FinishReason::Stop,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn run_turn(
        &self,
        session: SessionId,
        agent: &AgentSpec,
        cancel: CancellationToken,
    ) -> Result<FinishReason, CoreError> {
        let message = MessageId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
            },
        )
        .await?;

        const MAX_TOOL_ROUNDS: u32 = 25;
        let mut rounds: u32 = 0;
        loop {
            if cancel.is_cancelled() {
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        finish: FinishReason::Cancelled,
                    },
                )
                .await?;
                return Ok(FinishReason::Cancelled);
            }

            let projection = self.store.read_projection(session).await?;
            let messages = projection_to_messages(agent, &projection);
            let messages = if let Some(summarizer) = &self.summarizer {
                match compact_with(messages, &self.compaction, summarizer.as_ref()).await {
                    Ok(compacted) => compacted,
                    Err(_) => projection_to_messages(agent, &projection),
                }
            } else {
                messages
            };
            let request = request_from_messages(agent, messages, &self.tools);
            let mut stream = self.providers.stream(request, session, message).await?;

            let mut tool_calls: Vec<ToolCallReq> = Vec::new();
            let mut finish = FinishReason::Stop;
            while let Some(item) = stream.next().await {
                let event = item?;
                if let Event::ToolCallRequested {
                    part,
                    call,
                    name,
                    input,
                    ..
                } = &event
                {
                    tool_calls.push(ToolCallReq {
                        part: *part,
                        call: *call,
                        name: name.to_string(),
                        input: input.clone(),
                    });
                }
                if let Event::MessageFinished { finish: f, .. } = &event {
                    finish = *f;
                    continue;
                }
                self.emit(session, event).await?;
            }

            if tool_calls.is_empty() {
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        finish,
                    },
                )
                .await?;
                return Ok(finish);
            }

            for tc in tool_calls {
                let started = std::time::Instant::now();
                let result = match self.tools.get(&tc.name) {
                    Some(tool) => {
                        let ctx = ToolCtx {
                            permission: self.permission.for_session(session),
                            interaction: self.interaction.for_session(session),
                            spawner: self.spawner.for_session(session),
                            parent_session: projection.session.parent,
                            workdir: agent.workdir.clone(),
                            cancel: cancel.clone(),
                        };
                        tool.execute(&ctx, tc.input).await
                    }
                    None => Err(ToolError::Other(format!("unknown tool: {}", tc.name))),
                };
                let time_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let event = match result {
                    Ok(output) => Event::ToolResult {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        output,
                        time_ms,
                    },
                    Err(e) => Event::ToolError {
                        session,
                        message,
                        part: tc.part,
                        call: tc.call,
                        message_text: e.to_string(),
                    },
                };
                self.emit(session, event).await?;
            }

            rounds += 1;
            if rounds >= MAX_TOOL_ROUNDS {
                let part = PartId::new();
                self.emit(
                    session,
                    Event::TextStart {
                        session,
                        message,
                        part,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::TextDelta {
                        session,
                        message,
                        part,
                        delta: format!("[stopped: reached the {MAX_TOOL_ROUNDS}-tool-call limit]"),
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::TextEnd {
                        session,
                        message,
                        part,
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::MessageFinished {
                        session,
                        message,
                        finish: FinishReason::Error,
                    },
                )
                .await?;
                return Ok(FinishReason::Error);
            }
        }
    }
}

struct ToolCallReq {
    part: PartId,
    call: yaca_proto::ToolCallId,
    name: String,
    input: serde_json::Value,
}

fn collect_text(parts: &[PartProjection]) -> String {
    let mut s = String::new();
    for p in parts {
        if let PartProjection::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

fn map_parts(parts: &[PartProjection]) -> Vec<Part> {
    parts
        .iter()
        .filter_map(|p| match p {
            PartProjection::Text { id, text } => Some(Part::Text {
                id: *id,
                text: text.clone(),
            }),
            PartProjection::Tool {
                id,
                call,
                name,
                state,
            } => Some(Part::Tool {
                id: *id,
                call_id: *call,
                name: name.clone(),
                state: state.clone(),
            }),
            PartProjection::Reasoning { .. } => None,
        })
        .collect()
}

fn projection_to_messages(agent: &AgentSpec, projection: &Projection) -> Vec<Message> {
    compacted_messages(projection)
        .filter(|m| !(m.role == Role::Assistant && m.parts.is_empty()))
        .map(|m| match m.role {
            Role::User => Message::User {
                id: m.id,
                parts: map_parts(&m.parts),
            },
            Role::Assistant => Message::Assistant {
                id: m.id,
                agent: agent.name.clone(),
                model: agent.model.clone(),
                parts: map_parts(&m.parts),
                finish: m.finish,
                tokens: None,
            },
            Role::System => Message::System {
                id: m.id,
                content: collect_text(&m.parts),
            },
        })
        .collect()
}

fn request_from_messages(
    agent: &AgentSpec,
    messages: Vec<Message>,
    tools: &ToolRegistry,
) -> CompletionRequest {
    CompletionRequest {
        model: agent.model.clone(),
        system: Some(agent.system_prompt.clone()),
        messages,
        tools: tools.schemas(),
        temperature: None,
        max_output_tokens: None,
        reasoning: agent.reasoning,
    }
}

fn compacted_messages(
    projection: &Projection,
) -> impl Iterator<Item = &yaca_proto::MessageProjection> {
    let start = projection
        .session
        .messages
        .iter()
        .rposition(|message| {
            message.role == Role::System
                && collect_text(&message.parts).starts_with(COMPACT_CONTEXT_MARKER)
        })
        .unwrap_or(0);
    projection.session.messages[start..].iter()
}
