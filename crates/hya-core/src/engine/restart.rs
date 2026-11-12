//! Stop cancels turns; the next process restores them with an explicit prompt.

use std::time::Duration;

use hya_proto::FinishCause;

use super::{SessionEngine, TurnDrainReport};
use crate::error::CoreError;

impl SessionEngine {
    /// Inject a queued restore once and return the restore prompt the next
    /// turn answers: the new prompt, or the one injected by a previous process
    /// that died before its assistant turn started. `None`: nothing to restore.
    pub async fn inject_restart_prompt(
        &self,
        session: hya_proto::SessionId,
    ) -> Result<Option<hya_proto::MessageId>, CoreError> {
        let Some((prompt, envelopes)) = self
            .store
            .take_restart_resume(session, self.runtime_owner)
            .await?
        else {
            return Ok(None);
        };
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        Ok(Some(prompt))
    }

    /// Cancel active turns; the store indexes their shutdown finishes for restore.
    pub async fn stop_for_restart(&self, deadline: Duration) -> Result<TurnDrainReport, CoreError> {
        Ok(self.drain_turns(FinishCause::Shutdown, deadline).await)
    }
}
