//! Stop cancels turns; the next process restores them with an explicit prompt.

use std::time::Duration;

use hya_proto::FinishCause;

use super::{SessionEngine, TurnDrainReport};
use crate::error::CoreError;

impl SessionEngine {
    /// Inject a queued restore once. `true` also covers an injected prompt
    /// whose assistant turn did not start before the previous process died.
    pub async fn inject_restart_prompt(
        &self,
        session: hya_proto::SessionId,
    ) -> Result<bool, CoreError> {
        let Some(envelopes) = self
            .store
            .take_restart_resume(session, self.runtime_owner)
            .await?
        else {
            return Ok(false);
        };
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        Ok(true)
    }

    /// Cancel every active turn and queue one restore prompt per interrupted session.
    pub async fn stop_for_restart(&self, deadline: Duration) -> Result<TurnDrainReport, CoreError> {
        Ok(self.drain_turns(FinishCause::Shutdown, deadline).await)
    }
}
