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

/// Tool-result image data (data URL length, about the base64 length) one
/// request carries.
const MAX_REQUEST_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Which tool-result images one request sends.
pub struct ToolImageReplay<'a> {
    /// The request's model accepts image input (`Capabilities::image_input ==
    /// Some(true)`) on a protocol that carries tool-result images.
    send: bool,
    /// Images older than the newest ones that fit [`MAX_REQUEST_IMAGE_BYTES`],
    /// as (tool part, index in its attachment array).
    aged_out: HashSet<(&'a PartId, usize)>,
}

impl<'a> ToolImageReplay<'a> {
    /// Plan the images of a request over `messages`. Walking from the newest
    /// image back, images are kept until the next would exceed the budget;
    /// it and every older image are aged out. The plan depends only on the
    /// history, so a later request differs only in the images that newer ones
    /// pushed out.
    #[must_use]
    pub fn new(messages: &'a [Message], send: bool) -> Self {
        let mut aged_out = HashSet::new();
        if send {
            let mut used = 0usize;
            let parts = messages
                .iter()
                .rev()
                .filter_map(|message| match message {
                    Message::Assistant { parts, .. } => Some(parts),
                    Message::User { .. } | Message::System { .. } => None,
                })
                .flat_map(|parts| parts.iter().rev());
            for part in parts {
                let Part::Tool {
                    id,
                    state: ToolPartState::Completed { output, .. },
                    ..
                } = part
                else {
                    continue;
                };
                for (index, _, data) in attached_images(output).rev() {
                    used = used.saturating_add(data.len());
                    if used > MAX_REQUEST_IMAGE_BYTES {
                        aged_out.insert((id, index));
                    }
                }
            }
        }
        Self { send, aged_out }
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
            let notice = if self.aged_out.contains(&(part, index)) {
                Some(format!(
                    "\n\n[The image attached to this result ({mime}) is no longer attached: \
                     newer images fill this request's image budget. Read the file again to \
                     see it.]"
                ))
            } else if self.send && accepts(mime, data) {
                None
            } else {
                Some(format!(
                    "\n\n[The image attached to this result ({mime}) is not shown to you. \
                     Inspect it with a program instead (an image library, OCR, a conversion \
                     to text).]"
                ))
            };
            (mime, data, notice)
        })
    }
}

/// Image attachments (`{"mime": "image/…", "url": …}`) of a tool result.
fn attached_images(output: &Value) -> impl DoubleEndedIterator<Item = (usize, &str, &str)> {
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
