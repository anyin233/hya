//! Turn termination: the end-event invariant, the graceful drain, and the
//! leader-failed broadcast.
//!
//! Invariant: every assistant message ends with exactly one `MessageFinished`,
//! every non-terminal tool part reaches a terminal state, and every member
//! reaches a terminal status. A turn that does not end on its own (cancel,
//! provider/runtime error) is closed here with a [`FinishCause`]; a process
//! stop drains every in-flight turn in every session; a crash is repaired by
//! the next runtime owner (`SessionStore::recover_interrupted_turns`).

use std::time::Duration;

use hya_proto::{
    Event, FinishCause, FinishReason, MemberRunStatus, MessageId, PartProjection, Role,
    RosterStatus, SessionId, SubagentMode, ToolPartState,
};
use hya_store::ActorClaim;

use super::SessionEngine;
use crate::error::CoreError;

/// How long a graceful stop waits for cancelled turns to end on their own
/// before closing whatever is still open itself.
pub const DRAIN_DEADLINE: Duration = Duration::from_secs(5);

/// Result of [`SessionEngine::drain_turns`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TurnDrainReport {
    /// Sessions whose in-flight turn was cancelled by the drain.
    pub cancelled: Vec<SessionId>,
    /// Sessions whose turn had not ended by the deadline; their open messages
    /// were closed by the drain itself.
    pub stragglers: Vec<SessionId>,
}

/// Body of the harness mail every live member receives when its lead's turn
/// fails. `{error}` is the lead's (truncated) failure.
pub(crate) fn leader_failed_notice(error: &str) -> String {
    const MAX: usize = 400;
    let error = if error.len() > MAX {
        let mut end = MAX;
        while !error.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &error[..end])
    } else {
        error.to_string()
    };
    format!(
        "LEADER FAILED: your team lead's turn ended with an error and the lead will not \
         respond ({error}). Wrap up now: finish or commit the unit you are on, send your \
         report, then stop. Do not start new work."
    )
}

/// The `cause` a failed turn records next to `finish: error`.
pub(crate) fn error_cause(error: &CoreError) -> Option<FinishCause> {
    match error {
        CoreError::Provider(_) => Some(FinishCause::ProviderError),
        _ => None,
    }
}

impl SessionEngine {
    /// Cancel `session`'s in-flight turn and record `cause` on its closing
    /// `MessageFinished`. `false` when the session has no active turn.
    pub fn cancel_turn(&self, session: SessionId, cause: FinishCause) -> bool {
        self.turn_gate.cancel(session, cause)
    }

    /// Stop `session`'s in-flight turn with `cause` and wait (up to
    /// `deadline`) for it to close its own message; a turn still running at
    /// the deadline has its open messages closed here, as a drain would.
    /// `false` when the session had no active turn.
    pub async fn stop_turn(
        &self,
        session: SessionId,
        cause: FinishCause,
        deadline: Duration,
    ) -> bool {
        if !self.turn_gate.cancel(session, cause) {
            return false;
        }
        let until = tokio::time::Instant::now() + deadline;
        if !self.turn_gate.wait_released(session, until).await
            && let Ok(envelopes) = self
                .store
                .close_open_turns(session, cause, "stopped: the stop deadline passed")
                .await
        {
            for envelope in envelopes {
                self.publish_envelope(envelope);
            }
        }
        true
    }

    /// Whether a drain has begun (new turns are refused), and with which cause.
    #[must_use]
    pub fn draining(&self) -> Option<FinishCause> {
        self.turn_gate.draining()
    }

    /// First, synchronous step of a drain: refuse every new turn and cancel
    /// every in-flight turn with `cause`. Returns the cancelled sessions.
    /// [`drain_turns`](Self::drain_turns) does this and then waits.
    pub fn begin_drain(&self, cause: FinishCause) -> Vec<SessionId> {
        self.turn_gate.begin_drain(cause)
    }

    /// Gracefully stop every in-flight turn in every session.
    ///
    /// Refuses new turns from now on, cancels each active turn with `cause`
    /// (each turn then closes its own message: `MessageFinished { cancelled,
    /// cause }`, open tool parts errored, the members it spawned cancelled),
    /// and waits up to `deadline` for them to end. A turn still running at the
    /// deadline has its open messages closed here instead.
    pub async fn drain_turns(&self, cause: FinishCause, deadline: Duration) -> TurnDrainReport {
        let mut cancelled = self.turn_gate.begin_drain(cause);
        // Turns an earlier `begin_drain` already cancelled are still ours.
        for session in self.turn_gate.active_sessions() {
            if !cancelled.contains(&session) {
                cancelled.push(session);
            }
        }
        let until = tokio::time::Instant::now() + deadline;
        let mut report = TurnDrainReport {
            cancelled,
            stragglers: Vec::new(),
        };
        if self.turn_gate.wait_idle(until).await {
            return report;
        }
        report.stragglers = self.turn_gate.active_sessions();
        for session in &report.stragglers {
            if let Ok(envelopes) = self
                .store
                .close_open_turns(*session, cause, "stopped: the drain deadline passed")
                .await
            {
                for envelope in envelopes {
                    self.publish_envelope(envelope);
                }
            }
        }
        report
    }

    /// Close one assistant message a turn could not finish itself: error its
    /// pending/running tool parts, cancel the members those tool calls
    /// spawned, then append its single `MessageFinished { finish, cause }`.
    /// Everything is checked against the folded log, so a message or part
    /// that already reached a terminal state is left alone.
    pub(crate) async fn close_turn_message(
        &self,
        actor_claim: Option<&ActorClaim>,
        session: SessionId,
        message: MessageId,
        finish: FinishReason,
        cause: Option<FinishCause>,
    ) -> Result<(), CoreError> {
        let projection = self.store.read_projection(session).await?;
        let Some(entry) = projection
            .session
            .messages
            .iter()
            .find(|entry| entry.id == message)
        else {
            return Ok(());
        };
        if entry.finish.is_some() {
            return Ok(());
        }
        let (reason, code) = match finish {
            FinishReason::Error => (
                "the turn failed before this tool call finished",
                "TURN_FAILED",
            ),
            _ => (
                "cancelled: the turn stopped before this tool call finished",
                "CANCELLED",
            ),
        };
        let open_parts: Vec<_> = entry
            .parts
            .iter()
            .filter_map(|part| match part {
                PartProjection::Tool {
                    id,
                    call,
                    state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                    ..
                } => Some((*id, *call)),
                _ => None,
            })
            .collect();
        for member in &projection.session.members {
            if matches!(
                member.status,
                MemberRunStatus::Spawning | MemberRunStatus::Running
            ) && member
                .tool_call
                .is_some_and(|call| open_parts.iter().any(|(_, open)| *open == call))
            {
                self.emit_for_actor(
                    actor_claim,
                    session,
                    Event::MemberFinished {
                        session,
                        member: member.member,
                        status: MemberRunStatus::Cancelled,
                        summary: reason.to_string(),
                        child: member.child,
                    },
                )
                .await?;
            }
        }
        for (part, call) in open_parts {
            self.emit_for_actor(
                actor_claim,
                session,
                Event::ToolError {
                    session,
                    message,
                    part,
                    call,
                    message_text: reason.to_string(),
                    value: Some(serde_json::json!({ "code": code })),
                },
            )
            .await?;
        }
        self.emit_for_actor(
            actor_claim,
            session,
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish,
                tokens: None,
                cause,
            },
        )
        .await
    }

    /// Leader-failed broadcast: when a team root's (lead's) turn fails, mail
    /// every live resident member of its team — author `harness` — telling
    /// it to wrap up. Sessions that are not a team root, or teams without
    /// live members, send nothing. Best effort: a member that went terminal
    /// meanwhile is skipped.
    pub(crate) async fn broadcast_leader_failed(&self, session: SessionId, error: &CoreError) {
        let Ok(projection) = self.store.read_projection(session).await else {
            return;
        };
        if projection.session.parent.is_some() {
            return;
        }
        let members: Vec<String> = projection
            .team
            .roster
            .values()
            .filter(|entry| {
                entry.session != session
                    && entry.mode == SubagentMode::Resident
                    && !matches!(entry.status, RosterStatus::Done | RosterStatus::Failed)
            })
            .map(|entry| entry.handle.clone())
            .collect();
        if members.is_empty() {
            return;
        }
        let body = leader_failed_notice(&error.to_string());
        for handle in members {
            if let Ok(envelope) = self
                .store
                .append_harness_mail(session, &handle, body.clone())
                .await
            {
                self.publish_envelope(envelope);
            }
        }
    }
}
