use serde_json::{Value, json};
use yaca_proto::{Message, MessageId, Part, SessionId};

use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

mod decoder;

pub use decoder::AnthropicDecoder;

pub struct AnthropicMessagesProtocol;

impl Protocol for AnthropicMessagesProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut messages: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m {
                Message::User { parts, .. } => {
                    messages.push(json!({"role": "user", "content": parts_text(parts)?}));
                }
                Message::Assistant { parts, .. } => emit_assistant(&mut messages, parts),
                Message::System { .. } => {}
            }
        }
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name.as_str(),
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        let mut max_tokens = req.max_output_tokens.unwrap_or(4096);
        let mut body = json!({
            "model": req.model.as_str(),
            "messages": messages,
            "stream": true,
        });
        if let Some(budget) = req.reasoning.and_then(|e| e.anthropic_budget()) {
            if max_tokens <= budget {
                max_tokens = budget + 4096;
            }
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
        }
        body["max_tokens"] = json!(max_tokens);
        if let Some(system) = &req.system {
            body["system"] = json!(system);
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        Ok(body)
    }

    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder> {
        Box::new(AnthropicDecoder::new(session, message))
    }
}

fn parts_text(parts: &[Part]) -> Result<String, ProviderError> {
    let mut s = String::new();
    for p in parts {
        match p {
            Part::Text { text, .. } => s.push_str(text),
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "Anthropic messages does not support media type {media_type}"
                )));
            }
            Part::Reasoning { .. } | Part::Tool { .. } => {}
        }
    }
    Ok(s)
}

// Anthropic puts tool_use blocks in the assistant message and the matching
// tool_result blocks in the FOLLOWING user message. Segment each `[text?, tool+]`
// cluster into that pair; trailing text becomes a final assistant text message.
fn emit_assistant(out: &mut Vec<Value>, parts: &[Part]) {
    let mut text = String::new();
    let mut tools: Vec<&Part> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text: t, .. } => {
                if !tools.is_empty() {
                    flush_cluster(out, &text, &tools);
                    text.clear();
                    tools.clear();
                }
                text.push_str(t);
            }
            Part::Tool { .. } => tools.push(part),
            Part::Reasoning { .. } => {}
            Part::Media { .. } => {}
        }
    }
    if tools.is_empty() {
        if !text.is_empty() {
            out.push(json!({"role": "assistant", "content": [{"type": "text", "text": text}]}));
        }
    } else {
        flush_cluster(out, &text, &tools);
    }
}

fn flush_cluster(out: &mut Vec<Value>, text: &str, tools: &[&Part]) {
    let mut content: Vec<Value> = Vec::new();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    for &p in tools {
        if let Part::Tool {
            call_id,
            name,
            state,
            ..
        } = p
        {
            let input = tool_input(state);
            let input_obj = if input.is_null() {
                json!({})
            } else {
                input.clone()
            };
            content.push(json!({
                "type": "tool_use",
                "id": call_id.to_string(),
                "name": name.as_str(),
                "input": input_obj,
            }));
        }
    }
    out.push(json!({"role": "assistant", "content": content}));
    let results: Vec<Value> = tools
        .iter()
        .filter_map(|&p| {
            let Part::Tool { call_id, state, .. } = p else {
                return None;
            };
            let (result, is_error) = tool_result(state);
            Some(json!({
                "type": "tool_result",
                "tool_use_id": call_id.to_string(),
                "content": result,
                "is_error": is_error,
            }))
        })
        .collect();
    out.push(json!({"role": "user", "content": results}));
}
