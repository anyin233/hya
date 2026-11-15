//! Shared helpers for encoding stored tool parts into provider wire formats.

use std::collections::HashSet;

use hya_proto::{Message, Part, PartId, ToolCallId, ToolPartState};
use serde_json::Value;

pub(crate) fn tool_input(state: &ToolPartState) -> &Value {
    match state {
        ToolPartState::Pending { input }
        | ToolPartState::Running { input }
        | ToolPartState::Completed { input, .. }
        | ToolPartState::Error { input, .. } => input,
    }
}

/// A tool's result as the model of one request receives it.
pub(crate) struct ToolResult<'a> {
    pub(crate) text: String,
    pub(crate) is_error: bool,
    /// Images to send with the result as `(media type, data)`, already
    /// accepted by [`crate::media`].
    pub(crate) images: Vec<(&'a str, &'a str)>,
}

/// Tool-result image data (data URL length, about the base64 length) one
/// request carries.
pub(crate) const MAX_REQUEST_IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Which tool-result images one request sends.
pub(crate) struct ToolImages<'a> {
    /// The request's model accepts image input (`Capabilities::image_input ==
    /// Some(true)`) on a protocol that carries tool-result images.
    send: bool,
    /// Images older than the newest ones that fit [`MAX_REQUEST_IMAGE_BYTES`],
    /// as (tool part, index among its image attachments).
    aged_out: HashSet<(&'a PartId, usize)>,
}

impl<'a> ToolImages<'a> {
    /// Plan the images of a request over `messages`. Walking from the newest
    /// image back, images are kept until the next would exceed the budget;
    /// it and every older image are aged out. The plan depends only on the
    /// history, so a later request differs only in the images that newer ones
    /// pushed out.
    pub(crate) fn new(messages: &'a [Message], send: bool) -> Self {
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
                let images: Vec<_> = attached_images(output).collect();
                for (index, (_, data)) in images.iter().enumerate().rev() {
                    used = used.saturating_add(data.len());
                    if used > MAX_REQUEST_IMAGE_BYTES {
                        aged_out.insert((id, index));
                    }
                }
            }
        }
        Self { send, aged_out }
    }
}

/// Render the stored tool result of tool part `part` for one request.
///
/// The stored result is model-independent: an image attachment either goes
/// with the result or the text says it is not shown, so a session replayed
/// with another model is told the truth. A pending or running state should not
/// reach the encoder, but is handled so request pairing (every call needs a
/// result) never breaks.
pub(crate) fn tool_result<'a>(
    part: &PartId,
    state: &'a ToolPartState,
    images: &ToolImages<'_>,
) -> ToolResult<'a> {
    let (text, is_error) = match state {
        ToolPartState::Completed { output, .. } => {
            let mut text = value_to_text(output);
            let mut sent = Vec::new();
            for (index, (mime, data)) in attached_images(output).enumerate() {
                if images.aged_out.contains(&(part, index)) {
                    text.push_str(&format!(
                        "\n\n[The image attached to this result ({mime}) is no longer attached: \
                         newer images fill this request's image budget. Read the file again to \
                         see it.]"
                    ));
                } else if images.send && crate::media::image_url(mime, data).is_ok() {
                    sent.push((mime, data));
                } else {
                    text.push_str(&format!(
                        "\n\n[The image attached to this result ({mime}) is not shown to you. \
                         Inspect it with a program instead (an image library, OCR, a conversion \
                         to text).]"
                    ));
                }
            }
            // The tool-result cap drops an attachment over its size budget
            // and records that here.
            if output.pointer("/metadata/attachmentsTruncated") == Some(&Value::Bool(true)) {
                text.push_str(
                    "\n\n[An attachment of this result was dropped by the size limit and is not \
                     shown to you. Inspect the file with a program instead.]",
                );
            }
            return ToolResult {
                text,
                is_error: false,
                images: sent,
            };
        }
        ToolPartState::Error { message, value, .. } => {
            (error_value_to_text(message, value.as_ref()), true)
        }
        ToolPartState::Running { .. } | ToolPartState::Pending { .. } => {
            ("(no result)".to_string(), true)
        }
    };
    ToolResult {
        text,
        is_error,
        images: Vec::new(),
    }
}

/// Image attachments (`{"mime": "image/…", "url": …}`) of a tool result.
fn attached_images(output: &Value) -> impl Iterator<Item = (&str, &str)> {
    output
        .get("attachments")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|attachment| {
            let mime = attachment.get("mime")?.as_str()?;
            let data = attachment.get("url")?.as_str()?;
            mime.starts_with("image/").then_some((mime, data))
        })
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::Object(fields) => fields
            .get("output")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| value.to_string()),
        Value::String(text) => text.clone(),
        _ => value.to_string(),
    }
}

fn error_value_to_text(message: &str, value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(_) | Value::Object(_)) => value
            .and_then(|v| serde_json::to_string(v).ok())
            .unwrap_or_else(|| message.to_string()),
        Some(Value::String(s)) => s.clone(),
        Some(value) => value.to_string(),
        None => message.to_string(),
    }
}
pub(crate) struct ToolAsm {
    pub(crate) part: PartId,
    pub(crate) call: ToolCallId,
    pub(crate) name: String,
    pub(crate) args: String,
    pub(crate) started: bool,
    pub(crate) requested: bool,
}

impl ToolAsm {
    pub(crate) fn new() -> Self {
        Self {
            part: PartId::new(),
            call: ToolCallId::new(),
            name: String::new(),
            args: String::new(),
            started: false,
            requested: false,
        }
    }
    pub(crate) fn input(&self) -> Result<Value, crate::ProviderError> {
        serde_json::from_str(&self.args)
            .map_err(|e| crate::ProviderError::Decode(format!("tool {} arguments: {e}", self.name)))
    }

    pub(crate) fn delta(
        &mut self,
        session: hya_proto::SessionId,
        message: hya_proto::MessageId,
        delta: &str,
    ) -> hya_proto::Event {
        self.args.push_str(delta);
        hya_proto::Event::ToolInputDelta {
            session,
            message,
            part: self.part,
            call: self.call,
            name: hya_proto::ToolName::new(&self.name),
            delta: delta.to_string(),
        }
    }
}

/// Walk user text and media in order; each protocol supplies its wire blocks.
pub(crate) fn user_content(
    parts: &[hya_proto::Part],
    text_block: impl Fn(&str) -> Value,
    media_block: impl Fn(&str, &str) -> Result<Value, crate::ProviderError>,
) -> Result<(String, Vec<Value>, bool), crate::ProviderError> {
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut has_media = false;
    for part in parts {
        match part {
            hya_proto::Part::Text { text: value, .. } => {
                text.push_str(value);
                if !value.is_empty() {
                    blocks.push(text_block(value));
                }
            }
            hya_proto::Part::Media {
                media_type, data, ..
            } => {
                has_media = true;
                blocks.push(media_block(media_type, data)?);
            }
            _ => {}
        }
    }
    Ok((text, blocks, has_media))
}

/// Split after a run of tools when the next replayed text/reasoning part begins.
/// Protocol-specific reasoning admission determines whether it is a boundary.
pub(crate) fn assistant_clusters(
    parts: &[hya_proto::Part],
    reasoning_boundary: impl Fn(&hya_proto::Part) -> bool,
) -> Vec<&[hya_proto::Part]> {
    let mut clusters = Vec::new();
    let mut start = 0;
    let mut tools = false;
    for (index, part) in parts.iter().enumerate() {
        let boundary = matches!(part, hya_proto::Part::Text { .. })
            || (matches!(part, hya_proto::Part::Reasoning { .. }) && reasoning_boundary(part));
        if tools && boundary {
            clusters.push(&parts[start..index]);
            start = index;
            tools = false;
        }
        if matches!(part, hya_proto::Part::Tool { .. }) {
            tools = true;
        }
    }
    clusters.push(&parts[start..]);
    clusters
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn plain(state: &ToolPartState) -> (String, bool) {
        let result = tool_result(&PartId::new(), state, &ToolImages::new(&[], false));
        (result.text, result.is_error)
    }

    fn image_result(url: &str, metadata: Value) -> ToolPartState {
        ToolPartState::Completed {
            input: json!({"path": "board.png"}),
            output: json!({
                "title": "board.png",
                "output": "Image file (image/png, 5 bytes).",
                "metadata": metadata,
                "attachments": [{"type": "file", "mime": "image/png", "url": url}],
            }),
            time_ms: 1,
        }
    }

    #[test]
    fn truncated_tool_arguments_are_rejected() {
        let mut tool = ToolAsm::new();
        tool.name = "read".into();
        tool.args = "{\"path\":".into();
        assert!(tool.input().is_err());
    }

    #[test]
    fn completed_object_prefers_string_output() {
        let output = json!({
            "title": "src/main.rs",
            "output": "plain replay text",
            "metadata": {"diff": "presentation-only"},
        });
        let state = ToolPartState::Completed {
            input: json!({"path": "src/main.rs"}),
            output,
            time_ms: 1,
        };
        assert_eq!(plain(&state), ("plain replay text".to_string(), false));
    }

    #[test]
    fn completed_object_without_output_falls_back_to_json() {
        let output = json!({"title": "src/main.rs", "metadata": {"ok": true}});
        let expected = serde_json::to_string(&output).unwrap();
        let state = ToolPartState::Completed {
            input: Value::Null,
            output,
            time_ms: 1,
        };
        assert_eq!(plain(&state), (expected, false));
    }

    #[test]
    fn errors_keep_structured_json_behavior() {
        let value = json!({"output": "do not prefer errors", "type": "input"});
        let expected = serde_json::to_string(&value).unwrap();
        let state = ToolPartState::Error {
            input: Value::Null,
            message: "input: bad value".to_string(),
            value: Some(value),
        };
        assert_eq!(plain(&state), (expected, true));
    }

    #[test]
    fn an_image_goes_with_the_result_only_to_a_model_that_takes_images() {
        let state = image_result("data:image/png;base64,aGVsbG8=", json!({}));

        let shown = tool_result(&PartId::new(), &state, &ToolImages::new(&[], true));
        assert_eq!(shown.text, "Image file (image/png, 5 bytes).");
        assert_eq!(
            shown.images,
            vec![("image/png", "data:image/png;base64,aGVsbG8=")]
        );

        let hidden = tool_result(&PartId::new(), &state, &ToolImages::new(&[], false));
        assert!(hidden.images.is_empty());
        assert!(
            hidden.text.starts_with("Image file (image/png, 5 bytes).")
                && hidden.text.contains("(image/png) is not shown to you"),
            "{}",
            hidden.text
        );
    }

    #[test]
    fn an_image_the_media_check_rejects_is_described_as_not_shown() {
        let state = image_result("data:image/jpeg;base64,aGVsbG8=", json!({}));
        let result = tool_result(&PartId::new(), &state, &ToolImages::new(&[], true));
        assert!(result.images.is_empty());
        assert!(
            result.text.contains("is not shown to you"),
            "{}",
            result.text
        );
    }

    #[test]
    fn a_dropped_attachment_is_described_as_not_shown() {
        let state = ToolPartState::Completed {
            input: json!({"path": "big.png"}),
            output: json!({
                "title": "big.png",
                "output": "Image file (image/png, 900000 bytes).",
                "metadata": {"truncated": true, "attachmentsTruncated": true},
            }),
            time_ms: 1,
        };
        let result = tool_result(&PartId::new(), &state, &ToolImages::new(&[], true));
        assert!(result.images.is_empty());
        assert!(
            result
                .text
                .contains("dropped by the size limit and is not shown to you"),
            "{}",
            result.text
        );
    }
}
