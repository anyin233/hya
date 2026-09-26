use hya_proto::{
    Envelope, Event, MessageId, MessageProjection, PartId, PartProjection, Projection, Role,
    SessionId,
};

use super::SessionEngine;
use crate::error::CoreError;

/// Where a fork cuts the source transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkAt {
    /// Copy every visible message (the head).
    Head,
    /// Copy the messages strictly before this visible user message.
    Message(MessageId),
    /// Copy the messages whose `MessageStarted` has `seq <= until_seq`.
    UntilSeq(u64),
}

/// Why a fork cut was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ForkError {
    /// The cut message is not in the source's visible transcript.
    #[error("message not found: {0}")]
    MessageNotFound(MessageId),
    /// The cut message is not a user message.
    #[error("message {0} is not a user message")]
    NotUserMessage(MessageId),
}

/// Resolve a fork cut over the source's visible transcript (`projection`
/// folded from `envs`): the source message the copy stops before, or `None`
/// to copy every visible message. Messages hidden by a pending revert are
/// never copied.
///
/// # Errors
/// [`ForkError`] when `ForkAt::Message` names no visible user message.
pub fn fork_cut(
    envs: &[Envelope],
    projection: &Projection,
    at: ForkAt,
) -> Result<Option<MessageId>, ForkError> {
    let messages = &projection.session.messages;
    match at {
        ForkAt::Head => Ok(None),
        ForkAt::Message(id) => match messages.iter().find(|message| message.id == id) {
            Some(message) if message.role == Role::User => Ok(Some(id)),
            Some(_) => Err(ForkError::NotUserMessage(id)),
            None => Err(ForkError::MessageNotFound(id)),
        },
        ForkAt::UntilSeq(until_seq) => {
            let started_after: std::collections::BTreeSet<MessageId> = envs
                .iter()
                .filter(|env| env.seq.0 > until_seq)
                .filter_map(|env| match &env.event {
                    Event::MessageStarted { message, .. } => Some(*message),
                    _ => None,
                })
                .collect();
            Ok(messages
                .iter()
                .find(|message| started_after.contains(&message.id))
                .map(|message| message.id))
        }
    }
}

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

    /// Copy the source's visible messages strictly before `before` (all of
    /// them when `None`) into a forked session log.
    ///
    /// # Errors
    /// Propagates store append failures.
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
            self.copy_message(target, source.session.id, message)
                .await?;
        }
        Ok(())
    }

    async fn copy_message(
        &self,
        session: SessionId,
        from: Option<SessionId>,
        source: &MessageProjection,
    ) -> Result<(), CoreError> {
        // Prompt images live in the source session's blob table; the copy
        // needs its own (blobs are removed with their session).
        if let Some(from) = from {
            for attachment in crate::attachments::recorded_attachments(&source.files) {
                if let Some(bytes) = self.store.file_blob(from, &attachment.blob).await? {
                    self.store
                        .put_file_blob(session, &attachment.blob, &bytes)
                        .await?;
                }
            }
        }
        let message = MessageId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: source.role,
                // The copy keeps the original attribution; its usage stays
                // with the source, so carry the model that served it.
                agent: source.agent.clone(),
                model: source.served_model().cloned(),
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
                    cause: None,
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
                self.copy_text_part(session, message, text, false, None, None)
                    .await
            }
            PartProjection::Reasoning {
                text,
                reason,
                provider_data,
                ..
            } => {
                self.copy_text_part(
                    session,
                    message,
                    text,
                    true,
                    reason.as_deref(),
                    provider_data.as_ref(),
                )
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
        reason: Option<&str>,
        provider_data: Option<&serde_json::Value>,
    ) -> Result<(), CoreError> {
        let part = PartId::new();
        let start = if reasoning {
            Event::ReasoningStart {
                session,
                message,
                part,
                reason: reason.map(str::to_owned),
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
