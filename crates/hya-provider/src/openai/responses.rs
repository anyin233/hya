use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use super::OpenAiResponsesDecoder;
use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

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
        let mut input = Vec::new();
        for message in &req.messages {
            let (role, parts) = match message {
                Message::System { content, .. } => {
                    input.push(json!({"role": "system", "content": content}));
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
