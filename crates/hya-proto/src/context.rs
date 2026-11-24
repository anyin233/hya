//! How a transcript is replayed into a request: which reasoning parts a route
//! resends, how tool results are represented, and the markers compacted
//! context is written behind.
//!
//! Token accounting and the request encoders both read these, so they live
//! here, below the provider routes and the native bundles alike.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::Write;

use serde_json::Value;

use crate::{Message, Part, PartId, TodoItem, ToolPartState};

/// Name of the kernel tool that updates the session's completion criteria.
pub const TODO_TOOL: &str = "todo";

/// Prefix of the rendered todo list.
pub const TODO_LIST_HEADER: &str = "Current todos:";

/// Render the full list for a tool result or a compaction marker.
#[must_use]
pub fn render_todos(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return format!("{TODO_LIST_HEADER} (empty)");
    }
    let mut out = TODO_LIST_HEADER.to_string();
    for item in items {
        let _ = write!(
            out,
            "\n- #{} [{}] {}",
            item.id,
            item.status.as_str(),
            item.content
        );
    }
    out
}

const NO_GOAL: &str = "No todo list is recorded. Use the todo tool now to record this task's completion criteria, each a checkable end state, with its current status; the list is kept verbatim through later rewrites.";

const KEPT_RESULTS: &str = "Results recorded in this list are still valid: do not redo that work. Record each further result in the list as you obtain it.";

/// The goal section of a rewrite or compaction marker: the list verbatim, or,
/// when none is recorded, a request to record it now.
#[must_use]
pub fn marker_goal(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return NO_GOAL.to_string();
    }
    format!("{KEPT_RESULTS}\n\n{}", render_todos(items))
}

/// A marker body without its trailing goal section (`\n\n` + [`marker_goal`]),
/// so a summary updated from it does not restate the list or the request.
#[must_use]
pub fn without_marker_goal(body: &str) -> &str {
    [format!("\n\n{TODO_LIST_HEADER}"), format!("\n\n{NO_GOAL}")]
        .iter()
        .filter_map(|section| body.rfind(section.as_str()))
        .max()
        .map_or(body, |at| {
            let kept = &body[..at];
            kept.strip_suffix(KEPT_RESULTS)
                .and_then(|rest| rest.strip_suffix("\n\n"))
                .unwrap_or(kept)
        })
}

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

/// `provider_data` type marking a Google Gemini `thought: true` reasoning part.
pub const GOOGLE_THOUGHT_TYPE: &str = "google_thought";

/// `provider_data` type marking a Google Gemini trailing `thoughtSignature`
/// captured from a non-thought model `text` part.
pub const GOOGLE_TEXT_SIGNATURE_TYPE: &str = "google_text_signature";

/// Normalize a model reference to its bare upstream model id: strip any
/// trailing `#<variant>` suffix and any leading `<provider>/` prefix.
#[must_use]
pub fn bare_model_id(model: &crate::ModelRef) -> &str {
    let s = model.as_str();
    let without_variant = match s.rsplit_once('#') {
        Some((head, variant)) if !variant.is_empty() => head,
        _ => s,
    };
    without_variant
        .rsplit_once('/')
        .map_or(without_variant, |(_, tail)| tail)
}

/// Whether two model references identify the same upstream model, ignoring
/// routing prefixes and `#variant` effort suffixes.
#[must_use]
pub fn same_model_id(a: &crate::ModelRef, b: &crate::ModelRef) -> bool {
    bare_model_id(a) == bare_model_id(b)
}

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
    /// The Google Gemini encoder replays Google thought parts and thought signatures.
    GoogleThought,
}

/// Opening of the notice sent after a reasoning-only model step was cut at a
/// run-deadline stage (docs/kernel.md).
pub const REASONING_DEADLINE_NOTICE_PREFIX: &str =
    "[hya] Reasoning interrupted at budget checkpoint:";

/// Whether `text` is an ephemeral harness notice rendered as a trailing
/// `Message::User` inside an active turn (headless clock/deadline/continuation
/// notices, goal checkpoints, dropped-reasoning notices, or the notice after a
/// reasoning-only step was cut at a deadline stage). These notices
/// belong to the active turn and must not advance [`current_turn_start`].
#[must_use]
pub fn is_harness_transient_notice_text(text: &str) -> bool {
    text.starts_with("[hya] Reasoning replay notice:")
        || text.starts_with(REASONING_DEADLINE_NOTICE_PREFIX)
        || text.starts_with("[hya] Goal checkpoint:")
        || text.starts_with("[hya] Completion check, sent once before this run ends.")
        || text.starts_with("[hya] Your last turn ended with thinking only:")
        || text.strip_prefix("HEADLESS RUN — ").is_some_and(|rest| {
            ["TIME: ", "FINAL ", "clock: "]
                .iter()
                .any(|kind| rest.starts_with(kind))
        })
}

/// Index of the latest user message that carries text: assistant messages
/// after it form the current turn. Compute once per transcript and pass it to
/// [`ReasoningReplayPolicy::replays`].
#[must_use]
pub fn current_turn_start(messages: &[Message]) -> Option<usize> {
    messages.iter().rposition(|message| {
        let Message::User { parts, .. } = message else {
            return false;
        };
        let mut non_empty = false;
        let mut combined = String::new();
        for part in parts {
            if let Part::Text { text, .. } = part
                && !text.is_empty()
            {
                non_empty = true;
                combined.push_str(text);
            }
        }
        non_empty && !is_harness_transient_notice_text(&combined)
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
            Self::GoogleThought => provider_data.as_ref().is_some_and(is_google_reasoning),
        }
    }

    /// Whether replaying prior reasoning expands the next request's reported
    /// prompt token count proportionally to the prior round's reasoning tokens.
    ///
    /// Google Gemini replays compact encrypted `thoughtSignature` tokens and
    /// does not bill prior `thoughtsTokenCount` back into `promptTokenCount`, so
    /// prompt-growth drop detection applies only to `ProviderData` and
    /// `ReasoningContent`.
    #[must_use]
    pub fn expands_next_prompt(self) -> bool {
        matches!(self, Self::ProviderData | Self::ReasoningContent)
    }
}

fn is_google_reasoning(data: &serde_json::Value) -> bool {
    match data.get("type").and_then(serde_json::Value::as_str) {
        Some(t) if t == GOOGLE_THOUGHT_TYPE => true,
        Some(t) if t == GOOGLE_TEXT_SIGNATURE_TYPE => data
            .get("thought_signature")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|sig| !sig.is_empty()),
        _ => false,
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

/// Text of a completed tool result before attachment notices.
#[must_use]
pub fn tool_result_text(value: &Value) -> Cow<'_, str> {
    value
        .as_str()
        .or_else(|| value.get("output").and_then(Value::as_str))
        .map_or_else(|| Cow::Owned(value.to_string()), Cow::Borrowed)
}

/// Notice appended when the tool-result size cap dropped an attachment.
#[must_use]
pub fn tool_attachment_limit_notice(output: &Value) -> Option<&'static str> {
    (output.pointer("/metadata/attachmentsTruncated") == Some(&Value::Bool(true))).then_some(
        "\n\n[An attachment of this result was dropped by the size limit and is not \
         shown to you. Inspect the file with a program instead.]",
    )
}

/// Budget for the images of one round of tool results (data URL length, about
/// the base64 length).
const MAX_ROUND_IMAGE_BYTES: usize = 2 * 1024 * 1024;

/// Which tool-result images one request sends.
///
/// An image is sent only in the request after the round that read it: every
/// request that carries image data is slow and costly, and what the model saw
/// stays in its replayed reasoning and text. The model reads the file again
/// to look again.
pub struct ToolImageReplay<'a> {
    /// The request's model accepts image input (`Capabilities::image_input ==
    /// Some(true)`) on a protocol that carries tool-result images.
    send: bool,
    /// Tool parts of the newest round of results.
    round: HashSet<&'a PartId>,
    /// Images this request attaches, as (tool part, index in its attachment array).
    attached: HashSet<(&'a PartId, usize)>,
}

impl<'a> ToolImageReplay<'a> {
    /// The newest round is the completed tool results among the parts of the
    /// last step of the last assistant message (`last_step_start`); without
    /// step information no image is sent. Its images are attached in call
    /// order while they fit the budget; the first one always is.
    #[must_use]
    pub fn new(messages: &'a [Message], send: bool) -> Self {
        let mut round = HashSet::new();
        let mut attached = HashSet::new();
        let last = messages.iter().rev().find_map(|message| match message {
            Message::Assistant {
                parts,
                last_step_start,
                ..
            } => Some((parts, *last_step_start)),
            Message::User { .. } | Message::System { .. } => None,
        });
        if send && let Some((parts, Some(start))) = last {
            let mut used = 0usize;
            for part in parts.iter().skip(start) {
                let Part::Tool {
                    id,
                    state: ToolPartState::Completed { output, .. },
                    ..
                } = part
                else {
                    continue;
                };
                round.insert(id);
                for (index, _, data) in attached_images(output) {
                    used = used.saturating_add(data.len());
                    if attached.is_empty() || used <= MAX_ROUND_IMAGE_BYTES {
                        attached.insert((id, index));
                    }
                }
            }
        }
        Self {
            send,
            round,
            attached,
        }
    }

    /// Whether this request carries at least one tool-result image attachment.
    #[must_use]
    pub fn has_attached(&self) -> bool {
        !self.attached.is_empty()
    }

    /// Image candidates for a completed result as (MIME type, data, omission notice).
    /// Encoders supply their media check. Estimators can accept candidates without validating data.
    pub fn attachments<'output>(
        &self,
        part: &PartId,
        output: &'output Value,
        accepts: impl Fn(&str, &str) -> bool,
    ) -> impl Iterator<Item = (&'output str, &'output str, Option<String>)> {
        attached_images(output).map(move |(index, mime, data)| {
            let notice = if self.send && self.attached.contains(&(part, index)) {
                (!accepts(mime, data)).then(|| not_shown_notice(mime))
            } else if self.send && self.round.contains(part) {
                Some(format!(
                    "\n\n[The image attached to this result ({mime}) was not shown: this \
                     step's images exceed what one request can carry. Read it again in a \
                     later step, fewer images at a time.]"
                ))
            } else if self.send {
                // Not "was sent to you": an earlier round's image may never have
                // been sent (over that round's budget, or a rejected media type).
                Some(format!(
                    "\n\n[An image is sent to you only in the request right after the call \
                     that produced it; later requests, like this one, carry this note \
                     instead. This is the normal course, not a failure. To look at the \
                     image of this result ({mime}), make the call again (for a file: read \
                     it again).]"
                ))
            } else {
                Some(not_shown_notice(mime))
            };
            (mime, data, notice)
        })
    }
}

fn not_shown_notice(mime: &str) -> String {
    format!(
        "\n\n[The image attached to this result ({mime}) is not shown to you. Inspect it \
         with a program instead (an image library, OCR, a conversion to text).]"
    )
}

/// Image attachments (`{"mime": "image/…", "url": …}`) of a tool result.
fn attached_images(output: &Value) -> impl Iterator<Item = (usize, &str, &str)> {
    output
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flat_map(|attachments| attachments.iter().enumerate())
        .filter_map(|(index, attachment)| {
            let mime = attachment.get("mime")?.as_str()?;
            let data = attachment.get("url")?.as_str()?;
            mime.starts_with("image/").then_some((index, mime, data))
        })
}

#[cfg(test)]
mod tests {
    use super::{
        GOOGLE_TEXT_SIGNATURE_TYPE, GOOGLE_THOUGHT_TYPE, ReasoningReplayPolicy, bare_model_id,
        current_turn_start, is_harness_transient_notice_text, same_model_id,
    };
    use crate::{Message, MessageId, ModelRef, Part, PartId};
    use serde_json::json;

    #[test]
    fn current_turn_start_skips_trailing_harness_transient_notices() {
        let user_msg = |text: &str| Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: text.to_string(),
            }],
        };
        let asst_msg = Message::Assistant {
            id: MessageId::new(),
            agent: crate::AgentName::new("build"),
            model: ModelRef::new("claude-opus-4-7"),
            parts: Vec::new(),
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: None,
        };
        let notices = [
            "HEADLESS RUN — clock: 360s of 3600s used, 3240s left.",
            "HEADLESS RUN — TIME: 10s left.",
            "HEADLESS RUN — FINAL turn.",
            "[hya] Completion check, sent once before this run ends.\n1. Re-read.",
            "[hya] Your last turn ended with thinking only: continue.",
            "[hya] Reasoning replay notice: endpoint dropped prior step reasoning.",
            "[hya] Goal checkpoint: summarize progress.",
            "[hya] Reasoning interrupted at budget checkpoint: act now.",
        ];
        for notice in notices {
            assert!(is_harness_transient_notice_text(notice), "{notice}");
            let transcript = vec![user_msg("real prompt"), asst_msg.clone(), user_msg(notice)];
            assert_eq!(current_turn_start(&transcript), Some(0), "{notice}");
        }
        assert!(!is_harness_transient_notice_text("new user prompt"));
        let multi_turn = vec![
            user_msg("first turn"),
            asst_msg.clone(),
            user_msg("second real turn"),
            asst_msg,
            user_msg("HEADLESS RUN — clock: 60s"),
        ];
        assert_eq!(current_turn_start(&multi_turn), Some(2));
    }

    #[test]
    fn google_thought_replay_and_prompt_expansion_contracts() {
        let thought = Part::Reasoning {
            id: PartId::new(),
            text: "reason".into(),
            provider_data: Some(json!({"type": GOOGLE_THOUGHT_TYPE, "thought_signature": "sig"})),
        };
        let text_sig = Part::Reasoning {
            id: PartId::new(),
            text: String::new(),
            provider_data: Some(json!({
                "type": GOOGLE_TEXT_SIGNATURE_TYPE,
                "thought_signature": "sig"
            })),
        };
        let empty_text_sig = Part::Reasoning {
            id: PartId::new(),
            text: String::new(),
            provider_data: Some(json!({"type": GOOGLE_TEXT_SIGNATURE_TYPE})),
        };
        let foreign = Part::Reasoning {
            id: PartId::new(),
            text: "claude".into(),
            provider_data: Some(json!({"type": "thinking", "signature": "sig"})),
        };
        assert!(ReasoningReplayPolicy::GoogleThought.replays(None, 0, &thought));
        assert!(ReasoningReplayPolicy::GoogleThought.replays(None, 0, &text_sig));
        assert!(!ReasoningReplayPolicy::GoogleThought.replays(None, 0, &empty_text_sig));
        assert!(!ReasoningReplayPolicy::GoogleThought.replays(None, 0, &foreign));

        assert!(ReasoningReplayPolicy::ProviderData.expands_next_prompt());
        assert!(ReasoningReplayPolicy::ReasoningContent.expands_next_prompt());
        assert!(!ReasoningReplayPolicy::GoogleThought.expands_next_prompt());
        assert!(!ReasoningReplayPolicy::SignedCurrentTurn.expands_next_prompt());
        assert!(!ReasoningReplayPolicy::None.expands_next_prompt());
    }

    #[test]
    fn bare_model_id_and_same_model_id_normalize_routes_and_variants() {
        assert_eq!(
            bare_model_id(&ModelRef::new("anthropic/claude-opus-4-7#high")),
            "claude-opus-4-7"
        );
        assert_eq!(bare_model_id(&ModelRef::new("gpt-5.6-sol")), "gpt-5.6-sol");
        assert!(same_model_id(
            &ModelRef::new("claude-opus-4-7"),
            &ModelRef::new("anthropic/claude-opus-4-7#high"),
        ));
        assert!(same_model_id(
            &ModelRef::new("openai/gpt-5.6-sol#xhigh"),
            &ModelRef::new("12th/gpt-5.6-sol#medium"),
        ));
        assert!(!same_model_id(
            &ModelRef::new("claude-opus-4-7"),
            &ModelRef::new("claude-sonnet-4-6"),
        ));
    }
}
