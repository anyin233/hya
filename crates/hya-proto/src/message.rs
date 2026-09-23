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

/// Why an assistant message was ended by the harness rather than the model
/// (wire: snake_case). Optional context on
/// [`crate::event::Event::MessageFinished`]; the [`FinishReason`] stays the
/// terminal classification (`cancelled` / `error`).
///
/// Unknown values written by a newer build decode as [`FinishCause::Other`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishCause {
    /// A user stopped the turn (client abort, SIGINT on a one-shot run).
    UserCancel,
    /// The process stopped gracefully (end of a one-shot run, SIGTERM,
    /// `serve` shutdown) and drained in-flight turns.
    Shutdown,
    /// The member was stopped because its team lead's turn failed.
    LeaderFailed,
    /// The process died with the turn open; closed by startup crash recovery.
    Interrupted,
    /// The model provider failed the turn.
    ProviderError,
    /// The member's parent archived it (`archive` tool) while it was
    /// mid-turn.
    Archived,
    /// A cause this build does not know.
    #[serde(other)]
    Other,
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
/// # Invariant (normalized by every provider decoder)
///
/// - `input`: uncached prompt tokens. Excludes `cache_read` and `cache_write`,
///   so the whole prompt is `input + cache_read + cache_write`
///   ([`TokenUsage::prompt`]).
/// - `cache_read`: prompt tokens served from the provider's cache.
/// - `cache_write`: prompt tokens written to the provider's cache (cache
///   creation).
/// - `output`: every generated token, **including** thinking.
/// - `reasoning`: thinking tokens, a subset of `output`. Meaningful only when
///   `reasoning_unknown` is false.
/// - `reasoning_unknown`: the provider did not report how many of `output`
///   were thinking tokens (Anthropic). `reasoning` is then 0 and the split of
///   `output` into thinking and visible text is unknown — never estimated.
///
/// Logs written before this invariant carry provider-native values (OpenAI
/// `input` included cached tokens; Google `output` excluded thoughts;
/// Anthropic reported `reasoning: 0`) and no `reasoning_unknown` field.
/// Readers must treat the thinking split of such legacy usage as unknown.
///
/// Decode accepts `prompt`/`completion` aliases for `input`/`output`.
/// [`TokenUsage::merge`] takes the **max** per field (providers re-report
/// cumulative totals); [`TokenUsage::add`] **sums** rounds, which the turn loop
/// uses to build the final `MessageFinished.tokens`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Uncached prompt tokens (serde alias `prompt`).
    #[serde(default, alias = "prompt")]
    pub input: u64,
    /// All generated tokens including thinking (serde alias `completion`).
    #[serde(default, alias = "completion")]
    pub output: u64,
    /// Thinking tokens within `output`, when the provider reports them.
    #[serde(default)]
    pub reasoning: u64,
    /// Prompt tokens read from the provider cache.
    #[serde(default)]
    pub cache_read: u64,
    /// Prompt tokens written to the provider cache.
    #[serde(default)]
    pub cache_write: u64,
    /// The provider did not report the thinking share of `output`.
    ///
    /// Omitted from the wire when false; absent on legacy logs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reasoning_unknown: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

impl TokenUsage {
    /// True when every counter is zero (the `reasoning_unknown` flag is ignored).
    #[must_use]
    pub fn is_zero(self) -> bool {
        self.input == 0
            && self.output == 0
            && self.reasoning == 0
            && self.cache_read == 0
            && self.cache_write == 0
    }

    /// Whole prompt size: `input + cache_read + cache_write`.
    #[must_use]
    pub fn prompt(self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    /// Visible (non-thinking) output, or `None` when the thinking split is unknown.
    #[must_use]
    pub fn visible_output(self) -> Option<u64> {
        (!self.reasoning_unknown).then(|| self.output.saturating_sub(self.reasoning))
    }

    /// Fold another sample by taking the maximum of each counter (not a sum).
    ///
    /// The thinking split stays unknown once any sample reported it unknown.
    pub fn merge(&mut self, other: Self) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.reasoning = self.reasoning.max(other.reasoning);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.reasoning_unknown |= other.reasoning_unknown;
    }

    /// Add another round's usage (saturating sum per counter).
    ///
    /// The sum's thinking split is unknown when either side's is.
    pub fn add(&mut self, other: Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.reasoning = self.reasoning.saturating_add(other.reasoning);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.reasoning_unknown |= other.reasoning_unknown;
    }
}

/// Why a provider call was made, for [`crate::Event::UsageRecorded`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsagePurpose {
    /// One streaming round of an assistant turn.
    Turn,
    /// Automatic session title generation.
    Title,
    /// Summarizer call: compaction ladder summary/handoff, `/compact`, or the
    /// terminal handoff document.
    Compaction,
    /// A purpose written by a newer binary; decodes instead of failing replay.
    #[serde(other)]
    Other,
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
