use async_trait::async_trait;
use futures::stream;
use yaca_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, SessionId, ToolCallId, ToolName,
};

use crate::{Capabilities, CompletionRequest, EventStream, Provider, ProviderError};

#[derive(Clone, Debug)]
pub enum FakeStep {
    Text(String),
    Reasoning(String),
    ToolCall {
        name: String,
        input: serde_json::Value,
    },
    Finish(FinishReason),
}

pub struct FakeProvider {
    id: String,
    script: Vec<FakeStep>,
}

impl FakeProvider {
    #[must_use]
    pub fn scripted(script: Vec<FakeStep>) -> Self {
        Self {
            id: "fake".to_string(),
            script,
        }
    }

    #[must_use]
    pub fn materialize(script: &[FakeStep], session: SessionId, message: MessageId) -> Vec<Event> {
        let mut out = Vec::new();
        for step in script {
            match step {
                FakeStep::Text(t) => {
                    let part = PartId::new();
                    out.push(Event::TextStart {
                        session,
                        message,
                        part,
                    });
                    out.push(Event::TextDelta {
                        session,
                        message,
                        part,
                        delta: t.clone(),
                    });
                    out.push(Event::TextEnd {
                        session,
                        message,
                        part,
                    });
                }
                FakeStep::Reasoning(t) => {
                    let part = PartId::new();
                    out.push(Event::ReasoningStart {
                        session,
                        message,
                        part,
                    });
                    out.push(Event::ReasoningDelta {
                        session,
                        message,
                        part,
                        delta: t.clone(),
                    });
                    out.push(Event::ReasoningEnd {
                        session,
                        message,
                        part,
                    });
                }
                FakeStep::ToolCall { name, input } => {
                    let part = PartId::new();
                    let call = ToolCallId::new();
                    let tool = ToolName::new(name.clone());
                    out.push(Event::ToolInputStart {
                        session,
                        message,
                        part,
                        call,
                        name: tool.clone(),
                    });
                    out.push(Event::ToolInputDelta {
                        session,
                        message,
                        part,
                        call,
                        delta: input.to_string(),
                    });
                    out.push(Event::ToolCallRequested {
                        session,
                        message,
                        part,
                        call,
                        name: tool,
                        input: input.clone(),
                    });
                }
                FakeStep::Finish(reason) => out.push(Event::MessageFinished {
                    session,
                    message,
                    finish: *reason,
                }),
            }
        }
        out
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            parallel_tool_calls: true,
            usage_reporting: true,
            max_context: 200_000,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let events = Self::materialize(&self.script, session, message);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}
