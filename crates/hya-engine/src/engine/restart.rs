//! Stop cancels turns; the next process restores them with an explicit prompt.

use std::time::Duration;

use hya_proto::FinishCause;

use super::{SessionEngine, TurnDrainReport};
use crate::error::CoreError;
use crate::hooks::HookPoint;
use crate::store_bridge::SessionStoreExt as _;

impl SessionEngine {
    /// Inject a queued restore once and return the restore prompt the next
    /// turn answers: the new prompt, or the one injected by a previous process
    /// that died before its assistant turn started. `None`: nothing to restore.
    pub async fn inject_restart_prompt(
        &self,
        session: hya_proto::SessionId,
    ) -> Result<Option<hya_proto::MessageId>, CoreError> {
        // A bundle that answers `session.restore` owns the restore text;
        // until one does, the supervisor's interrupted-member suffix applies.
        let restored = match self
            .session_hook_dispatcher(session)
            .filter(|hooks| hooks.handles(HookPoint::SessionRestore))
        {
            Some(hooks) => {
                let suffix: String = hooks
                    .session_restore(session)
                    .await
                    .iter()
                    .map(|text| format!("\n\n{text}"))
                    .collect();
                self.store
                    .take_restart_resume_with_suffix(
                        session,
                        self.runtime_owner,
                        (!suffix.is_empty()).then_some(suffix.as_str()),
                    )
                    .await?
            }
            None => {
                self.store
                    .take_restart_resume_with_subagents(session, self.runtime_owner)
                    .await?
            }
        };
        let Some((prompt, envelopes)) = restored else {
            return Ok(None);
        };
        for envelope in envelopes {
            self.publish_envelope(envelope);
        }
        Ok(Some(prompt))
    }

    /// Dispatch `runtime.restored` once every restore prompt has been admitted.
    pub async fn notify_runtime_restored(&self) {
        self.global_hooks().runtime_restored().await;
    }

    /// Cancel active turns; the store indexes their shutdown finishes for restore.
    pub async fn stop_for_restart(&self, deadline: Duration) -> Result<TurnDrainReport, CoreError> {
        Ok(self.drain_turns(FinishCause::Shutdown, deadline).await)
    }
}
