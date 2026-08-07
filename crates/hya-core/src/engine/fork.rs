use hya_proto::{
    Event, MessageId, MessageProjection, PartId, PartProjection, Projection, SessionId,
};

use super::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// Record that `target` was forked from `source` at the `before` cut point.
    ///
    /// Call before copying messages. This is the only durable trace of a fork:
    /// forked sessions carry no `parent` (that means subagent lineage) and copied
    /// messages get fresh ids, so without this the fork is an orphan root.
    ///
    /// # Errors
    /// Propagates store append failures.
    pub async fn record_session_forked(
        &self,
        target: SessionId,
        source: SessionId,
        before: Option<MessageId>,
    ) -> Result<(), CoreError> {
        self.emit(
            target,
            Event::SessionForked {
                session: target,
                source,
                before_message: before,
            },
        )
        .await
    }

    /// Copy selected messages into a forked session log.
    pub async fn copy_messages_to_session(
        &self,
        target: SessionId,
        source: &Projection,
        before: Option<MessageId>,
    ) -> Result<(), CoreError> {
        for message in &source.session.messages {
            if before.is_some_and(|id| id == message.id) {
                break;
            }
            self.copy_message(target, message).await?;
        }
        Ok(())
    }

    async fn copy_message(
        &self,
        session: SessionId,
        source: &MessageProjection,
    ) -> Result<(), CoreError> {
        let message = MessageId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: source.role,
            },
        )
        .await?;
        if let Some(generation) = source.config_generation {
            self.emit(
                session,
                Event::TurnBindingRecorded {
                    session,
                    message,
                    generation,
                },
            )
            .await?;
        }
        self.record_user_prompt_context(
            session,
            message,
            source.files.clone(),
            source.agents.clone(),
        )
        .await?;
        for part in &source.parts {
            self.copy_part(session, message, part).await?;
        }
        if let Some(finish) = source.finish {
            self.emit(
                session,
                Event::MessageFinished {
                    session,
                    message,
                    role: source.role,
                    finish,
                    tokens: source.tokens,
                },
            )
            .await?;
        }
        Ok(())
    }

    async fn copy_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: &PartProjection,
    ) -> Result<(), CoreError> {
        match part {
            PartProjection::Text { text, .. } => {
                self.copy_text_part(session, message, text, false, None)
                    .await
            }
            PartProjection::Reasoning {
                text,
                provider_data,
                ..
            } => {
                self.copy_text_part(session, message, text, true, provider_data.as_ref())
                    .await
            }
            PartProjection::Tool {
                call, name, state, ..
            } => {
                let part = PartId::new();
                self.emit(
                    session,
                    Event::ToolInputStart {
                        session,
                        message,
                        part,
                        call: *call,
                        name: name.clone(),
                    },
                )
                .await?;
                self.emit(
                    session,
                    Event::ToolPartUpdated {
                        session,
                        message,
                        part,
                        state: state.clone(),
                    },
                )
                .await
            }
        }
    }

    async fn copy_text_part(
        &self,
        session: SessionId,
        message: MessageId,
        text: &str,
        reasoning: bool,
        provider_data: Option<&serde_json::Value>,
    ) -> Result<(), CoreError> {
        let part = PartId::new();
        let start = if reasoning {
            Event::ReasoningStart {
                session,
                message,
                part,
            }
        } else {
            Event::TextStart {
                session,
                message,
                part,
            }
        };
        self.emit(session, start).await?;
        let delta = if reasoning {
            Event::ReasoningDelta {
                session,
                message,
                part,
                delta: text.to_string(),
            }
        } else {
            Event::TextDelta {
                session,
                message,
                part,
                delta: text.to_string(),
            }
        };
        self.emit(session, delta).await?;
        let end = if reasoning {
            Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: provider_data.cloned(),
            }
        } else {
            Event::TextEnd {
                session,
                message,
                part,
            }
        };
        self.emit(session, end).await
    }
}
