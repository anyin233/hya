use serde_json::{Value, json};
use yaca_proto::{Message, MessageId, Part, SessionId};

use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

mod decoder;

pub use decoder::OpenAiChatDecoder;

pub struct OpenAiChatProtocol;

impl Protocol for OpenAiChatProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut messages = Vec::new();
        if let Some(system) = &req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        for m in &req.messages {
            match m {
                Message::System { content, .. } => {
                    messages.push(json!({"role": "system", "content": content}));
                }
                Message::User { parts, .. } => {
                    messages.push(json!({"role": "user", "content": parts_text(parts)}));
                }
                Message::Assistant { parts, .. } => emit_assistant(&mut messages, parts),
            }
        }
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name.as_str(),
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        let mut body = json!({
            "model": req.model.as_str(),
            "messages": messages,
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_output_tokens {
            body["max_tokens"] = json!(m);
        }
        if let Some(effort) = req.reasoning {
            body["reasoning_effort"] = json!(effort.as_str());
        }
        Ok(body)
    }

    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder> {
        Box::new(OpenAiChatDecoder::new(session, message))
    }
}

fn parts_text(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        if let Part::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

// Split an assistant message into wire messages: each `[text?, tool_call+]` cluster
// becomes `assistant(content, tool_calls)` followed by its `role:tool` results, and
// any trailing text becomes a final tool-free assistant message. This keeps tool
// results paired with their calls (OpenAI requires it) without scrambling order.
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
        }
    }
    if tools.is_empty() {
        if !text.is_empty() {
            out.push(json!({"role": "assistant", "content": text}));
        }
    } else {
        flush_cluster(out, &text, &tools);
    }
}

fn flush_cluster(out: &mut Vec<Value>, text: &str, tools: &[&Part]) {
    let tool_calls: Vec<Value> = tools
        .iter()
        .filter_map(|&p| {
            let Part::Tool {
                call_id,
                name,
                state,
                ..
            } = p
            else {
                return None;
            };
            let input = tool_input(state);
            let arguments = if input.is_null() {
                "{}".to_string()
            } else {
                serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
            };
            Some(json!({
                "id": call_id.to_string(),
                "type": "function",
                "function": { "name": name.as_str(), "arguments": arguments },
            }))
        })
        .collect();
    let content = if text.is_empty() {
        Value::Null
    } else {
        json!(text)
    };
    out.push(json!({"role": "assistant", "content": content, "tool_calls": tool_calls}));
    for &p in tools {
        if let Part::Tool { call_id, state, .. } = p {
            let (result, _is_error) = tool_result(state);
            out.push(
                json!({"role": "tool", "tool_call_id": call_id.to_string(), "content": result}),
            );
        }
    }
}
