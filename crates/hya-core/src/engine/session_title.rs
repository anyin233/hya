use futures::StreamExt as _;
use hya_proto::{
    Event, Message, MessageId, ModelRef, Part, PartId, PartProjection, Role, SessionId,
};
use hya_provider::CompletionRequest;

use super::SessionEngine;
use crate::error::CoreError;
use crate::title;

const TITLE_SYSTEM_PROMPT: &str = "You are a title generator. Output only a concise single-line conversation title. Use the same language as the user when possible. Do not include explanations, quotes, or tool names.";

impl SessionEngine {
    pub async fn auto_title_session(
        &self,
        session: SessionId,
        fallback_model: &ModelRef,
    ) -> Result<bool, CoreError> {
        let projection = self.store.read_projection(session).await?;
        if projection.session.id.is_none() || projection.session.parent.is_some() {
            return Ok(false);
        }
        if let Some(current) = projection.session.title.as_deref()
            && !title::is_default_or_fallback_title(current)
        {
            return Ok(false);
        }
        let Some(user_text) = only_user_text(&projection.session.messages) else {
            return Ok(false);
        };
        let generated = self.generate_title(fallback_model, &user_text).await?;
        let Some(title) = title::clean_title_output(&generated) else {
            return Ok(false);
        };
        self.set_title(session, title).await?;
        Ok(true)
    }

    async fn generate_title(&self, model: &ModelRef, user_text: &str) -> Result<String, CoreError> {
        let request = CompletionRequest {
            model: model.clone(),
            system: Some(TITLE_SYSTEM_PROMPT.to_string()),
            messages: vec![Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: user_text.to_string(),
                }],
            }],
            tools: Vec::new(),
            temperature: Some(0.0),
            max_output_tokens: Some(128),
            reasoning: None,
            headers: Default::default(),
        };
        let mut stream = self
            .providers
            .stream(request, SessionId::new(), MessageId::new())
            .await?;
        let mut output = String::new();
        while let Some(event) = stream.next().await {
            if let Event::TextDelta { delta, .. } = event? {
                output.push_str(&delta);
            }
        }
        Ok(output)
    }
}

fn only_user_text(messages: &[hya_proto::MessageProjection]) -> Option<String> {
    let mut text = None;
    for message in messages {
        match message.role {
            Role::User => {
                if text.is_some() {
                    return None;
                }
                text = Some(parts_text(&message.parts));
            }
            Role::Assistant | Role::System => {}
        }
    }
    text
}

fn parts_text(parts: &[PartProjection]) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            PartProjection::Text { text, .. } => out.push_str(text),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => {}
        }
    }
    out
}
