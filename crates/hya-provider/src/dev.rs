//! `DevProvider` — offline provider for local/dev use. Echoes the latest user
//! message on EVERY turn so the full stack is usable without API keys. Unlike the
//! finite scripted `FakeProvider`, it never runs out of responses.

use async_trait::async_trait;
use futures::stream;
use hya_proto::{Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, Role, SessionId};

use crate::{
    Capabilities, CompletionRequest, EventStream, Provider, ProviderError,
    append_capabilities_identity, append_identity_bytes,
};

/// Offline provider: echoes the latest user text on every turn (never exhausts).
#[derive(Default)]
pub struct DevProvider;

impl DevProvider {
    /// Construct the canonical `hya/offline` echo route.
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
    let notice = "No live provider is available. Configure a provider to continue.";
    match last_user_text(messages) {
        Some(user) if !user.trim().is_empty() => format!("{user}\n\n{notice}"),
        _ => notice.to_string(),
    }
}

fn dev_capabilities() -> Capabilities {
    Capabilities {
        streaming_tool_calls: true,
        parallel_tool_calls: true,
        ..Capabilities::default()
    }
}

#[async_trait]
impl Provider for DevProvider {
    fn id(&self) -> &str {
        "hya"
    }

    fn capabilities(&self, model: &ModelRef) -> Option<Capabilities> {
        (model.as_str() == "hya/offline").then(dev_capabilities)
    }

    fn configured_identity_v1(&self) -> Option<Vec<u8>> {
        let mut identity = Vec::new();
        append_identity_bytes(&mut identity, b"hya.provider.offline.configured.v1")?;
        append_identity_bytes(&mut identity, env!("CARGO_PKG_VERSION").as_bytes())?;
        append_identity_bytes(&mut identity, b"hya/offline")?;
        append_capabilities_identity(&mut identity, &dev_capabilities())?;
        Some(identity)
    }

    fn catalog(&self) -> Vec<crate::ProviderModel> {
        vec![crate::ProviderModel {
            provider_id: "hya".to_string(),
            model_id: "offline".to_string(),
            capabilities: dev_capabilities(),
            reasoning_variants: Vec::new(),
            reasoning_default: None,
            source: crate::ModelCatalogSource::Offline,
        }]
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
    use hya_proto::{MessageId, ModelRef, Part, PartId, SessionId};

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
