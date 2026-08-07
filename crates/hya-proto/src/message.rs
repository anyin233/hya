//! Message / Part tagged unions (design.md §3). Timestamps live on the DB rows
//! and the `Envelope`, not on these value types (added per-need in later phases).

use serde::{Deserialize, Serialize};

use crate::ids::{MessageId, PartId, ToolCallId};
use crate::model::{AgentName, ModelRef, ToolName};

/// Speaker role of a transcript message (wire: snake_case).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Human or client-admitted user content.
    User,
    /// Model output for a turn.
    Assistant,
    /// Injected system/summary/compact content.
    System,
}

/// Why a message or provider step ended (wire: snake_case).
///
/// Terminal on both [`crate::event::Event::MessageFinished`] and
/// [`crate::event::Event::StepFinished`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    /// Normal completion with no further tool calls.
    Stop,
    /// Model requested tools; the turn continues with another round.
    ToolCalls,
    /// Hit an output length limit.
    Length,
    /// Cancel token, sidecar loss, or client abort.
    Cancelled,
    /// Hard provider/tool failure after the assistant message started.
    Error,
}

/// Lifecycle status of a spawned subagent member, as observed by the lead/tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRunStatus {
    /// Child session is being created / admitted.
    Spawning,
    /// Child is actively running a turn.
    Running,
    /// Child finished successfully (bounded summary available).
    Done,
    /// Child failed or errored.
    Failed,
    /// Child was cancelled (parent cancel, takeover, root cleanup).
    Cancelled,
}

/// How a spawned subagent is scheduled (ADR-0002).
///
/// - `Transient` (default): the historical blocking join model — spawn, run one
///   turn, summarize, and die while the parent waits. Unchanged behavior.
/// - `Resident`: a long-lived, addressable event-driven actor. Idle at zero token
///   cost; woken by inbound mail to run exactly one turn, then back to idle.
///
/// `Default` is `Transient` and the field is `#[serde(default)]` everywhere it is
/// carried, so older logs (which never wrote a mode) replay as transient.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentMode {
    /// Blocking one-shot subagent (default for older logs).
    #[default]
    Transient,
    /// Long-lived mail-woken actor.
    Resident,
}

impl SubagentMode {
    /// Parse a model-/frontmatter-supplied mode. Truthy resident markers map to
    /// [`SubagentMode::Resident`]; everything else (including empty) is transient,
    /// so a missing mode is never an error and defaults safely.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "resident" | "true" | "yes" | "1" => SubagentMode::Resident,
            _ => SubagentMode::Transient,
        }
    }

    /// Whether this is the resident (long-lived actor) mode.
    #[must_use]
    pub fn is_resident(self) -> bool {
        matches!(self, SubagentMode::Resident)
    }
}

/// Live activity of a team member in the roster (ADR-0002). Drives the TUI status
/// column and the team-scoped quiescence detector.
///
/// `Default` is `Idle` and it is `#[serde(default)]` on `RosterEntry`, so older
/// logs replay with idle members.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterStatus {
    /// Parked at zero token cost, awaiting mail (or never yet woken).
    #[default]
    Idle,
    /// Currently running (or queued to run) a turn.
    Busy,
    /// Finished its work and will not run again unless re-woken.
    Done,
    /// Terminated by cancellation / budget kill.
    Failed,
}

/// Token counters on a finished message or stream round.
///
/// Decode accepts `prompt`/`completion` aliases for `input`/`output`.
/// [`TokenUsage::merge`] takes the **max** per field (providers re-report
/// cumulative totals); the turn loop **sums** rounds separately when building
/// final `MessageFinished.tokens`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Prompt / input tokens (serde alias `prompt`).
    #[serde(default, alias = "prompt")]
    pub input: u64,
    /// Completion / output tokens (serde alias `completion`).
    #[serde(default, alias = "completion")]
    pub output: u64,
    /// Reasoning tokens when the provider reports them.
    #[serde(default)]
    pub reasoning: u64,
    /// Cache-read tokens when the provider reports them.
    #[serde(default)]
    pub cache_read: u64,
    /// Cache-write tokens when the provider reports them.
    #[serde(default)]
    pub cache_write: u64,
}

impl TokenUsage {
    /// True when every counter is zero.
    #[must_use]
    pub fn is_zero(self) -> bool {
        self == Self::default()
    }

    /// Fold another sample by taking the maximum of each counter (not a sum).
    pub fn merge(&mut self, other: Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.reasoning = self.reasoning.max(other.reasoning);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
    }
}

/// Per-message USD cost pair (schema companion for `message.cost_json`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CostBreakdown {
    /// Estimated input cost in USD.
    pub input_usd: f64,
    /// Estimated output cost in USD.
    pub output_usd: f64,
}

/// Lifecycle of a tool call as it streams: pending → running → completed | error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum ToolPartState {
    /// Arguments not yet fully known / tool not yet authorized.
    Pending {
        /// Partial or null input JSON so far.
        input: serde_json::Value,
    },
    /// Model issued a call; tool is executing or about to.
    Running {
        /// Full input JSON for the call.
        input: serde_json::Value,
    },
    /// Tool returned successfully.
    Completed {
        /// Input that was executed.
        input: serde_json::Value,
        /// Tool output JSON (may be capped for context size).
        output: serde_json::Value,
        /// Wall time for the call in milliseconds.
        time_ms: u64,
    },
    /// Tool failed, was denied, or was blocked.
    Error {
        /// Input associated with the failed call.
        input: serde_json::Value,
        /// Human/model-facing error message.
        message: String,
        /// Optional structured payload (for example `{ "error": { "type", "message" } }`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
    },
}

/// One content part of a model-facing message (not the projected view type).
///
/// Wire tag is `type` (snake_case). Media exists here for provider requests but
/// has no [`crate::projection::PartProjection`] counterpart.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    /// Plain text segment.
    Text {
        /// Stable part id for streaming replace/end correlation.
        id: PartId,
        /// Full text for this part.
        text: String,
    },
    /// Provider reasoning / thinking text.
    Reasoning {
        /// Stable part id.
        id: PartId,
        /// Accumulated reasoning text.
        text: String,
        /// Opaque provider state (for example encrypted thinking blocks) to round-trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_data: Option<serde_json::Value>,
    },
    /// Media attachment for the model request path (not folded into projection).
    Media {
        /// Stable part id.
        id: PartId,
        /// MIME type (for example `image/png`).
        media_type: String,
        /// Payload (URI or encoded data, depending on producer).
        data: String,
        /// Optional original filename for display.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
    },
    /// Tool call with streaming state.
    Tool {
        /// Stable part id.
        id: PartId,
        /// Correlates with tool events and permission asks.
        call_id: ToolCallId,
        /// Canonical tool name.
        name: ToolName,
        /// Current phase and payloads.
        state: ToolPartState,
    },
}

/// A full message value. Phase 1 covers the core three roles; synthetic /
/// agent-switched / model-switched / compaction variants are added with the
/// phases that emit them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    /// User message assembled for the provider request.
    User {
        /// Message id.
        id: MessageId,
        /// Content parts (text, media, etc.).
        parts: Vec<Part>,
    },
    /// Assistant message with agent/model metadata and optional finish/usage.
    Assistant {
        /// Message id.
        id: MessageId,
        /// Agent that produced this message.
        agent: AgentName,
        /// Model route used for this message.
        model: ModelRef,
        /// Content parts (text, reasoning, tools).
        parts: Vec<Part>,
        /// Set when the assistant message is finished.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish: Option<FinishReason>,
        /// Aggregated usage when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<TokenUsage>,
    },
    /// System message (instructions, compact window, injected context).
    System {
        /// Message id.
        id: MessageId,
        /// Full system content string.
        content: String,
    },
}

impl Message {
    /// Id of this message, whatever its role.
    ///
    /// Compaction records its folded range as `MessageId` endpoints, so it needs
    /// a role-agnostic accessor over the transcript slice it is about to fold.
    #[must_use]
    pub const fn id(&self) -> MessageId {
        match self {
            Self::User { id, .. } | Self::Assistant { id, .. } | Self::System { id, .. } => *id,
        }
    }
}

#[cfg(test)]
mod message_id_tests {
    use super::*;

    #[test]
    fn id_returns_the_id_of_every_variant() {
        let user_id = MessageId::new();
        let assistant_id = MessageId::new();
        let system_id = MessageId::new();

        let user = Message::User {
            id: user_id,
            parts: Vec::new(),
        };
        let assistant = Message::Assistant {
            id: assistant_id,
            agent: AgentName::new("build"),
            model: ModelRef::new("m"),
            parts: Vec::new(),
            finish: None,
            tokens: None,
        };
        let system = Message::System {
            id: system_id,
            content: String::new(),
        };

        assert_eq!(user.id(), user_id);
        assert_eq!(assistant.id(), assistant_id);
        assert_eq!(system.id(), system_id);
    }
}
