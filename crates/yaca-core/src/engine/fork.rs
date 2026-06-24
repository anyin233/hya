use yaca_proto::{
    Event, MessageId, MessageProjection, PartId, PartProjection, Projection, SessionId,
};

use super::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
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
                self.copy_text_part(session, message, text, false).await
            }
            PartProjection::Reasoning { text, .. } => {
                self.copy_text_part(session, message, text, true).await
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
