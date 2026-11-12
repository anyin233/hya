//! Recorder methods for subagent (member) lifecycle events.
//!
//! These live in a child module of `engine` so they can call the module-private
//! `emit`. They append member lifecycle events to the PARENT session's log, which
//! is how a live agent tree becomes observable without leaking child transcripts.

use hya_proto::{Event, MemberId, MemberRunStatus, SessionId};

use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// Record a member's non-terminal run status on the parent log.
    pub(crate) async fn record_member_status(
        &self,
        parent: SessionId,
        member: MemberId,
        status: MemberRunStatus,
    ) -> Result<(), CoreError> {
        self.emit(
            parent,
            Event::MemberStatusChanged {
                session: parent,
                member,
                status,
            },
        )
        .await
    }

    /// Record a member's terminal outcome plus its bounded summary.
    pub(crate) async fn record_member_finished(
        &self,
        parent: SessionId,
        member: MemberId,
        status: MemberRunStatus,
        summary: String,
        child: Option<SessionId>,
    ) -> Result<(), CoreError> {
        self.emit(
            parent,
            Event::MemberFinished {
                session: parent,
                member,
                status,
                summary,
                child,
            },
        )
        .await
    }
}
