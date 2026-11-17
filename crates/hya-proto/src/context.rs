//! How a transcript is replayed into a request: which reasoning parts a route
//! resends, and the markers compacted context is written behind.
//!
//! Token accounting and the request encoders both read these, so they live
//! here, below the provider routes and the native bundles alike.

use crate::{Message, Part};

/// System-message prefix of compacted context (`HYA_COMPACTED_CONTEXT`).
pub const COMPACT_CONTEXT_MARKER: &str = "HYA_COMPACTED_CONTEXT";

/// System-message prefix written after a successful `POST /responses/compact`.
/// The JSON array that follows is the canonical next input window and must be
/// re-injected verbatim.
pub const RESPONSES_COMPACT_ITEMS_MARKER: &str = "<<<RESPONSES_COMPACT_ITEMS>>>";

/// `provider_data` type marking a reasoning part streamed as chat
/// `delta.reasoning_content`; the chat encoder replays exactly these parts.
pub const REASONING_CONTENT_TYPE: &str = "reasoning_content";

/// Marker written by older chat decoders; historical sessions still replay it.
pub const LEGACY_CHAT_REASONING_MARKER: &str = "openai_chat_reasoning_content";

/// Which reasoning parts a route replays into its next request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReasoningReplayPolicy {
    /// The encoder omits reasoning parts.
    None,
    /// The encoder sends only provider-native reasoning payloads.
    ProviderData,
    /// The encoder sends signed current-turn reasoning text and metadata.
    SignedCurrentTurn,
    /// The chat encoder sends the text of reasoning streamed as
    /// `reasoning_content`, on every turn, as the message's `reasoning_content`.
    ReasoningContent,
}

/// Index of the latest user message that carries text: assistant messages
/// after it form the current turn. Compute once per transcript and pass it to
/// [`ReasoningReplayPolicy::replays`].
#[must_use]
pub fn current_turn_start(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(|message| {
        matches!(message, Message::User { parts, .. }
            if parts.iter().any(|part| matches!(part, Part::Text { text, .. } if !text.is_empty())))
    })
}

impl ReasoningReplayPolicy {
    /// Whether the route's encoder sends `part` (at `message_index` of the full
    /// transcript whose [`current_turn_start`] is `turn_start`). Shared by the
    /// request encoders and token accounting so the two cannot drift.
    #[must_use]
    pub fn replays(self, turn_start: Option<usize>, message_index: usize, part: &Part) -> bool {
        let Part::Reasoning { provider_data, .. } = part else {
            return false;
        };
        match self {
            Self::None => false,
            Self::ProviderData => provider_data.is_some(),
            Self::SignedCurrentTurn => {
                turn_start.is_some_and(|start| message_index > start)
                    && provider_data
                        .as_ref()
                        .is_some_and(is_signed_anthropic_reasoning)
            }
            Self::ReasoningContent => provider_data.as_ref().is_some_and(|data| {
                data.get("type").and_then(serde_json::Value::as_str) == Some(REASONING_CONTENT_TYPE)
                    || data
                        .get(LEGACY_CHAT_REASONING_MARKER)
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
            }),
        }
    }
}

fn is_signed_anthropic_reasoning(data: &serde_json::Value) -> bool {
    match data.get("type").and_then(serde_json::Value::as_str) {
        Some("thinking") => data
            .get("signature")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|signature| !signature.is_empty()),
        Some("redacted_thinking") => data
            .get("data")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        _ => false,
    }
}
