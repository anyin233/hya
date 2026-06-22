use yaca_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId};

use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    pub(super) async fn admit_shell_user_message(
        &self,
        session: SessionId,
    ) -> Result<MessageId, CoreError> {
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
                delta: "The following tool was executed by the user".to_string(),
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
}
