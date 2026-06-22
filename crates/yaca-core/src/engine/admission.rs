use yaca_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId};

use super::SessionEngine;
use crate::error::CoreError;
use crate::hooks::{
    CommandExecuteBeforeInput, CommandExecuteBeforeOutcome, MessageUserBeforeInput,
    MessageUserBeforeOutcome,
};

impl SessionEngine {
    pub async fn inject_system_message(
        &self,
        session: SessionId,
        content: String,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::System,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextStart {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextDelta {
                session,
                message,
                part,
                delta: content,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextEnd {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                role: Role::System,
                finish: FinishReason::Stop,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn admit_user_prompt(
        &self,
        session: SessionId,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let text = if let Some(hooks) = &self.hooks {
            match hooks
                .message_user_before(MessageUserBeforeInput { session, text })
                .await
            {
                MessageUserBeforeOutcome::Continue { text } => text,
            }
        } else {
            text
        };
        let message = MessageId::new();
        let part = PartId::new();
        self.emit(
            session,
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextStart {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextDelta {
                session,
                message,
                part,
                delta: text,
            },
        )
        .await?;
        self.emit(
            session,
            Event::TextEnd {
                session,
                message,
                part,
            },
        )
        .await?;
        self.emit(
            session,
            Event::MessageFinished {
                session,
                message,
                role: Role::User,
                finish: FinishReason::Stop,
            },
        )
        .await?;
        Ok(message)
    }

    pub async fn admit_command_prompt(
        &self,
        session: SessionId,
        command: String,
        arguments: String,
        text: String,
    ) -> Result<MessageId, CoreError> {
        let text = if let Some(hooks) = &self.hooks {
            match hooks
                .command_execute_before(CommandExecuteBeforeInput {
                    session,
                    command: command.clone(),
                    arguments: arguments.clone(),
                    text,
                })
                .await
            {
                CommandExecuteBeforeOutcome::Continue { text } => text,
            }
        } else {
            text
        };
        let message = self.admit_user_prompt(session, text).await?;
        self.emit(
            session,
            Event::CommandExecuted {
                session,
                command,
                arguments,
                message,
            },
        )
        .await?;
        Ok(message)
    }
}
