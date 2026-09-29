use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use crate::media::image_url;
use crate::wire::{tool_input, tool_result};
use crate::{
    CompletionRequest, Decoder, Protocol, ProviderError, ReasoningEffort, ReasoningReplayPolicy,
};

mod decoder;
mod response_decoder;
mod responses;

pub use decoder::OpenAiChatDecoder;
pub(crate) use decoder::REASONING_CONTENT_TYPE;
pub use response_decoder::OpenAiResponsesDecoder;
pub(crate) use responses::GrokBuildProtocol;
pub use responses::{
    COMPACT_CONTEXT_MARKER, OpenAiResponsesProtocol, RESPONSES_COMPACT_ITEMS_MARKER,
    encode_input_items, format_responses_compact_system, parse_responses_compact_items,
};

/// OpenAI Chat Completions request encoder + SSE decoder factory.
pub struct OpenAiChatProtocol;

impl Protocol for OpenAiChatProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut messages = Vec::new();
        // Thinking-mode routes (DeepSeek, Kimi) reject a later request whose
        // assistant message lacks `reasoning_content`, so once the transcript
        // holds chat-native reasoning every assistant message carries it.
        // `deepseek-*` models require it even before any reasoning streamed
        // (a tool turn whose reply had no reasoning delta).
        let deepseek = req
            .model
            .as_str()
            .rsplit('/')
            .next()
            .is_some_and(|model| model.starts_with("deepseek-"));
        let thinking = deepseek
            || req.messages.iter().any(|m| {
                matches!(m, Message::Assistant { parts, .. }
                    if parts.iter().any(replays_reasoning))
            });
        if let Some(system) = &req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        for m in &req.messages {
            match m {
                Message::System { content, .. } => {
                    messages.push(json!({"role": "system", "content": content}));
                }
                Message::User { parts, .. } => {
                    messages.push(json!({
                        "role": "user",
                        "content": user_content(parts)?,
                    }));
                }
                Message::Assistant { parts, .. } => {
                    emit_assistant(&mut messages, parts, thinking)?;
                }
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
            "stream_options": { "include_usage": true },
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
        if let Some(label) = req
            .reasoning
            .and_then(|e| e.openai_label(req.model.as_str()))
        {
            body["reasoning_effort"] = json!(label);
        }
        Ok(body)
    }

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        _reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        Box::new(OpenAiChatDecoder::new(session, message))
    }
}

fn user_content(parts: &[Part]) -> Result<Value, ProviderError> {
    let mut text = String::new();
    let mut content = Vec::new();
    let mut has_media = false;
    for part in parts {
        match part {
            Part::Text { text: part, .. } => {
                text.push_str(part);
                if !part.is_empty() {
                    content.push(json!({"type": "text", "text": part}));
                }
            }
            Part::Media {
                media_type, data, ..
            } => {
                has_media = true;
                content.push(json!({
                    "type": "image_url",
                    "image_url": {"url": image_url(media_type, data)?},
                }));
            }
            Part::Reasoning { .. } | Part::Tool { .. } => {}
        }
    }
    if has_media {
        Ok(Value::Array(content))
    } else {
        Ok(Value::String(text))
    }
}

fn replays_reasoning(part: &Part) -> bool {
    ReasoningReplayPolicy::ReasoningContent.replays(None, 0, part)
}

/// One wire assistant message: `reasoning_content` + `content` + `tool_calls`.
#[derive(Default)]
struct Cluster<'a> {
    reasoning: String,
    text: String,
    tools: Vec<&'a Part>,
}

// Split an assistant message into wire messages: each `[reasoning?, text?,
// tool_call+]` cluster becomes `assistant(reasoning_content, content,
// tool_calls)` followed by its `role:tool` results, and any trailing text
// becomes a final tool-free assistant message. This keeps tool results paired
// with their calls (OpenAI requires it) without scrambling order.
fn emit_assistant(
    out: &mut Vec<Value>,
    parts: &[Part],
    thinking: bool,
) -> Result<(), ProviderError> {
    let mut cluster = Cluster::default();
    for part in parts {
        match part {
            Part::Text { text, .. } => {
                if !cluster.tools.is_empty() {
                    flush_cluster(out, &std::mem::take(&mut cluster), thinking);
                }
                cluster.text.push_str(text);
            }
            Part::Tool { .. } => cluster.tools.push(part),
            Part::Reasoning { text, .. } if replays_reasoning(part) => {
                if !cluster.tools.is_empty() {
                    flush_cluster(out, &std::mem::take(&mut cluster), thinking);
                }
                cluster.reasoning.push_str(text);
            }
            Part::Reasoning { .. } => {}
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "OpenAI chat does not support assistant media type {media_type}"
                )));
            }
        }
    }
    if !cluster.tools.is_empty() || !cluster.text.is_empty() {
        flush_cluster(out, &cluster, thinking);
    }
    Ok(())
}

fn flush_cluster(out: &mut Vec<Value>, cluster: &Cluster<'_>, thinking: bool) {
    let content = if cluster.text.is_empty() {
        Value::Null
    } else {
        json!(cluster.text)
    };
    let mut message = json!({"role": "assistant", "content": content});
    if thinking {
        message["reasoning_content"] = json!(cluster.reasoning);
    }
    if cluster.tools.is_empty() {
        out.push(message);
        return;
    }
    let tool_calls: Vec<Value> = cluster
        .tools
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
    message["tool_calls"] = Value::Array(tool_calls);
    out.push(message);
    for &p in &cluster.tools {
        if let Part::Tool { call_id, state, .. } = p {
            let (result, _is_error) = tool_result(state);
            out.push(
                json!({"role": "tool", "tool_call_id": call_id.to_string(), "content": result}),
            );
        }
    }
}
