use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use crate::media::image_url;
use crate::wire::{ToolImages, tool_input, tool_result};
use crate::{
    CompletionRequest, Decoder, Protocol, ProviderError, ReasoningEffort, ReasoningReplayPolicy,
};

mod decoder;
mod response_decoder;
mod responses;

pub use decoder::OpenAiChatDecoder;
pub use response_decoder::OpenAiResponsesDecoder;
pub(crate) use responses::GrokBuildProtocol;
pub(crate) use responses::encode_input_items_for_model;
pub use responses::{
    COMPACT_CONTEXT_MARKER, OpenAiResponsesProtocol, RESPONSES_COMPACT_ITEMS_MARKER,
    encode_input_items, format_responses_compact_system, parse_responses_compact_items,
};

/// OpenAI Chat Completions request encoder + SSE decoder factory.
pub struct OpenAiChatProtocol;

impl Protocol for OpenAiChatProtocol {
    fn endpoint_url(&self, base: &str, _model: &str) -> String {
        format!("{base}/chat/completions")
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        crate::ReasoningReplayPolicy::ReasoningContent
    }

    fn reasoning_variant_labels(&self) -> &'static [&'static str] {
        &["minimal", "low", "medium", "high", "xhigh"]
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
        let images = ToolImages::new(&req.messages, images);
        let mut messages = Vec::new();
        let deepseek = req
            .model
            .as_str()
            .rsplit('/')
            .next()
            .is_some_and(|model| model.starts_with("deepseek-"));
        // Thinking-mode routes (DeepSeek, Kimi) reject a later request whose
        // assistant message lacks `reasoning_content`, so once the transcript
        // holds chat-native reasoning every assistant message carries it.
        // DeepSeek also needs an empty field when it supplied no delta.
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
                Message::Assistant { model, parts, .. } => {
                    let same_model = hya_proto::context::same_model_id(model, &req.model);
                    emit_assistant(&mut messages, parts, thinking, same_model, &images)?;
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
    let (text, content, has_media) = crate::wire::user_content(
        parts,
        |text| json!({"type":"text","text":text}),
        |mime, data| Ok(json!({"type":"image_url","image_url":{"url":image_url(mime, data)?}})),
    )?;
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
    same_model: bool,
    images: &ToolImages<'_>,
) -> Result<(), ProviderError> {
    for parts in crate::wire::assistant_clusters(parts, replays_reasoning) {
        let mut cluster = Cluster::default();
        for part in parts {
            match part {
                Part::Text { text, .. } => {
                    cluster.text.push_str(text);
                }
                Part::Tool { .. } => cluster.tools.push(part),
                Part::Reasoning { text, .. } if replays_reasoning(part) => {
                    if !cluster.reasoning.is_empty() && !text.is_empty() {
                        cluster.reasoning.push_str("\n\n");
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
        if !cluster.tools.is_empty() || !cluster.text.is_empty() || !cluster.reasoning.is_empty() {
            flush_cluster(out, &cluster, thinking, same_model, images)?;
        }
    }
    Ok(())
}

fn flush_cluster(
    out: &mut Vec<Value>,
    cluster: &Cluster<'_>,
    thinking: bool,
    same_model: bool,
    images: &ToolImages<'_>,
) -> Result<(), ProviderError> {
    let content = if cluster.text.is_empty() && !thinking {
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
        return Ok(());
    }
    let tool_calls: Vec<Value> = cluster
        .tools
        .iter()
        .filter_map(|&p| {
            let Part::Tool {
                call_id,
                name,
                state,
                provider_data,
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
            let wire_id =
                crate::wire::wire_tool_call_id(call_id, provider_data.as_ref(), same_model);
            let mut tc = json!({
                "id": wire_id.as_ref(),
                "type": "function",
                "function": { "name": name.as_str(), "arguments": arguments },
            });
            if same_model
                && let Some(sig) = provider_data
                    .as_ref()
                    .and_then(|d| d.get("google_thought_signature"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            {
                tc["extra_content"] = json!({ "google": { "thought_signature": sig } });
            }
            Some(tc)
        })
        .collect();
    message["tool_calls"] = Value::Array(tool_calls);
    out.push(message);
    // A tool message carries text only, so the step's images follow its
    // tool messages as one user message, each labelled with its call.
    let mut shown = Vec::new();
    for &p in &cluster.tools {
        if let Part::Tool {
            id,
            call_id,
            state,
            provider_data,
            ..
        } = p
        {
            let wire_id =
                crate::wire::wire_tool_call_id(call_id, provider_data.as_ref(), same_model);
            let result = tool_result(id, state, images);
            out.push(
                json!({"role": "tool", "tool_call_id": wire_id.as_ref(), "content": result.text}),
            );
            if !result.images.is_empty() {
                shown.push(json!({
                    "type": "text",
                    "text": format!("Image from the result of tool call {wire_id}:"),
                }));
            }
            for (mime, data) in result.images {
                shown.push(
                    json!({"type": "image_url", "image_url": {"url": image_url(mime, data)?}}),
                );
            }
        }
    }
    if !shown.is_empty() {
        out.push(json!({"role": "user", "content": shown}));
    }
    Ok(())
}
