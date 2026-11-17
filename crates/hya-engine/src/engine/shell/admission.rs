use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId};

use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    pub(super) async fn admit_shell_user_message(
        &self,
        session: SessionId,
    ) -> Result<MessageId, CoreError> {
        let message = MessageId::new();
        let part = PartId::new();
        self.emit_batch_for_actor(
            None,
            session,
            vec![
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: "The following tool was executed by the user".to_string(),
                },
                Event::TextEnd {
                    session,
                    message,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
            ],
        )
        .await?;
        Ok(message)
    }
}
