//! Stop cancels turns; the next process restores them with an explicit prompt.

use std::time::Duration;

use hya_proto::{FinishCause, PartProjection, Role, ToolPartState};

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
        let sessions = self.turn_gate.prepare_drain(FinishCause::Shutdown);
        for session in &sessions {
            let projection = self.store.read_projection(*session).await?;
            let Some(message) = projection
                .session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant && message.finish.is_none())
            else {
                self.cancel_turn(*session, FinishCause::Shutdown);
                continue;
            };
            let mut unfinished = Vec::new();
            for part in &message.parts {
                if let PartProjection::Tool {
                    call, name, state, ..
                } = part
                {
                    let interrupted = matches!(
                        state,
                        ToolPartState::Pending { .. } | ToolPartState::Running { .. }
                    );
                    if interrupted {
                        unfinished.push(format!("- {name} (call {call})"));
                    }
                }
            }
            let tools = if unfinished.is_empty() {
                "None.".to_string()
            } else {
                unfinished.join("\n")
            };
            let prompt = format!(
                "Your previous turn was interrupted by a restart.\n\nTool calls that did not finish:\n{tools}\n\n\
                 Unanswered permission and question requests were cancelled. Unfinished calls are not \
                 successes; side effects may have happened before interruption. Review the existing \
                 transcript and decide what to redo, then continue the task."
            );
            let children = projection
                .team
                .roster
                .values()
                .filter(|entry| entry.session != *session && sessions.contains(&entry.session))
                .map(|entry| entry.handle.as_str())
                .collect::<Vec<_>>();
            let prompt = if children.is_empty() {
                prompt
            } else {
                format!(
                    "{prompt}\n\nInterrupted subagents: {}. Their recovery is managed by the supervisor.",
                    children.join(", ")
                )
            };
            self.restart_prompts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(*session, (message.id, prompt));
            self.cancel_turn(*session, FinishCause::Shutdown);
        }
        let report = self.drain_turns(FinishCause::Shutdown, deadline).await;
        // Surface a failed cancellation transaction without retrying it.
        for session in sessions {
            if let Some((message, _)) = self.restart_prompt(session) {
                let projection = self.store.read_projection(session).await?;
                if projection
                    .session
                    .messages
                    .iter()
                    .any(|entry| entry.id == message && entry.finish.is_none())
                {
                    return Err(CoreError::Invalid(format!(
                        "restart cancellation did not commit for session {session}"
                    )));
                }
            }
        }
        Ok(report)
    }

    pub(super) fn restart_prompt(
        &self,
        session: hya_proto::SessionId,
    ) -> Option<(hya_proto::MessageId, String)> {
        self.restart_prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session)
            .cloned()
    }
}
