//! Recorder methods for subagent (member) lifecycle events.
//!
//! These live in a child module of `engine` so they can call the module-private
//! `emit`. They append member lifecycle events to the PARENT session's log, which
//! is how a live agent tree becomes observable without leaking child transcripts.

use hya_proto::{AgentName, Event, MemberId, MemberRunStatus, SessionId};

use crate::engine::SessionEngine;
use crate::error::CoreError;

/// Everything a spawn record carries beyond the parent session it lands on.
pub(crate) struct MemberSpawnRecord {
    /// Member id on the parent log.
    pub member: MemberId,
    /// Child session when known.
    pub child: Option<SessionId>,
    /// Subagent type / agent name for the spawn.
    pub subagent_type: AgentName,
    /// Short UI description.
    pub description: String,
    /// Depth in the subagent tree.
    pub depth: u32,
    /// Verbatim directive defining the member's purpose.
    pub directive: String,
}

impl SessionEngine {
    /// Record that a member was spawned under `parent`.
    pub(crate) async fn record_member_spawned(
        &self,
        parent: SessionId,
        record: MemberSpawnRecord,
    ) -> Result<(), CoreError> {
        let MemberSpawnRecord {
            member,
            child,
            subagent_type,
            description,
            depth,
            directive,
        } = record;
        self.emit(
            parent,
            Event::MemberSpawned {
                session: parent,
                member,
                child,
                subagent_type,
                description,
                depth,
                directive,
                // Resident members are started by the supervisor, not by a tool call.
                tool_call: None,
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
