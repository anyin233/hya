use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use super::OpenAiResponsesDecoder;
use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

/// System-message prefix written by the engine after a successful
/// `POST /responses/compact`. The JSON array that follows is the canonical
/// next input window and must be re-injected verbatim.
pub const RESPONSES_COMPACT_ITEMS_MARKER: &str = "<<<RESPONSES_COMPACT_ITEMS>>>";

/// Shared with `hya-core` compaction injects (`HYA_COMPACTED_CONTEXT`).
pub const COMPACT_CONTEXT_MARKER: &str = "HYA_COMPACTED_CONTEXT";

pub struct OpenAiResponsesProtocol;

pub(crate) struct GrokBuildProtocol;

impl Protocol for GrokBuildProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut body = OpenAiResponsesProtocol.encode(req)?;
        body["include"] = json!(["reasoning.encrypted_content"]);
        Ok(body)
    }

    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder> {
        Box::new(OpenAiResponsesDecoder::new_requiring_typed_terminal(
            session, message,
        ))
    }
}

impl Protocol for OpenAiResponsesProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let input = encode_input_items(&req.messages)?;
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
        }
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_output_tokens) = req.max_output_tokens {
            body["max_output_tokens"] = json!(max_output_tokens);
        }
        Ok(body)
    }

    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder> {
        Box::new(OpenAiResponsesDecoder::new(session, message))
    }
}

/// Build the Responses API `input` item array from hya messages.
///
/// Used by both `/responses` create and `/responses/compact`.
pub fn encode_input_items(messages: &[Message]) -> Result<Vec<Value>, ProviderError> {
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
            Message::Assistant { parts, .. } => {
                emit_assistant(&mut input, parts)?;
                continue;
            }
        };
        let content = text(parts)?;
        if !content.is_empty() {
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

/// Extract the compact-item array from a system message body, if present.
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
    let value: Value = serde_json::from_str(json_text).map_err(|e| {
        ProviderError::Decode(format!("invalid responses compact window payload: {e}"))
    })?;
    match value {
        Value::Array(items) => Ok(Some(items)),
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

fn emit_assistant(out: &mut Vec<Value>, parts: &[Part]) -> Result<(), ProviderError> {
    let mut text = String::new();
    for part in parts {
        match part {
            Part::Text { text: part, .. } => text.push_str(part),
            Part::Reasoning { provider_data, .. } => {
                flush_text(out, &mut text);
                if let Some(data) = provider_data {
                    out.push(data.clone());
                }
            }
            Part::Tool {
                call_id,
                name,
                state,
                ..
            } => {
                flush_text(out, &mut text);
                let input = tool_input(state);
                let arguments = if input.is_null() {
                    "{}".to_string()
                } else {
                    serde_json::to_string(input)?
                };
                out.push(json!({
                    "type": "function_call",
                    "call_id": call_id.to_string(),
                    "name": name.as_str(),
                    "arguments": arguments,
                }));
                let (output, _is_error) = tool_result(state);
                out.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id.to_string(),
                    "output": output,
                }));
            }
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "OpenAI Responses does not support media type {media_type}"
                )));
            }
        }
    }
    flush_text(out, &mut text);
    Ok(())
}

fn flush_text(out: &mut Vec<Value>, text: &mut String) {
    if !text.is_empty() {
        out.push(json!({"role": "assistant", "content": text}));
        text.clear();
    }
}

fn text(parts: &[Part]) -> Result<String, ProviderError> {
    let mut text = String::new();
    for part in parts {
        match part {
            Part::Text { text: part, .. } => text.push_str(part),
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "OpenAI Responses does not support media type {media_type}"
                )));
            }
            Part::Reasoning { .. } | Part::Tool { .. } => {}
        }
    }
    Ok(text)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::{MessageId, PartId};

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
        let input = encode_input_items(&messages).unwrap();
        assert_eq!(input.len(), 3);
        assert_eq!(input[0], items[0]);
        assert_eq!(input[1], items[1]);
        assert_eq!(input[2]["role"], "user");
        assert_eq!(input[2]["content"], "next turn");
    }

    #[test]
    fn plain_system_message_still_encoded_as_system() {
        let messages = vec![Message::System {
            id: MessageId::new(),
            content: "HYA_COMPACTED_CONTEXT\nsummary only".into(),
        }];
        let input = encode_input_items(&messages).unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "system");
    }
}
