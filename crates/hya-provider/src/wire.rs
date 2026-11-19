//! Shared helpers for encoding stored tool parts into provider wire formats.

pub(crate) use hya_proto::context::ToolImageReplay as ToolImages;
use hya_proto::context::{tool_attachment_limit_notice, tool_result_text};
use hya_proto::{PartId, ToolCallId, ToolPartState};
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
            let mut text = tool_result_text(output).into_owned();
            let mut sent = Vec::new();
            for (mime, data, notice) in images.attachments(part, output, |mime, data| {
                crate::media::image_url(mime, data).is_ok()
            }) {
                if let Some(notice) = notice {
                    text.push_str(&notice);
                } else {
                    sent.push((mime, data));
                }
            }
            if let Some(notice) = tool_attachment_limit_notice(output) {
                text.push_str(notice);
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

    /// Render `state` as the newest round's only result.
    fn newest(state: &ToolPartState, send: bool) -> ToolResult<'_> {
        let id = PartId::new();
        let messages = [hya_proto::Message::Assistant {
            id: hya_proto::MessageId::new(),
            agent: "agent".into(),
            model: "model".into(),
            parts: vec![hya_proto::Part::Tool {
                id,
                call_id: ToolCallId::new(),
                name: "read".into(),
                state: state.clone(),
            }],
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: Some(0),
        }];
        tool_result(&id, state, &ToolImages::new(&messages, send))
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

        let shown = newest(&state, true);
        assert_eq!(shown.text, "Image file (image/png, 5 bytes).");
        assert_eq!(
            shown.images,
            vec![("image/png", "data:image/png;base64,aGVsbG8=")]
        );

        let hidden = newest(&state, false);
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
        let result = newest(&state, true);
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
