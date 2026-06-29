//! Shared idempotent reducer: fold an event log into a session view. Used by the
//! store (read path) and the client (SSE reconnect); idempotent by `EventSeq` so
//! re-delivered events are no-ops.

mod helpers;

use serde::{Deserialize, Serialize};

use self::helpers::{find_part, push_part, tool_input, upsert_tool};
use crate::event::{Envelope, Event};
use crate::ids::{MessageId, PartId, SessionId, ToolCallId};
use crate::message::{FinishReason, Role, TokenUsage, ToolPartState};
use crate::model::{AgentName, ModelRef, ToolName};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionProjection {
    pub id: Option<SessionId>,
    pub parent: Option<SessionId>,
    pub agent: Option<AgentName>,
    pub model: Option<ModelRef>,
    pub workdir: Option<String>,
    pub title: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub permission: Option<Vec<serde_json::Value>>,
    pub archived: Option<serde_json::Number>,
    pub share: Option<String>,
    pub messages: Vec<MessageProjection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageProjection {
    pub id: MessageId,
    pub role: Role,
    pub finish: Option<FinishReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<serde_json::Value>,
    pub parts: Vec<PartProjection>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PartProjection {
    Text {
        id: PartId,
        text: String,
    },
    Reasoning {
        id: PartId,
        text: String,
    },
    Tool {
        id: PartId,
        call: ToolCallId,
        name: ToolName,
        state: ToolPartState,
    },
}

impl PartProjection {
    #[must_use]
    pub fn id(&self) -> PartId {
        match self {
            PartProjection::Text { id, .. }
            | PartProjection::Reasoning { id, .. }
            | PartProjection::Tool { id, .. } => *id,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub session: SessionProjection,
    pub last_seq: u64,
}

impl Projection {
    #[must_use]
    pub fn from_events(envs: &[Envelope]) -> Self {
        let mut p = Self::default();
        for e in envs {
            p.apply(e);
        }
        p
    }

    pub fn apply(&mut self, env: &Envelope) {
        if env.seq.0 <= self.last_seq {
            return;
        }
        self.apply_event(&env.event);
        self.last_seq = env.seq.0;
    }

    fn message_mut(&mut self, id: MessageId) -> Option<&mut MessageProjection> {
        self.session.messages.iter_mut().find(|m| m.id == id)
    }

    fn apply_event(&mut self, e: &Event) {
        match e {
            Event::SessionCreated {
                session,
                parent,
                agent,
                model,
                workdir,
                ..
            } => {
                self.session.id = Some(*session);
                self.session.parent = *parent;
                self.session.agent = Some(agent.clone());
                self.session.model = Some(model.clone());
                self.session.workdir = Some(workdir.clone());
            }
            Event::SessionMoved { workdir, .. } => {
                self.session.workdir = Some(workdir.clone());
            }
            Event::SessionTitled { title, .. } => {
                self.session.title = Some(title.clone());
            }
            Event::SessionMetadataSet { metadata, .. } => {
                self.session.metadata = Some(metadata.clone());
            }
            Event::SessionPermissionSet { permission, .. } => {
                self.session.permission = Some(permission.clone());
            }
            Event::SessionArchived { archived, .. } => {
                self.session.archived = Some(archived.clone());
            }
            Event::SessionShareSet { url, .. } => {
                self.session.share = Some(url.clone());
            }
            Event::SessionShareCleared { .. } => {
                self.session.share = None;
            }
            Event::AgentSwitched { agent, .. } => {
                self.session.agent = Some(agent.clone());
            }
            Event::ModelSwitched { model, .. } => {
                self.session.model = Some(model.clone());
            }
            Event::MessageStarted { message, role, .. } => {
                if self.message_mut(*message).is_none() {
                    self.session.messages.push(MessageProjection {
                        id: *message,
                        role: *role,
                        finish: None,
                        tokens: None,
                        files: Vec::new(),
                        agents: Vec::new(),
                        parts: Vec::new(),
                    });
                }
            }
            Event::UserPromptContextRecorded {
                message,
                files,
                agents,
                ..
            } => {
                if let Some(message) = self.message_mut(*message) {
                    message.files = files.clone();
                    message.agents = agents.clone();
                }
            }
            Event::MessageFinished {
                message,
                finish,
                tokens,
                ..
            } => {
                if let Some(m) = self.message_mut(*message) {
                    m.finish = Some(*finish);
                    m.tokens = *tokens;
                }
            }
            Event::MessageDeleted { message, .. } => {
                self.session.messages.retain(|item| item.id != *message);
            }
            Event::PartDeleted { message, part, .. } => {
                if let Some(message) = self.message_mut(*message) {
                    message.parts.retain(|item| item.id() != *part);
                }
            }
            Event::TextStart { message, part, .. } => push_part(
                self,
                *message,
                PartProjection::Text {
                    id: *part,
                    text: String::new(),
                },
            ),
            Event::TextDelta {
                message,
                part,
                delta,
                ..
            } => {
                if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
                    text.push_str(delta);
                }
            }
            Event::TextReplace {
                message,
                part,
                text: replacement,
                ..
            } => {
                if let Some(PartProjection::Text { text, .. }) = find_part(self, *message, *part) {
                    *text = replacement.clone();
                }
            }
            Event::ReasoningStart { message, part, .. } => push_part(
                self,
                *message,
                PartProjection::Reasoning {
                    id: *part,
                    text: String::new(),
                },
            ),
            Event::ReasoningDelta {
                message,
                part,
                delta,
                ..
            } => {
                if let Some(PartProjection::Reasoning { text, .. }) =
                    find_part(self, *message, *part)
                {
                    text.push_str(delta);
                }
            }
            Event::ReasoningReplace {
                message,
                part,
                text: replacement,
                ..
            } => {
                if let Some(PartProjection::Reasoning { text, .. }) =
                    find_part(self, *message, *part)
                {
                    *text = replacement.clone();
                }
            }
            Event::ToolInputStart {
                message,
                part,
                call,
                name,
                ..
            } => push_part(
                self,
                *message,
                PartProjection::Tool {
                    id: *part,
                    call: *call,
                    name: name.clone(),
                    state: ToolPartState::Pending {
                        input: serde_json::Value::Null,
                    },
                },
            ),
            Event::ToolCallRequested {
                message,
                part,
                call,
                name,
                input,
                ..
            } => upsert_tool(
                self,
                *message,
                *part,
                *call,
                name.clone(),
                ToolPartState::Running {
                    input: input.clone(),
                },
            ),
            Event::ToolResult {
                message,
                part,
                output,
                time_ms,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    let input = tool_input(state);
                    *state = ToolPartState::Completed {
                        input,
                        output: output.clone(),
                        time_ms: *time_ms,
                    };
                }
            }
            Event::ToolError {
                message,
                part,
                message_text,
                value,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    let input = tool_input(state);
                    *state = ToolPartState::Error {
                        input,
                        message: message_text.clone(),
                        value: value.clone(),
                    };
                }
            }
            Event::ToolPartUpdated {
                message,
                part,
                state: next,
                ..
            } => {
                if let Some(PartProjection::Tool { state, .. }) = find_part(self, *message, *part) {
                    *state = next.clone();
                }
            }
            Event::TextEnd { .. }
            | Event::ReasoningEnd { .. }
            | Event::SessionStatus { .. }
            | Event::ToolInputDelta { .. }
            | Event::CommandExecuted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::Error { .. } => {}
        }
    }
}
