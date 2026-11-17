//! One-shot model probe: the "does this model answer?" check behind the
//! provider view's test action (`POST /v1/providers/{provider_id}/test`).

use futures::StreamExt as _;
use hya_proto::{Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, SessionId};
use hya_provider::{CompletionRequest, ProviderError};

use super::SessionEngine;

/// Text the probe sends as its single user message.
pub const MODEL_PROBE_PROMPT: &str = "hi";

/// Reply collected from a successful probe stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelProbeReply {
    /// Concatenated text deltas (often empty or one token).
    pub text: String,
    /// Finish reason the provider reported, when it reported one. A
    /// `length` finish is a normal reply: the probe caps output tokens.
    pub finish: Option<FinishReason>,
}

impl SessionEngine {
    /// Send one `hi` user message to `model` with `max_output_tokens`, no
    /// system prompt, no tools, and no reasoning effort (so Anthropic
    /// thinking budgets and reasoning labels never reject a tiny cap),
    /// through the live provider router. Nothing is recorded in any session
    /// log or usage ledger.
    ///
    /// # Errors
    /// Returns the provider error when the model is unrouted, the request
    /// fails before streaming, the stream yields an error, or the provider
    /// finishes the message with `error`.
    pub async fn probe_model(
        &self,
        model: &ModelRef,
        max_output_tokens: u32,
    ) -> Result<ModelProbeReply, ProviderError> {
        let request = CompletionRequest {
            model: model.clone(),
            system: None,
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: MODEL_PROBE_PROMPT.to_string(),
                }],
            }],
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: Some(max_output_tokens.max(1)),
            reasoning: None,
            headers: Default::default(),
        };
        let mut stream = self
            .provider_router()
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let mut reply = ModelProbeReply::default();
        while let Some(item) = stream.next().await {
            match item? {
                Event::TextDelta { delta, .. } => reply.text.push_str(&delta),
                Event::MessageFinished { finish, .. } => reply.finish = Some(finish),
                _ => {}
            }
        }
        if reply.finish == Some(FinishReason::Error) {
            return Err(ProviderError::Http(
                "provider finished the probe with an error".to_string(),
            ));
        }
        Ok(reply)
    }
}
