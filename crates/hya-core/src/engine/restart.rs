//! Stop cancels turns; the next process restores them with an explicit prompt.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hya_proto::{FinishCause, PartProjection, Role, SessionId, ToolPartState};

use super::{AgentSpec, SessionEngine, TurnDrainReport};
use crate::error::CoreError;

impl SessionEngine {
    /// Cancel every active turn and queue one restore prompt per interrupted session.
    pub async fn stop_for_restart(&self, deadline: Duration) -> Result<TurnDrainReport, CoreError> {
        let report = self.drain_turns(FinishCause::Shutdown, deadline).await;
        for session in &report.cancelled {
            let projection = self.store.read_projection(*session).await?;
            let mut unfinished = Vec::new();
            if let Some(message) = projection
                .session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .filter(|message| message.cause == Some(FinishCause::Shutdown))
            {
                for part in &message.parts {
                    if let PartProjection::Tool {
                        call, name, state, ..
                    } = part
                    {
                        let interrupted = match state {
                            ToolPartState::Pending { .. } | ToolPartState::Running { .. } => true,
                            ToolPartState::Error { value, .. } => {
                                value.as_ref().is_some_and(|value| {
                                    value["code"] == "CANCELLED"
                                        || value["error"]["type"] == "cancelled"
                                })
                            }
                            ToolPartState::Completed { .. } => false,
                        };
                        if interrupted {
                            unfinished.push(format!("- {name} (call {call})"));
                        }
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
            self.store
                .queue_restart_resume(*session, self.runtime_owner, &prompt)
                .await?;
        }
        Ok(report)
    }

    /// Inject each pending restore once, reserve its session, and start a fresh turn.
    pub async fn resume_interrupted_turns(
        self: &Arc<Self>,
        base: &AgentSpec,
    ) -> Result<Vec<SessionId>, CoreError> {
        let rows = self.store.list_pending_resumes().await?;
        let mut resumed = Vec::new();
        for row in rows {
            let session = row.session;
            let lease = self.try_begin_turn(session)?;
            let projection = self.store.read_projection(session).await?;
            let envelopes = self
                .store
                .take_restart_resume(session, self.runtime_owner)
                .await?;
            if envelopes.is_empty() {
                continue;
            }
            for envelope in envelopes {
                self.publish_envelope(envelope);
            }
            let mut agent = base.clone();
            if let Some(workdir) = projection.session.workdir.as_deref() {
                agent.workdir = PathBuf::from(workdir);
            }
            if let Some(name) = projection.session.agent.clone() {
                agent.name = name;
            }
            if let Some(model) = projection.session.model.clone() {
                agent.model = model;
            }
            let engine = Arc::clone(self);
            tokio::spawn(async move {
                if let Err(error) = engine.run_restored_turn(session, &agent, lease).await {
                    tracing::error!(%session, %error, "restored turn failed");
                }
            });
            resumed.push(session);
        }
        Ok(resumed)
    }
}
