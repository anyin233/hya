use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use super::OpenAiResponsesDecoder;
use crate::media::image_url;
use crate::wire::{ToolImages, tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError, ReasoningEffort};

use hya_proto::context::TODO_LIST_HEADER;
pub use hya_proto::context::{COMPACT_CONTEXT_MARKER, RESPONSES_COMPACT_ITEMS_MARKER};

/// OpenAI Responses API request encoder + stream decoder factory.
pub struct OpenAiResponsesProtocol;

pub(crate) struct GrokBuildProtocol;

impl Protocol for GrokBuildProtocol {
    fn endpoint_url(&self, base: &str, model: &str) -> String {
        OpenAiResponsesProtocol.endpoint_url(base, model)
    }

    fn compact_endpoint(&self, base: &str) -> Option<String> {
        OpenAiResponsesProtocol.compact_endpoint(base)
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        OpenAiResponsesProtocol.reasoning_replay_policy()
    }

    fn reasoning_variant_labels(&self) -> &'static [&'static str] {
        &["low", "medium", "high"]
    }

    fn session_model_override<'a>(&self, model: &'a str) -> Option<&'a str> {
        Some(model)
    }
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        self.encode_for_model(req, None, false)
    }

    fn encode_for_model(
        &self,
        req: &CompletionRequest,
        output_limit: Option<u32>,
        images: bool,
    ) -> Result<Value, ProviderError> {
        let mut body = OpenAiResponsesProtocol.encode_for_model(req, output_limit, images)?;
        body["include"] = json!(["reasoning.encrypted_content"]);
        Ok(body)
    }

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        Box::new(OpenAiResponsesDecoder::new(session, message, reasoning))
    }
}

impl Protocol for OpenAiResponsesProtocol {
    fn endpoint_url(&self, base: &str, _model: &str) -> String {
        format!("{base}/responses")
    }

    fn compact_endpoint(&self, base: &str) -> Option<String> {
        Some(format!("{base}/responses/compact"))
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        crate::ReasoningReplayPolicy::ProviderData
    }

    fn reasoning_variant_labels(&self) -> &'static [&'static str] {
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"]
    }
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        self.encode_for_model(req, None, false)
    }

    fn encode_for_model(
        &self,
        req: &CompletionRequest,
        _output_limit: Option<u32>,
        images: bool,
    ) -> Result<Value, ProviderError> {
        let input = encode_input_items_for_model(&req.messages, images, Some(&req.model))?;
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name.as_str(),
                    "description": tool.description,
                    "parameters": tool.input_schema,
                })
            })
            .collect();
        let mut body = json!({
            "model": req.model.as_str(),
            "input": input,
            "tools": tools,
            "stream": true,
            "store": false,
        });
        if let Some(instructions) = &req.system {
            body["instructions"] = json!(instructions);
        }
        if let Some(reasoning) = req.reasoning {
            body["reasoning"] = json!({
                "effort": reasoning.as_str(),
                "summary": "auto",
            });
            // Stateless (`store: false`) replay needs the encrypted item;
            // non-reasoning models reject the include, so `Off` omits it.
            if reasoning != ReasoningEffort::Off {
                body["include"] = json!(["reasoning.encrypted_content"]);
            }
        }
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_output_tokens) = req.max_output_tokens {
            body["max_output_tokens"] = json!(max_output_tokens);
        }
        Ok(body)
    }

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        Box::new(OpenAiResponsesDecoder::new(session, message, reasoning))
    }
}

/// Build the Responses API `input` item array from hya messages. `images`:
/// the model accepts image input, so tool-result images are sent (see
/// [`crate::Protocol::encode_for_model`]).
///
/// Used by both `/responses` create and `/responses/compact`.
pub fn encode_input_items(messages: &[Message], images: bool) -> Result<Vec<Value>, ProviderError> {
    encode_input_items_for_model(messages, images, None)
}

pub(crate) fn encode_input_items_for_model(
    messages: &[Message],
    images: bool,
    target_model: Option<&hya_proto::ModelRef>,
) -> Result<Vec<Value>, ProviderError> {
    let images = ToolImages::new(messages, images);
    let mut input = Vec::new();
    for message in messages {
        let (role, parts) = match message {
            Message::System { content, .. } => {
                if let Some(items) = parse_responses_compact_items(content)? {
                    input.extend(items);
                } else {
                    input.push(json!({"role": "system", "content": content}));
                }
                continue;
            }
            Message::User { parts, .. } => ("user", parts),
            Message::Assistant { model, parts, .. } => {
                let same_model = target_model
                    .is_none_or(|target| hya_proto::context::same_model_id(model, target));
                emit_assistant(&mut input, parts, same_model, &images)?;
                continue;
            }
        };
        if let Some(content) = user_content(parts)? {
            input.push(json!({"role": role, "content": content}));
        }
    }
    Ok(input)
}

/// Format a system message body that carries a compact window for re-injection.
#[must_use]
pub fn format_responses_compact_system(items: &[Value]) -> String {
    format!(
        "{COMPACT_CONTEXT_MARKER}\n{RESPONSES_COMPACT_ITEMS_MARKER}\n{}",
        Value::Array(items.to_vec())
    )
}

/// Extract the compact-item array and append any carried todo list as a
/// system item.
pub fn parse_responses_compact_items(content: &str) -> Result<Option<Vec<Value>>, ProviderError> {
    let Some(rest) = content
        .strip_prefix(COMPACT_CONTEXT_MARKER)
        .map(str::trim_start)
    else {
        return Ok(None);
    };
    let Some(json_text) = rest
        .strip_prefix(RESPONSES_COMPACT_ITEMS_MARKER)
        .map(str::trim_start)
    else {
        // Local-summarizer compact (text summary only) — not a Responses window.
        return Ok(None);
    };
    // Native windows store compact JSON on one line. A fold can append
    // the todo list verbatim after that array.
    let (json_text, todos) = match json_text.split_once("\n\n") {
        Some((json, todos)) if todos.starts_with(TODO_LIST_HEADER) => (json, Some(todos)),
        _ => (json_text, None),
    };
    let value: Value = serde_json::from_str(json_text).map_err(|e| {
        ProviderError::Decode(format!("invalid responses compact window payload: {e}"))
    })?;
    match value {
        Value::Array(mut items) => {
            if let Some(todos) = todos {
                items.push(json!({"role": "system", "content": todos}));
            }
            Ok(Some(items))
        }
        other => Err(ProviderError::Decode(format!(
            "responses compact window must be a JSON array, got {}",
            other_kind(&other)
        ))),
    }
}

fn other_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn emit_assistant(
    out: &mut Vec<Value>,
    parts: &[Part],
    same_model: bool,
    images: &ToolImages<'_>,
) -> Result<(), ProviderError> {
    let mut text = String::new();
    // The API rejects a reasoning item without the answer or call that
    // followed it, as after a step stopped while thinking: such an item is
    // left out (a later one replaces it), and the transcript keeps it.
    let mut reasoning = None;
    for part in parts {
        match part {
            Part::Text { text: part, .. } => text.push_str(part),
            Part::Reasoning { provider_data, .. } => {
                flush_text(out, &mut reasoning, &mut text);
                reasoning = provider_data
                    .as_ref()
                    .filter(|data| {
                        same_model && data.get("type").and_then(Value::as_str) == Some("reasoning")
                    })
                    .cloned();
            }
            Part::Tool {
                id,
                call_id,
                name,
                state,
                provider_data,
            } => {
                flush_text(out, &mut reasoning, &mut text);
                out.extend(reasoning.take());
                let input = tool_input(state);
                let arguments = if input.is_null() {
                    "{}".to_string()
                } else {
                    serde_json::to_string(input)?
                };
                let wire_id =
                    crate::wire::wire_tool_call_id(call_id, provider_data.as_ref(), same_model);
                out.push(json!({
                    "type": "function_call",
                    "call_id": wire_id.as_ref(),
                    "name": name.as_str(),
                    "arguments": arguments,
                }));
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": wire_id.as_ref(),
                    "output": function_call_output(tool_result(id, state, images))?,
                }));
            }
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "OpenAI Responses does not support media type {media_type}"
                )));
            }
        }
    }
    flush_text(out, &mut reasoning, &mut text);
    Ok(())
}

/// A result without images stays a string; with images `output` is a
/// content list of `input_text` and `input_image` items.
fn function_call_output(result: crate::wire::ToolResult<'_>) -> Result<Value, ProviderError> {
    if result.images.is_empty() {
        return Ok(Value::String(result.text));
    }
    let mut content = Vec::new();
    if !result.text.is_empty() {
        content.push(json!({"type": "input_text", "text": result.text}));
    }
    for (mime, data) in result.images {
        content.push(json!({"type": "input_image", "image_url": image_url(mime, data)?}));
    }
    Ok(Value::Array(content))
}

/// Push the pending text, after the reasoning item it answers.
fn flush_text(out: &mut Vec<Value>, reasoning: &mut Option<Value>, text: &mut String) {
    if !text.is_empty() {
        out.extend(reasoning.take());
        out.push(json!({"role": "assistant", "content": text}));
        text.clear();
    }
}

fn user_content(parts: &[Part]) -> Result<Option<Value>, ProviderError> {
    let mut text = String::new();
    let mut content = Vec::new();
    let mut has_media = false;
    for part in parts {
        match part {
            Part::Text { text: part, .. } => {
                text.push_str(part);
                if !part.is_empty() {
                    content.push(json!({"type": "input_text", "text": part}));
                }
            }
            Part::Media {
                media_type, data, ..
            } => {
                has_media = true;
                content.push(json!({
                    "type": "input_image",
                    "image_url": image_url(media_type, data)?,
                }));
            }
            Part::Reasoning { .. } | Part::Tool { .. } => {}
        }
    }
    if has_media {
        Ok(Some(Value::Array(content)))
    } else if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::String(text)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::{MessageId, PartId};

    #[test]
    fn native_compaction_preserves_the_response_items_and_carried_todo_text() {
        let item = json!({"type": "compaction", "encrypted_content": "opaque"});
        let list = "Current todos:\n- #1 [pending] quote \"this\"\n\nsecond line";
        let content = format!(
            "{}\n\n{list}",
            format_responses_compact_system(std::slice::from_ref(&item))
        );
        let messages = [Message::System {
            id: MessageId::new(),
            content,
        }];
        assert_eq!(
            encode_input_items(&messages, false).unwrap(),
            [item, json!({"role": "system", "content": list})]
        );
    }

    #[test]
    fn encode_expands_compact_items_marker_verbatim() {
        let items = vec![
            json!({"type": "compaction", "encrypted_content": "abc"}),
            json!({"role": "user", "content": "continue"}),
        ];
        let system = format_responses_compact_system(&items);
        let messages = vec![
            Message::System {
                id: MessageId::new(),
                content: system,
            },
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "next turn".into(),
                }],
            },
        ];
        let input = encode_input_items(&messages, false).unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0], items[0]);
        assert_eq!(input[1], items[1]);
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"], "next turn");
    }

    /// A reasoning item goes out only paired with the answer or call that
    /// followed it; one left alone by a stopped step stays out.
    #[test]
    fn reasoning_items_are_sent_only_with_what_followed_them() {
        let rs = |id: &str| json!({"id": id, "type": "reasoning", "encrypted_content": "opaque"});
        let reasoning = |id: &str| Part::Reasoning {
            id: PartId::new(),
            text: "thought".into(),
            provider_data: Some(rs(id)),
        };
        let text = |body: &str| Part::Text {
            id: PartId::new(),
            text: body.into(),
        };
        let call = Part::Tool {
            id: PartId::new(),
            call_id: hya_proto::ToolCallId::new(),
            name: hya_proto::ToolName::new("read"),
            state: hya_proto::ToolPartState::Completed {
                input: json!({"path": "a.txt"}),
                output: json!("contents"),
                time_ms: 1,
            },
            provider_data: None,
        };
        let assistant = |parts| Message::Assistant {
            id: MessageId::new(),
            agent: hya_proto::AgentName::new("build"),
            model: hya_proto::ModelRef::new("model"),
            parts,
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: None,
        };
        let user = Message::User {
            id: MessageId::new(),
            parts: vec![text("notice")],
        };
        let kinds = |input: &[Value]| -> Vec<String> {
            input
                .iter()
                .map(|item| {
                    item.get("id")
                        .or_else(|| item.get("type"))
                        .or_else(|| item.get("role"))
                        .and_then(Value::as_str)
                        .unwrap()
                        .to_string()
                })
                .collect()
        };

        let alone =
            encode_input_items(&[assistant(vec![reasoning("rs_cut")]), user], false).unwrap();
        assert_eq!(alone, [json!({"role": "user", "content": "notice"})]);

        let replaced = encode_input_items(
            &[assistant(vec![
                reasoning("rs_cut"),
                reasoning("rs_next"),
                call.clone(),
            ])],
            false,
        )
        .unwrap();
        assert_eq!(
            kinds(&replaced),
            ["rs_next", "function_call", "function_call_output"]
        );

        let paired = encode_input_items(
            &[assistant(vec![
                reasoning("rs_1"),
                text("answer"),
                reasoning("rs_2"),
                call,
            ])],
            false,
        )
        .unwrap();
        assert_eq!(
            kinds(&paired),
            [
                "rs_1",
                "assistant",
                "rs_2",
                "function_call",
                "function_call_output"
            ]
        );
        assert_eq!(paired[0], rs("rs_1"));
        assert_eq!(paired[2], rs("rs_2"));
    }

    #[test]
    fn plain_system_message_still_encoded_as_system() {
        let messages = vec![Message::System {
            id: MessageId::new(),
            content: "HYA_COMPACTED_CONTEXT\nsummary only".into(),
        }];
        let input = encode_input_items(&messages, false).unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "system");
    }
}
