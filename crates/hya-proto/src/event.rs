//! The canonical streaming `Event` (design.md §3) + its ordered `Envelope`.
//!
//! Phase 1 defines the core agent-loop events (session/message/step/text/
//! reasoning/tool/error). Team, goal, and loop event variants are additive and
//! land with their phases.
//!
//! Wire form: `#[serde(tag = "type", rename_all = "snake_case")]`. Consumers
//! treat envelopes as the unit of store replay and SSE; see
//! `docs/architecture/event-model.md`.

use serde::{Deserialize, Serialize};

use crate::ids::{
    ActorEpoch, ConfigGeneration, EventSeq, MemberId, MessageId, PartId, SessionId, ToolCallId,
};
use crate::mail::{MailEndpoint, MailKind};
use crate::message::{
    FinishReason, MemberRunStatus, Role, RosterStatus, SubagentMode, TokenUsage, ToolPartState,
};
use crate::model::{AgentName, ModelRef, ToolName};

/// Canonical runtime event stream: one tagged variant per discrete state change.
///
/// Persist durable variants with a real `EventSeq`; high-frequency text may be
/// published live at `seq == 0` and re-emitted durably after the stream ends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // -------- session lifecycle --------
    /// Session is created; fold sets id/parent/agent/model/workdir.
    SessionCreated {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent session when this is a child (lineage toward team root).
        parent: Option<SessionId>,
        /// Initial agent binding.
        agent: AgentName,
        /// Initial model binding.
        model: ModelRef,
        /// Absolute workdir for tools.
        workdir: String,
    },
    /// Session workdir changed.
    SessionMoved {
        /// Session this event belongs to.
        session: SessionId,
        /// New absolute workdir.
        workdir: String,
    },
    /// Session title set or updated.
    SessionTitled {
        /// Session this event belongs to.
        session: SessionId,
        /// Display title.
        title: String,
    },
    /// Arbitrary session metadata replaced wholesale.
    SessionMetadataSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Full metadata object (replaces prior metadata).
        metadata: serde_json::Value,
    },
    /// Session permission rule list replaced (does not merge).
    SessionPermissionSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Full permission rule list after the set.
        permission: Vec<serde_json::Value>,
    },
    /// Session archived stamp applied.
    SessionArchived {
        /// Session this event belongs to.
        session: SessionId,
        /// Archive marker (compat numeric stamp).
        archived: serde_json::Number,
    },
    /// Share URL recorded for the session.
    SessionShareSet {
        /// Session this event belongs to.
        session: SessionId,
        /// Public share URL.
        url: String,
    },
    /// Share URL cleared (reducer sets share to `None`).
    SessionShareCleared {
        /// Session this event belongs to.
        session: SessionId,
    },
    /// Active agent switched for the session.
    AgentSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// Optional transcript anchor for when the switch occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        /// New agent name.
        agent: AgentName,
    },
    /// Active model switched for the session.
    ModelSwitched {
        /// Session this event belongs to.
        session: SessionId,
        /// Optional transcript anchor for when the switch occurred.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<MessageId>,
        /// New model reference.
        model: ModelRef,
    },
    /// Free-form status ping; reducer no-op (compat `session.status` bridge).
    SessionStatus {
        /// Session this event belongs to.
        session: SessionId,
        /// Opaque status payload for live UIs.
        status: serde_json::Value,
    },
    /// Slash command produced a user message; reducer no-op (compat `command.executed`).
    CommandExecuted {
        /// Session this event belongs to.
        session: SessionId,
        /// Command name without `/`.
        command: String,
        /// Argument string after the command.
        arguments: String,
        /// User message id that was admitted.
        message: MessageId,
    },

    // -------- message lifecycle --------
    /// Opens a message row in the projection (`role` + new `message` id).
    MessageStarted {
        /// Session this event belongs to.
        session: SessionId,
        /// New message id.
        message: MessageId,
        /// Speaker role for the message.
        role: Role,
    },
    /// Records which immutable runtime snapshot (`ConfigGeneration`) ran this assistant turn.
    TurnBindingRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message the binding applies to.
        message: MessageId,
        /// Lightweight generation identity (registry contents stay outside the log).
        generation: ConfigGeneration,
    },
    /// `@file` / `@agent` prompt context; engine emits nothing when both vectors are empty.
    UserPromptContextRecorded {
        /// Session this event belongs to.
        session: SessionId,
        /// User message the context attaches to.
        message: MessageId,
        /// File attachment metadata for the provider request builder.
        files: Vec<serde_json::Value>,
        /// Agent mention metadata for the provider request builder.
        agents: Vec<serde_json::Value>,
    },
    /// Closes a message; clients that saw start must eventually see finish.
    MessageFinished {
        /// Session this event belongs to.
        session: SessionId,
        /// Message being finished.
        message: MessageId,
        /// Role of the finished message.
        role: Role,
        /// Terminal finish reason.
        finish: FinishReason,
        /// Aggregated token usage when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TokenUsage>,
    },
    /// Removes a whole message from the projected view.
    MessageDeleted {
        /// Session this event belongs to.
        session: SessionId,
        /// Message to drop.
        message: MessageId,
    },
    /// Removes one part from a message in the projected view.
    PartDeleted {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Part to drop.
        part: PartId,
    },

    // -------- assistant streaming --------
    /// Provider round started; reducer no-op (UI / step markers).
    StepStarted {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message owning the round.
        message: MessageId,
        /// Zero-based round index within the turn.
        step: u32,
    },
    /// Provider round finished; reducer no-op. `finish` defaults to `stop` on old logs.
    StepFinished {
        /// Session this event belongs to.
        session: SessionId,
        /// Assistant message owning the round.
        message: MessageId,
        /// Zero-based round index within the turn.
        step: u32,
        /// Provider finish for this round (default `stop` when absent).
        #[serde(default = "default_step_finish_reason")]
        finish: FinishReason,
    },
    /// Begin a text part (empty until deltas/replace).
    TextStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New text part id.
        part: PartId,
    },
    /// Append streaming text; field is `delta`, not `text`.
    TextDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target text part.
        part: PartId,
        /// Chunk to append.
        delta: String,
    },
    /// Wholesale text overwrite (durable final content and `text_complete` rewrites).
    TextReplace {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target text part.
        part: PartId,
        /// Full replacement text.
        text: String,
    },
    /// Text stream end marker; reducer no-op (text already accumulated).
    TextEnd {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Closed text part.
        part: PartId,
    },
    /// Begin a reasoning part.
    ReasoningStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New reasoning part id.
        part: PartId,
    },
    /// Append reasoning text chunk.
    ReasoningDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target reasoning part.
        part: PartId,
        /// Chunk to append.
        delta: String,
    },
    /// End reasoning and attach opaque `provider_data` for round-trip.
    ReasoningEnd {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Closed reasoning part.
        part: PartId,
        /// Opaque provider state (for example encrypted thinking); round-trip verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    /// Wholesale reasoning text overwrite.
    ReasoningReplace {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Target reasoning part.
        part: PartId,
        /// Full replacement reasoning text.
        text: String,
    },

    // -------- tool lifecycle --------
    /// Tool part opened in `Pending` (null input until arguments arrive).
    ToolInputStart {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// New tool part id.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
    },
    /// Raw argument JSON stream; reducer no-op (compat may forward as pending raw).
    ToolInputDelta {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Argument JSON fragment.
        delta: String,
    },
    /// Model requested a tool call → `Running`; turn loop collects these for dispatch.
    ToolCallRequested {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Parsed tool arguments.
        input: serde_json::Value,
    },
    /// Tool succeeded → `Completed`.
    ToolResult {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Tool output JSON (may be capped).
        output: serde_json::Value,
        /// Execution duration in milliseconds.
        time_ms: u64,
    },
    /// Tool failed/denied/blocked → `Error`.
    ToolError {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Tool call correlation id.
        call: ToolCallId,
        /// Human/model-facing error string.
        message_text: String,
        /// Optional structured error (for example `{ "error": { "type", "message" } }` or `STALE_ACTOR_CLAIM`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
    /// Direct tool-part state overwrite (fork/copy, out-of-band progress).
    ToolPartUpdated {
        /// Session this event belongs to.
        session: SessionId,
        /// Parent message.
        message: MessageId,
        /// Tool part.
        part: PartId,
        /// Full replacement tool state.
        state: ToolPartState,
    },

    // -------- subagent (member) lifecycle --------
    // These attach to the PARENT (`session`) so they live in the parent's log and
    // stream with it. They carry only bounded metadata + a short summary — never a
    // child transcript — so observers can render a live agent tree cheaply.
    /// Member spawned on the **parent** log (status → Spawning); never carries child transcript.
    MemberSpawned {
        /// Parent session log this event is appended to.
        session: SessionId,
        /// Member id within the parent tree.
        member: MemberId,
        /// Child session when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child: Option<SessionId>,
        /// Subagent type / agent name for the spawn.
        subagent_type: AgentName,
        /// Short human description of the task.
        description: String,
        /// Depth in the subagent tree (root children are 1).
        depth: u32,
    },
    /// Member status update on the parent log.
    MemberStatusChanged {
        /// Parent session log.
        session: SessionId,
        /// Member being updated.
        member: MemberId,
        /// New run status.
        status: MemberRunStatus,
    },
    /// Member finished with a **bounded** summary string (not the child transcript).
    MemberFinished {
        /// Parent session log.
        session: SessionId,
        /// Member being finished.
        member: MemberId,
        /// Terminal member status.
        status: MemberRunStatus,
        /// Bounded summary for the parent/TUI.
        summary: String,
        /// Optional child session id when known at finish.
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
        /// Team-root log session.
        session: SessionId,
        /// Agent's own session id.
        agent_session: SessionId,
        /// Stable team-scoped handle (mail address).
        handle: String,
        /// Declared agent type for the roster.
        #[serde(default)]
        agent_type: AgentName,
        /// Transient vs resident scheduling (ADR-0002). `#[serde(default)]` so logs
        /// predating Phase 4 replay every member as transient.
        #[serde(default)]
        mode: SubagentMode,
    },
    /// A team member's live activity changed (idle ⇄ busy, or a terminal
    /// done/failed), optionally updating its short current-task label. Appended to
    /// the TEAM-ROOT log by the resident supervisor so the roster status column
    /// and quiescence view replay for free.
    AgentActivityChanged {
        /// Team-root log session.
        session: SessionId,
        /// Roster handle being updated.
        handle: String,
        /// New live activity.
        status: RosterStatus,
        /// Optional short task label; `Some("resident stopped")` is an explicit-stop sentinel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current_task: Option<String>,
    },
    /// Durable boundary between queued resident work and a turn that may dispatch
    /// tools, children, or provider requests. Appended before those effects.
    ResidentWorkStarted {
        /// Team-root log session.
        session: SessionId,
        /// Resident actor session whose work is starting.
        actor_session: SessionId,
        /// Roster handle of the actor.
        handle: String,
        /// Actor epoch for this incarnation.
        epoch: ActorEpoch,
        /// Inbox length covered by this coalesced resident turn.
        inbox_through: u64,
    },
    /// A message from one handle to another handle or a `#channel`. Channel sends
    /// fan out to every current eligible subscriber in the deterministic reducer, so no
    /// recipient set is baked into the event.
    MailSent {
        /// Team-root log session.
        session: SessionId,
        /// Sender handle.
        from: String,
        /// Direct handle or channel endpoint.
        to: MailEndpoint,
        /// Message intent (default chatter vs announcement).
        #[serde(default)]
        kind: MailKind,
        /// Message body text.
        body: String,
    },
    /// A handle subscribed to a channel; subsequent channel mail reaches it.
    ChannelJoined {
        /// Team-root log session.
        session: SessionId,
        /// Channel name without leading `#`.
        channel: String,
        /// Member handle joining.
        member: String,
    },
    /// A handle unsubscribed from a channel.
    ChannelLeft {
        /// Team-root log session.
        session: SessionId,
        /// Channel name without leading `#`.
        channel: String,
        /// Member handle leaving.
        member: String,
    },

    // -------- errors --------
    /// Runtime error frame; `session` optional for global errors; reducer no-op.
    Error {
        /// Session scope when the error is session-local; `None` for global errors.
        session: Option<SessionId>,
        /// Machine-readable error code for clients.
        code: String,
        /// Human-readable error text.
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
            | Event::TurnBindingRecorded { session, .. }
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
            | Event::AgentActivityChanged { session, .. }
            | Event::ResidentWorkStarted { session, .. }
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
    /// Global log sequence, or `0` for live-only (never persisted) publishes.
    pub seq: EventSeq,
    /// Unix-epoch milliseconds when the envelope was produced.
    pub ts_millis: i64,
    /// Event payload.
    pub event: Event,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn turn_binding_round_trips_and_folds_only_generation_identity() {
        let session = SessionId::new();
        let message = MessageId::new();
        let binding = Event::TurnBindingRecorded {
            session,
            message,
            generation: ConfigGeneration::INITIAL,
        };
        let encoded = serde_json::to_string(&binding).expect("encode turn binding");
        let decoded: Event = serde_json::from_str(&encoded).expect("decode turn binding");
        assert_eq!(decoded, binding);
        assert_eq!(decoded.session(), Some(session));

        let projection = crate::Projection::from_events(&[
            Envelope {
                seq: EventSeq(1),
                ts_millis: 1,
                event: Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                },
            },
            Envelope {
                seq: EventSeq(2),
                ts_millis: 2,
                event: binding,
            },
        ]);
        let projected = projection
            .session
            .messages
            .first()
            .expect("projected assistant message");
        assert_eq!(projected.config_generation, Some(ConfigGeneration::INITIAL));
        assert!(!encoded.contains("tools"));
        assert!(!encoded.contains("skills"));
    }

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
                mode: SubagentMode::Resident,
            },
            Event::AgentActivityChanged {
                session: root,
                handle: "reviewer-3".to_string(),
                status: RosterStatus::Busy,
                current_task: Some("reviewing".to_string()),
            },
            Event::ResidentWorkStarted {
                session: root,
                actor_session: agent,
                handle: "reviewer-3".to_string(),
                epoch: ActorEpoch::INITIAL,
                inbox_through: 2,
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
