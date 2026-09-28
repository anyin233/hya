//! Durable restart handoff and successor resume.
//!
//! A restart first quiesces new turns. Root turns checkpoint at their next
//! round boundary with a durable `cause: handoff` close and one pending-resume
//! row; only completed tools reach that boundary, member turns keep running,
//! and a still-`Running` Workflow run makes the restart reject. The resume
//! driver consumes each pending row exactly once — acknowledged in one
//! transaction with a durable continuation start — reserves the turn before
//! the tail check, and drives a prompt-less continuation. Turns that do not
//! reach a safe boundary stay live and reject the handoff rather than being
//! silently discarded.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hya_proto::{FinishCause, FinishReason, Projection, Role, SessionId, WorkflowRunStatus};

use super::SessionEngine;
use super::turn_end::TurnDrainReport;
use crate::engine::AgentSpec;
use crate::error::CoreError;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
/// Current restart-handoff admission state.
pub struct HandoffReadiness {
    /// Sessions holding an active turn.
    pub active: Vec<SessionId>,
    /// Sessions waiting for an unanswered permission decision.
    pub pending_asks: Vec<SessionId>,
    /// Whether new turns are currently refused by the handoff gate.
    pub quiescing: bool,
}

impl SessionEngine {
    /// Refuse new turns while current turns reach a safe boundary.
    pub fn begin_handoff_quiesce(&self) -> Vec<SessionId> {
        self.turn_gate.begin_quiesce()
    }

    /// Return active turns, pending permission asks, and gate state.
    #[must_use]
    pub fn handoff_readiness(&self) -> HandoffReadiness {
        HandoffReadiness {
            active: self.turn_gate.active_sessions(),
            pending_asks: self.pending_ask_sessions(),
            quiescing: self.turn_gate.quiescing(),
        }
    }

    fn pending_ask_sessions(&self) -> Vec<SessionId> {
        self.pending_asks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    /// Close the current open turn at a safe handoff boundary. The event log
    /// itself is the resume queue; no process-local state is required by the
    /// successor.
    pub(crate) async fn checkpoint_turn_for_handoff(
        &self,
        session: SessionId,
    ) -> Result<(), CoreError> {
        let checkpoint = self
            .store
            .checkpoint_for_handoff(
                session,
                self.runtime_owner,
                self.handoff_generation
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                String::new(),
            )
            .await?;
        for envelope in checkpoint.envelopes {
            self.publish_envelope(envelope);
        }
        Ok(())
    }

    /// Quiesce and wait for active turns to reach their handoff boundary. A
    /// turn that remains active at the deadline is reported as a straggler;
    /// the gate is reopened, sessions that already checkpointed are re-driven
    /// in-process, and the caller must keep serving. `cancelled` names only
    /// sessions that actually reached a handoff boundary — their transcript
    /// tail is the checkpoint close; a turn that ended on its own (`Stop`) or
    /// was cancelled during the window never became a handoff.
    ///
    /// `stragglers` is the reject gate: besides turns that missed the
    /// deadline it carries any session whose Workflow run is still `Running`,
    /// because a cutover cannot carry a run (the successor's startup recovery
    /// terminalizes it). A live child member is never listed here — it keeps
    /// running to its natural end, holds the gate busy past the deadline if
    /// needed, and an idle member's durable claim is revived by the
    /// successor's resident recovery.
    pub async fn handoff_turns(
        self: &Arc<Self>,
        base: &AgentSpec,
        deadline: Duration,
    ) -> TurnDrainReport {
        // A running Workflow run would be terminalized by the successor's
        // startup recovery, so the cutover must not happen while one exists.
        // Reject before quiescing anything; the caller's own quiesce (if it
        // already began) is lifted below.
        if let Ok(stranded) = self.store.sessions_with_running_workflows().await
            && !stranded.is_empty()
        {
            let _ = self.turn_gate.lift_quiesce();
            self.turn_gate.clear_handoff_started();
            let _ = self.resume_handed_off_turns(base, None).await;
            return TurnDrainReport {
                cancelled: Vec::new(),
                stragglers: stranded,
            };
        }
        let _ = self.begin_handoff_quiesce();
        let until = tokio::time::Instant::now() + deadline;
        let started = self.turn_gate.handoff_started();
        let idle = self.turn_gate.wait_idle(until).await;
        let active = self.turn_gate.active_sessions();
        let mut cancelled = Vec::new();
        for session in &started {
            if active.contains(session) {
                continue;
            }
            if let Ok(projection) = self.store.read_projection(*session).await
                && is_handoff_resume_candidate(&projection)
            {
                cancelled.push(*session);
            }
        }
        if !idle {
            let stragglers: Vec<SessionId> = started
                .into_iter()
                .filter(|session| active.contains(session))
                .collect();
            let _ = self.turn_gate.lift_quiesce();
            self.turn_gate.clear_handoff_started();
            let _ = self.resume_handed_off_turns(base, None).await;
            return TurnDrainReport {
                cancelled,
                stragglers,
            };
        }
        // The gate went idle, but a turn that ended inside the window can
        // still have started a Workflow run (a mid-flight `workflow` tool
        // call). The run would be terminalized by the successor — reject.
        if let Ok(stranded) = self.store.sessions_with_running_workflows().await
            && !stranded.is_empty()
        {
            let _ = self.turn_gate.lift_quiesce();
            self.turn_gate.clear_handoff_started();
            let _ = self.resume_handed_off_turns(base, None).await;
            return TurnDrainReport {
                cancelled,
                stragglers: stranded,
            };
        }
        self.turn_gate.clear_handoff_started();
        TurnDrainReport {
            cancelled,
            stragglers: Vec::new(),
        }
    }

    /// Resume every session with a live pending-resume row whose folded
    /// transcript still ends at a handoff close. Each resume is acknowledged
    /// exactly once — the row is consumed and a durable continuation-start
    /// marker appended in one store transaction — and the session's turn is
    /// reserved before the tail check, so concurrent discovery passes can
    /// neither duplicate a continuation nor queue one behind another. The
    /// consumed row also un-blocks the session's next handoff checkpoint, so
    /// a restart that follows a resumed (or re-driven) session works.
    pub async fn resume_handed_off_turns(
        self: &Arc<Self>,
        base: &AgentSpec,
        guidance: Option<Arc<str>>,
    ) -> Vec<SessionId> {
        self.turn_gate.clear_handoff_drain();
        let _ = self.turn_gate.lift_quiesce();
        let Ok(rows) = self.store.list_pending_resumes().await else {
            return Vec::new();
        };
        let mut resumed = Vec::new();
        for row in rows {
            let session = row.session;
            // Reserve the session's one turn BEFORE the tail check: a second
            // discovery pass (successor startup racing an aborted-handoff
            // re-drive) must see the reservation and skip, never queue a
            // duplicate continuation behind the first.
            let Ok(lease) = self.try_begin_turn(session) else {
                continue;
            };
            let Ok(projection) = self.store.read_projection(session).await else {
                drop(lease);
                continue;
            };
            if !is_handoff_resume_candidate(&projection) {
                // The transcript moved on since the checkpoint (a later
                // continuation or a fresh prompt). Consume the stale row so it
                // cannot block the next checkpoint, but drive nothing.
                drop(lease);
                if let Ok(Some(start)) = self
                    .store
                    .begin_handoff_resume(session, self.runtime_owner)
                    .await
                {
                    self.publish_envelope(start.envelope);
                }
                continue;
            }
            // Ack before driving: consume the row and append the durable
            // continuation-start marker in one transaction. A crash after this
            // point leaves a taken row — the turn closes as `interrupted` by
            // ordinary crash recovery and nothing re-runs it; a crash before
            // it leaves the row live for the next discovery pass.
            let acked = self
                .store
                .begin_handoff_resume(session, self.runtime_owner)
                .await;
            let Some(start) = (match acked {
                Ok(start) => start,
                Err(error) => {
                    tracing::warn!(
                        %session,
                        %error,
                        "handoff resume ack failed; the row stays live for the next pass"
                    );
                    drop(lease);
                    continue;
                }
            }) else {
                // Another pass took the row between discovery and ack.
                drop(lease);
                continue;
            };
            self.publish_envelope(start.envelope);
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
            let continuation_guidance = guidance.clone();
            tokio::spawn(async move {
                if let Err(error) = engine
                    .run_handoff_continuation(session, &agent, lease, continuation_guidance)
                    .await
                {
                    tracing::warn!(%session, %error, "restart-handoff continuation failed");
                }
            });
            resumed.push(session);
        }
        resumed
    }

    /// List sessions still fenced at a durable handoff close.
    pub async fn pending_handoff_sessions(self: &Arc<Self>) -> Vec<SessionId> {
        let candidates = self
            .store
            .handoff_candidate_sessions()
            .await
            .unwrap_or_default();
        let mut result = Vec::new();
        for session in candidates {
            if let Ok(projection) = self.store.read_projection(session).await
                && is_handoff_resume_candidate(&projection)
            {
                result.push(session);
            }
        }
        result
    }
}

fn is_handoff_resume_candidate(projection: &Projection) -> bool {
    let session = &projection.session;
    if session.parent.is_some() || session.is_archived() {
        return false;
    }
    if session
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.run.as_ref())
        .is_some_and(|run| {
            matches!(
                run.status,
                WorkflowRunStatus::Running | WorkflowRunStatus::Interrupted
            )
        })
    {
        return false;
    }
    let Some(last) = session.messages.last() else {
        return false;
    };
    last.role == Role::Assistant
        && last.finish == Some(FinishReason::Cancelled)
        && last.cause == Some(FinishCause::Handoff)
}
