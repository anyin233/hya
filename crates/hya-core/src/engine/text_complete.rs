use async_trait::async_trait;
use hya_proto::{MessageId, PartId, SessionId};

use super::SessionEngine;
use crate::hooks::{TextCompleteInput, TextCompleteOutcome};
use crate::kernel::round::CompleteText;

/// The `text.complete` hook, applied by the kernel round to each text part.
#[async_trait]
impl CompleteText for SessionEngine {
    async fn complete(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Option<String> {
        let hooks = self.active_hook_dispatcher(session)?;
        let original = text.clone();
        let TextCompleteOutcome::Continue { text } = hooks
            .text_complete(TextCompleteInput {
                session,
                message,
                part,
                text,
            })
            .await;
        (text != original).then_some(text)
    }
}
