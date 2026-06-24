//! `DevProvider` — offline provider for local/dev use. Echoes the latest user
//! message on EVERY turn so the full stack is usable without API keys. Unlike the
//! finite scripted `FakeProvider`, it never runs out of responses.

use async_trait::async_trait;
use futures::stream;
use yaca_proto::{
    Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, Role, SessionId,
};

use crate::{Capabilities, CompletionRequest, EventStream, Provider, ProviderError};

#[derive(Default)]
pub struct DevProvider;

impl DevProvider {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

fn last_user_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| match m {
        Message::User { parts, .. } => {
            let mut text = String::new();
            for p in parts {
                if let Part::Text { text: t, .. } = p {
                    text.push_str(t);
                }
            }
            Some(text)
        }
        _ => None,
    })
}

fn reply_for(messages: &[Message]) -> String {
    match last_user_text(messages) {
        Some(user) if !user.trim().is_empty() => format!(
            "(yaca dev provider) You said: \"{user}\". No live model is configured yet — \
             wire a real provider in config for actual answers."
        ),
        _ => "(yaca dev provider) No live model is configured. Configure a provider to get \
              real responses."
            .to_string(),
    }
}

#[async_trait]
impl Provider for DevProvider {
    fn id(&self) -> &str {
        "dev"
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
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let part = PartId::new();
        let events = vec![
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: reply_for(&req.messages),
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
            },
        ];
        Ok(Box::pin(stream::iter(events.into_iter().map(Ok))))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use futures::StreamExt as _;
    use yaca_proto::{MessageId, ModelRef, Part, PartId, SessionId};

    use super::*;

    fn user_req(text: &str) -> CompletionRequest {
        CompletionRequest {
            model: ModelRef::new("fake"),
            system: None,
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: text.to_string(),
                }],
            }],
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            reasoning: None,
            headers: Default::default(),
        }
    }

    async fn delta_of(provider: &DevProvider, text: &str) -> String {
        let stream = provider
            .stream(user_req(text), SessionId::new(), MessageId::new())
            .await
            .unwrap();
        let events: Vec<_> = stream.collect().await;
        events
            .into_iter()
            .filter_map(|e| match e.unwrap() {
                Event::TextDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn echoes_user_text_on_every_turn() {
        futures::executor::block_on(async {
            let provider = DevProvider::new();
            let first = delta_of(&provider, "first message").await;
            let second = delta_of(&provider, "second message").await;
            assert!(first.contains("first message"), "first turn echoes");
            assert!(
                second.contains("second message"),
                "second turn must also respond (multi-turn)"
            );
        });
    }
}
