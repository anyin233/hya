//! Open-turn terminalization shared by startup crash recovery and the
//! graceful-drain deadline.
//!
//! Invariant: every assistant message ends with exactly one `MessageFinished`,
//! every non-terminal tool part reaches a terminal state, and every member row
//! the turn owns reaches a terminal status (a resident whose `task` call
//! already returned outlives the turn; see [`open_turn_terminal_events`]). A process that died mid-turn breaks that
//! invariant in its log; the next runtime owner repairs it here before any
//! turn runs. The `open_assistant_message` index (maintained on append) names
//! the sessions to repair, so the pass costs O(sessions left mid-turn), not a
//! replay of every log.

use hya_proto::{
    Envelope, Event, FinishCause, FinishReason, MemberRunStatus, OwnerRunId, PartProjection,
    Projection, Role, SessionId, ToolCallId, ToolPartState,
};
use sqlx::Row as _;

use crate::{SessionStore, StoreError, append_event_in_transaction, replay_projection};

/// Reason text written on tool parts and member rows closed by crash recovery.
pub const INTERRUPTED_REASON: &str = "interrupted: the process stopped before this turn finished";

/// What one crash-recovery pass repaired.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InterruptedTurnRecovery {
    /// Sessions that had at least one open assistant message.
    pub sessions: usize,
    /// Assistant messages closed with `cause: interrupted`.
    pub messages: usize,
    /// Events appended in total (tool errors, member finishes, message finishes).
    pub events: usize,
}

/// Terminal events that close every open assistant message of `session`:
/// `ToolError` for each pending/running tool part, `MemberFinished
/// { Cancelled }` for each spawning/running member row with no spawning call
/// or whose spawning call is still open, then one
/// `MessageFinished { Cancelled }` per open assistant message carrying `cause`.
pub(crate) fn open_turn_terminal_events(
    session: SessionId,
    projection: &Projection,
    reason: &str,
    code: &str,
    cause: Option<FinishCause>,
) -> Vec<Event> {
    let mut events = Vec::new();
    // Tool calls the dying turn still had open. A member spawned by a call
    // that already returned is a resident (ADR-0015) that outlives this turn
    // and is recovered on its own, so its row stays open.
    let open_calls: Vec<ToolCallId> = projection
        .session
        .messages
        .iter()
        .filter(|message| message.role == Role::Assistant && message.finish.is_none())
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            PartProjection::Tool {
                call,
                state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                ..
            } => Some(*call),
            _ => None,
        })
        .collect();
    for member in &projection.session.members {
        if matches!(
            member.status,
            MemberRunStatus::Spawning | MemberRunStatus::Running
        ) && member
            .tool_call
            .is_none_or(|call| open_calls.contains(&call))
        {
            events.push(Event::MemberFinished {
                session,
                member: member.member,
                status: MemberRunStatus::Cancelled,
                summary: reason.to_string(),
                child: member.child,
            });
        }
    }
    for message in &projection.session.messages {
        if message.role != Role::Assistant || message.finish.is_some() {
            continue;
        }
        for part in &message.parts {
            if let PartProjection::Tool {
                id,
                call,
                state: ToolPartState::Pending { .. } | ToolPartState::Running { .. },
                ..
            } = part
            {
                events.push(Event::ToolError {
                    session,
                    message: message.id,
                    part: *id,
                    call: *call,
                    message_text: reason.to_string(),
                    value: Some(serde_json::json!({ "code": code })),
                });
            }
        }
        events.push(Event::MessageFinished {
            session,
            message: message.id,
            role: Role::Assistant,
            finish: FinishReason::Cancelled,
            tokens: None,
            cause,
        });
    }
    events
}

impl SessionStore {
    /// Startup crash recovery: close every assistant turn a dead process left
    /// open, with `cause: interrupted`.
    ///
    /// Requires the runtime-owner claim (the single writer), so no live turn of
    /// this process can be mistaken for an orphan. Idempotent: closing a turn
    /// removes it from the open-turn index, so a second pass finds nothing.
    ///
    /// # Errors
    /// Returns [`StoreError::RuntimeOwnerClaimRequired`] without the claim, or
    /// SQLite / decode failures.
    pub async fn recover_interrupted_turns(
        &self,
        owner: OwnerRunId,
    ) -> Result<InterruptedTurnRecovery, StoreError> {
        self.require_runtime_owner(owner)?;
        let keys = sqlx::query("SELECT DISTINCT session_id FROM open_assistant_message")
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|row| row.try_get::<Vec<u8>, _>("session_id"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut report = InterruptedTurnRecovery::default();
        for key in keys {
            let Some(session) = crate::decode_session_key(&key) else {
                sqlx::query("DELETE FROM open_assistant_message WHERE session_id = ?")
                    .bind(key)
                    .execute(&self.pool)
                    .await?;
                continue;
            };
            let envelopes = self
                .close_open_turns(session, FinishCause::Interrupted, INTERRUPTED_REASON)
                .await?;
            let messages = envelopes
                .iter()
                .filter(|envelope| matches!(envelope.event, Event::MessageFinished { .. }))
                .count();
            if messages > 0 {
                report.sessions += 1;
                report.messages += messages;
            }
            report.events += envelopes.len();
        }
        Ok(report)
    }

    /// Close every open assistant turn of `session` in one writer transaction
    /// (tool parts errored, member rows cancelled, one `MessageFinished
    /// { Cancelled, cause }` per open message). Returns the appended envelopes
    /// for the caller to publish; empty when nothing was open.
    ///
    /// Used by crash recovery and by the graceful drain for a turn that did not
    /// end before the drain deadline.
    ///
    /// # Errors
    /// Returns SQLite / decode failures.
    pub async fn close_open_turns(
        &self,
        session: SessionId,
        cause: FinishCause,
        reason: &str,
    ) -> Result<Vec<Envelope>, StoreError> {
        // Fold outside the writer transaction first so the (possibly first)
        // full replay lands in the cache; the transaction folds only the tail.
        self.warm_projection(session).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let projection = replay_projection(&self.projections, &mut tx, session).await?;
        let code = if cause == FinishCause::Interrupted {
            "INTERRUPTED"
        } else {
            "CANCELLED"
        };
        let has_open_message = projection
            .session
            .messages
            .iter()
            .any(|message| message.role == Role::Assistant && message.finish.is_none());
        let events = if has_open_message {
            open_turn_terminal_events(session, &projection, reason, code, Some(cause))
        } else {
            Vec::new()
        };
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        // Every open message is closed now; any index row left for this
        // session names a message the log does not hold open (stale).
        sqlx::query("DELETE FROM open_assistant_message WHERE session_id = ?")
            .bind(session.storage_key())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(envelopes)
    }
}
