//! Deterministic scripted provider for tests and process E2E matrices.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::stream;
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartId, Role, SessionId, TokenUsage, ToolCallId,
    ToolName,
};

use crate::{Capabilities, CompletionRequest, EventStream, Provider, ProviderError};

/// One step in a scripted assistant turn materialised as canonical events.
#[derive(Clone, Debug)]
pub enum FakeStep {
    /// Emit a complete text part with this body.
    Text(String),
    /// Emit a complete reasoning part with this body.
    Reasoning(String),
    /// Emit a tool-call request with the given name and JSON input.
    ToolCall {
        /// Tool name.
        name: String,
        /// Tool arguments object.
        input: serde_json::Value,
    },
    /// Attach usage before finish (carried into `MessageFinished`).
    Usage(TokenUsage),
    /// End the turn with this finish reason.
    Finish(FinishReason),
}

/// Provider that plays back one or more [`FakeStep`] scripts per `stream()` call.
pub struct FakeProvider {
    id: String,
    scripts: Vec<Vec<FakeStep>>,
    turn: AtomicUsize,
}

impl FakeProvider {
    /// Single-turn script reused until turns are exhausted (then bare `Finish(Stop)`).
    #[must_use]
    pub fn scripted(script: Vec<FakeStep>) -> Self {
        Self::scripted_turns(vec![script])
    }

    /// One script per assistant turn; each `stream()` call advances to the next.
    /// Calls past the last script yield a bare `Finish(Stop)` so an agent loop
    /// terminates instead of replaying a tool call forever.
    #[must_use]
    pub fn scripted_turns(scripts: Vec<Vec<FakeStep>>) -> Self {
        Self {
            id: "fake".to_string(),
            scripts,
            turn: AtomicUsize::new(0),
        }
    }

    /// Expand a script into the event sequence a live stream would emit for this turn.
    #[must_use]
    pub fn materialize(script: &[FakeStep], session: SessionId, message: MessageId) -> Vec<Event> {
        let mut out = Vec::new();
        let mut tokens = None;
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
                        reason: None,
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
                        provider_data: None,
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
                        name: tool.clone(),
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
                FakeStep::Usage(usage) => merge_tokens(&mut tokens, *usage),
                FakeStep::Finish(reason) => out.push(Event::MessageFinished {
                    session,
                    message,
                    role: Role::Assistant,
                    finish: *reason,
                    tokens,
                }),
            }
        }
        out
    }
}

fn merge_tokens(target: &mut Option<TokenUsage>, update: TokenUsage) {
    target.get_or_insert_with(TokenUsage::default).merge(update);
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
        let idx = self.turn.fetch_add(1, Ordering::Relaxed);
        let stop = [FakeStep::Finish(FinishReason::Stop)];
        let script = self.scripts.get(idx).map_or(&stop[..], Vec::as_slice);
        let events = Self::materialize(script, session, message);
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}
