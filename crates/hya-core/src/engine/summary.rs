use hya_proto::{
    AgentName, Message, MessageId, ModelRef, Part, PartProjection, Projection, Role, SessionId,
};

use super::SessionEngine;
use super::{
    FixedSystemAgent, fixed_system_agent, projection_workdir, summarize_options_from_definition,
};
use crate::error::CoreError;

const COMPACT_CONTEXT_MARKER: &str = "HYA_COMPACTED_CONTEXT";

impl SessionEngine {
    /// Compact the session transcript when thresholds are exceeded.
    pub async fn compact_context(
        &self,
        session: SessionId,
        summary: String,
    ) -> Result<MessageId, CoreError> {
        self.inject_system_message(session, format!("{COMPACT_CONTEXT_MARKER}\n{summary}"))
            .await
    }

    /// Produce a summary message for the session via the summarizer.
    pub async fn summarize_session(&self, session: SessionId) -> Result<MessageId, CoreError> {
        let projection = self.store.read_projection(session).await?;
        // Empty projection has no session id; fail before workdir/catalog lookup.
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_string()));
        }
        let workdir = projection_workdir(&projection).ok_or_else(|| {
            CoreError::Invalid("session workdir required for summarize_session".to_string())
        })?;
        // Bind once from the persisted session workdir; exact-lookup only.
        let binding = self.runtime.bind_turn(&workdir)?;
        let definition = fixed_system_agent(&binding, FixedSystemAgent::Summary)?;
        let options = summarize_options_from_definition(&definition);
        let messages = summary_messages(&projection)?;
        let summarizer = self
            .summarizer
            .as_ref()
            .ok_or_else(|| CoreError::Invalid("summarizer not configured".to_string()))?;
        let summary = summarizer.summarize(&messages, options).await?;
        self.inject_system_message(
            session,
            format!("Summary of earlier conversation:\n{summary}"),
        )
        .await
    }
}

fn summary_messages(projection: &Projection) -> Result<Vec<Message>, CoreError> {
    if projection.session.id.is_none() {
        return Err(CoreError::Invalid("session not found".to_string()));
    }
    let agent = projection
        .session
        .agent
        .clone()
        .unwrap_or_else(|| AgentName::new("build"));
    let model = projection
        .session
        .model
        .clone()
        .unwrap_or_else(|| ModelRef::new("unknown"));
    Ok(projection
        .session
        .messages
        .iter()
        .map(|message| match message.role {
            Role::User => Message::User {
                id: message.id,
                parts: text_parts(&message.parts),
            },
            Role::Assistant => Message::Assistant {
                id: message.id,
                agent: agent.clone(),
                model: model.clone(),
                parts: text_parts(&message.parts),
                finish: message.finish,
                tokens: None,
            },
            Role::System => Message::System {
                id: message.id,
                content: collect_text(&message.parts),
            },
        })
        .collect())
}

fn text_parts(parts: &[PartProjection]) -> Vec<Part> {
    parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { id, text } => Some(Part::Text {
                id: *id,
                text: text.clone(),
            }),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
        })
        .collect()
}

fn collect_text(parts: &[PartProjection]) -> String {
    let mut out = String::new();
    for part in parts {
        if let PartProjection::Text { text, .. } = part {
            out.push_str(text);
        }
    }
    out
}
