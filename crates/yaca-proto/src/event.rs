//! The canonical streaming `Event` (design.md §3) + its ordered `Envelope`.
//!
//! Phase 1 defines the core agent-loop events (session/message/step/text/
//! reasoning/tool/error). Team, goal, and loop event variants are additive and
//! land with their phases.

use serde::{Deserialize, Serialize};

use crate::ids::{EventSeq, MessageId, PartId, SessionId, ToolCallId};
use crate::message::{FinishReason, Role};
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
    SessionTitled {
        session: SessionId,
        title: String,
    },

    // -------- message lifecycle --------
    MessageStarted {
        session: SessionId,
        message: MessageId,
        role: Role,
    },
    MessageFinished {
        session: SessionId,
        message: MessageId,
        finish: FinishReason,
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
    },

    // -------- errors --------
    Error {
        session: Option<SessionId>,
        code: String,
        message: String,
    },
}

impl Event {
    /// The session this event belongs to, if any.
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Event::SessionCreated { session, .. }
            | Event::SessionTitled { session, .. }
            | Event::MessageStarted { session, .. }
            | Event::MessageFinished { session, .. }
            | Event::StepStarted { session, .. }
            | Event::StepFinished { session, .. }
            | Event::TextStart { session, .. }
            | Event::TextDelta { session, .. }
            | Event::TextEnd { session, .. }
            | Event::ReasoningStart { session, .. }
            | Event::ReasoningDelta { session, .. }
            | Event::ReasoningEnd { session, .. }
            | Event::ToolInputStart { session, .. }
            | Event::ToolInputDelta { session, .. }
            | Event::ToolCallRequested { session, .. }
            | Event::ToolResult { session, .. }
            | Event::ToolError { session, .. } => Some(*session),
            Event::Error { session, .. } => *session,
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
