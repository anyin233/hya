//! The canonical streaming `Event` (design.md §3) + its ordered `Envelope`.
//!
//! Phase 1 defines the core agent-loop events (session/message/step/text/
//! reasoning/tool/error). Team, goal, and loop event variants are additive and
//! land with their phases.

use serde::{Deserialize, Serialize};

use crate::ids::{EventSeq, MemberId, MessageId, PartId, SessionId, ToolCallId};
use crate::mail::{MailEndpoint, MailKind};
use crate::message::{FinishReason, MemberRunStatus, Role, TokenUsage, ToolPartState};
use crate::model::{AgentName, ModelRef, ToolName};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // -------- session lifecycle --------
    SessionCreated {
        session: SessionId,
        parent: Option<SessionId>,
        agent: AgentName,
        model: ModelRef,
        workdir: String,
    },
    SessionMoved {
        session: SessionId,
        workdir: String,
    },
    SessionTitled {
        session: SessionId,
        title: String,
    },
    SessionMetadataSet {
        session: SessionId,
        metadata: serde_json::Value,
    },
    SessionPermissionSet {
        session: SessionId,
        permission: Vec<serde_json::Value>,
    },
    SessionArchived {
        session: SessionId,
        archived: serde_json::Number,
    },
    SessionShareSet {
        session: SessionId,
        url: String,
    },
    SessionShareCleared {
        session: SessionId,
    },
    AgentSwitched {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        agent: AgentName,
    },
    ModelSwitched {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        model: ModelRef,
    },
    SessionStatus {
        session: SessionId,
        status: serde_json::Value,
    },
    CommandExecuted {
        session: SessionId,
        command: String,
        arguments: String,
        message: MessageId,
    },

    // -------- message lifecycle --------
    MessageStarted {
        session: SessionId,
        message: MessageId,
        role: Role,
    },
    UserPromptContextRecorded {
        session: SessionId,
        message: MessageId,
        files: Vec<serde_json::Value>,
        agents: Vec<serde_json::Value>,
    },
    MessageFinished {
        session: SessionId,
        message: MessageId,
        role: Role,
        finish: FinishReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TokenUsage>,
    },
    MessageDeleted {
        session: SessionId,
        message: MessageId,
    },
    PartDeleted {
        session: SessionId,
        message: MessageId,
        part: PartId,
    },

    // -------- assistant streaming --------
    StepStarted {
        session: SessionId,
        message: MessageId,
        step: u32,
    },
    StepFinished {
        session: SessionId,
        message: MessageId,
        step: u32,
        #[serde(default = "default_step_finish_reason")]
        finish: FinishReason,
    },
    TextStart {
        session: SessionId,
        message: MessageId,
        part: PartId,
    },
    TextDelta {
        session: SessionId,
        message: MessageId,
        part: PartId,
        delta: String,
    },
    TextReplace {
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    },
    TextEnd {
        session: SessionId,
        message: MessageId,
        part: PartId,
    },
    ReasoningStart {
        session: SessionId,
        message: MessageId,
        part: PartId,
    },
    ReasoningDelta {
        session: SessionId,
        message: MessageId,
        part: PartId,
        delta: String,
    },
    ReasoningEnd {
        session: SessionId,
        message: MessageId,
        part: PartId,
    },
    ReasoningReplace {
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    },

    // -------- tool lifecycle --------
    ToolInputStart {
        session: SessionId,
        message: MessageId,
        part: PartId,
        call: ToolCallId,
        name: ToolName,
    },
    ToolInputDelta {
        session: SessionId,
        message: MessageId,
        part: PartId,
        call: ToolCallId,
        name: ToolName,
        delta: String,
    },
    ToolCallRequested {
        session: SessionId,
        message: MessageId,
        part: PartId,
        call: ToolCallId,
        name: ToolName,
        input: serde_json::Value,
    },
    ToolResult {
        session: SessionId,
        message: MessageId,
        part: PartId,
        call: ToolCallId,
        output: serde_json::Value,
        time_ms: u64,
    },
    ToolError {
        session: SessionId,
        message: MessageId,
        part: PartId,
        call: ToolCallId,
        message_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    ToolPartUpdated {
        session: SessionId,
        message: MessageId,
        part: PartId,
        state: ToolPartState,
    },

    // -------- subagent (member) lifecycle --------
    // These attach to the PARENT (`session`) so they live in the parent's log and
    // stream with it. They carry only bounded metadata + a short summary — never a
    // child transcript — so observers can render a live agent tree cheaply.
    MemberSpawned {
        session: SessionId,
        member: MemberId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
        subagent_type: AgentName,
        description: String,
        depth: u32,
    },
    MemberStatusChanged {
        session: SessionId,
        member: MemberId,
        status: MemberRunStatus,
    },
    MemberFinished {
        session: SessionId,
        member: MemberId,
        status: MemberRunStatus,
        summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
    },

    // -------- event-sourced mailbox & channels (ADR-0001) --------
    // Team-scoped comms. Every variant is appended to the TEAM-ROOT session's log
    // (`session` = the root of the team tree) so a single replay reconstructs the
    // whole team's inboxes/channels/roster, and the live bus carries them to the
    // TUI for free. Additive variants — older binaries fold them via `Unknown`.
    /// Bind a team member's session to its stable, team-scoped handle.
    /// `agent_session` is the registered agent's own session; `session` is the
    /// team-root log the binding is recorded in.
    AgentRegistered {
        session: SessionId,
        agent_session: SessionId,
        handle: String,
        #[serde(default)]
        agent_type: AgentName,
    },
    /// A message from one handle to another handle or a `#channel`. Channel sends
    /// fan out to every current subscriber in the deterministic reducer, so no
    /// recipient set is baked into the event.
    MailSent {
        session: SessionId,
        from: String,
        to: MailEndpoint,
        #[serde(default)]
        kind: MailKind,
        body: String,
    },
    /// A handle subscribed to a channel; subsequent channel mail reaches it.
    ChannelJoined {
        session: SessionId,
        channel: String,
        member: String,
    },
    /// A handle unsubscribed from a channel.
    ChannelLeft {
        session: SessionId,
        channel: String,
        member: String,
    },

    // -------- errors --------
    Error {
        session: Option<SessionId>,
        code: String,
        message: String,
    },

    /// Forward-compatibility catch-all: any event whose `type` tag is not one of
    /// the variants above deserializes here instead of failing. This lets an older
    /// binary replay a log (or a client decode a stream) that contains newer event
    /// variants without erroring. NOTE: this is a unit variant, so the original
    /// payload is dropped — code that must forward unknown events losslessly should
    /// decode the raw JSON (`serde_json::Value`) at the boundary rather than relying
    /// on this round-tripping.
    #[serde(other)]
    Unknown,
}

fn default_step_finish_reason() -> FinishReason {
    FinishReason::Stop
}

impl Event {
    /// The session this event belongs to, if any.
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Event::SessionCreated { session, .. }
            | Event::SessionMoved { session, .. }
            | Event::SessionTitled { session, .. }
            | Event::SessionMetadataSet { session, .. }
            | Event::SessionPermissionSet { session, .. }
            | Event::SessionArchived { session, .. }
            | Event::SessionShareSet { session, .. }
            | Event::SessionShareCleared { session, .. }
            | Event::AgentSwitched { session, .. }
            | Event::ModelSwitched { session, .. }
            | Event::SessionStatus { session, .. }
            | Event::CommandExecuted { session, .. }
            | Event::MessageStarted { session, .. }
            | Event::UserPromptContextRecorded { session, .. }
            | Event::MessageFinished { session, .. }
            | Event::MessageDeleted { session, .. }
            | Event::PartDeleted { session, .. }
            | Event::StepStarted { session, .. }
            | Event::StepFinished { session, .. }
            | Event::TextStart { session, .. }
            | Event::TextDelta { session, .. }
            | Event::TextReplace { session, .. }
            | Event::TextEnd { session, .. }
            | Event::ReasoningStart { session, .. }
            | Event::ReasoningDelta { session, .. }
            | Event::ReasoningEnd { session, .. }
            | Event::ReasoningReplace { session, .. }
            | Event::ToolInputStart { session, .. }
            | Event::ToolInputDelta { session, .. }
            | Event::ToolCallRequested { session, .. }
            | Event::ToolResult { session, .. }
            | Event::ToolError { session, .. } => Some(*session),
            Event::ToolPartUpdated { session, .. } => Some(*session),
            Event::MemberSpawned { session, .. }
            | Event::MemberStatusChanged { session, .. }
            | Event::MemberFinished { session, .. } => Some(*session),
            Event::AgentRegistered { session, .. }
            | Event::MailSent { session, .. }
            | Event::ChannelJoined { session, .. }
            | Event::ChannelLeft { session, .. } => Some(*session),
            Event::Error { session, .. } => *session,
            Event::Unknown => None,
        }
    }
}

/// An ordered, replayable event: the unit shipped over SSE and stored in the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: EventSeq,
    pub ts_millis: i64,
    pub event: Event,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn unknown_event_type_deserializes_to_unknown() {
        // A future/unknown `type` must not fail deserialization: it maps to
        // Event::Unknown so old binaries can replay logs with newer variants.
        let json = r#"{"type":"totally_made_up_future_event","session":"ses_x","x":1}"#;
        let event: Event = serde_json::from_str(json).expect("unknown type must decode");
        assert_eq!(event, Event::Unknown);
        assert_eq!(event.session(), None);

        // A known variant still decodes to its proper variant.
        let known =
            r#"{"type":"session_share_cleared","session":"ses_00000000000000000000000000000001"}"#;
        let event: Event = serde_json::from_str(known).expect("known type decodes");
        assert!(matches!(event, Event::SessionShareCleared { .. }));

        // Envelope carrying an unknown event also decodes.
        let env_json = format!(r#"{{"seq":7,"ts_millis":1,"event":{json}}}"#);
        let env: Envelope = serde_json::from_str(&env_json).expect("envelope decodes");
        assert_eq!(env.event, Event::Unknown);
    }

    #[test]
    fn mailbox_events_round_trip_through_json() {
        let root = SessionId::new();
        let agent = SessionId::new();
        for event in [
            Event::AgentRegistered {
                session: root,
                agent_session: agent,
                handle: "reviewer-3".to_string(),
                agent_type: AgentName::new("reviewer"),
            },
            Event::MailSent {
                session: root,
                from: "main".to_string(),
                to: MailEndpoint::Channel("build".to_string()),
                kind: MailKind::Announcement,
                body: "ship it".to_string(),
            },
            Event::MailSent {
                session: root,
                from: "reviewer-1".to_string(),
                to: MailEndpoint::Handle("reviewer-2".to_string()),
                kind: MailKind::Message,
                body: "hi".to_string(),
            },
            Event::ChannelJoined {
                session: root,
                channel: "build".to_string(),
                member: "reviewer-1".to_string(),
            },
            Event::ChannelLeft {
                session: root,
                channel: "build".to_string(),
                member: "reviewer-1".to_string(),
            },
        ] {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "mailbox event must round-trip: {json}");
        }
    }
}
