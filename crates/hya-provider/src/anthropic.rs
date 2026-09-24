use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};

use crate::media::anthropic_image_source;
use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError, ReasoningEffort};

mod decoder;

pub use decoder::AnthropicDecoder;

/// Anthropic Messages API request encoder + stream decoder factory.
pub struct AnthropicMessagesProtocol;

/// Anthropic's minimum accepted `thinking.budget_tokens`.
const MIN_THINKING_BUDGET: u32 = 1024;
/// `max_tokens` when neither the request nor the model declares one, and the
/// answer headroom added above a thinking budget.
const DEFAULT_MAX_TOKENS: u32 = 4096;

impl Protocol for AnthropicMessagesProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        self.encode_with_output_limit(req, None)
    }

    fn encode_with_output_limit(
        &self,
        req: &CompletionRequest,
        output_limit: Option<u32>,
    ) -> Result<Value, ProviderError> {
        let mut messages: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m {
                Message::User { parts, .. } => {
                    push_coalesced(&mut messages, "user", user_content(parts)?);
                }
                Message::Assistant { parts, .. } => emit_assistant(&mut messages, parts)?,
                // Mid-conversation system text is where compaction summaries
                // live. The Messages API has no system role inside `messages`,
                // and folding it into the top-level `system` field would
                // invalidate the cached prefix on every compaction, so it is
                // encoded as user text at the position it was injected.
                Message::System { content, .. } => {
                    if !content.is_empty() {
                        push_coalesced(&mut messages, "user", Value::String(content.clone()));
                    }
                }
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
        let mut body = json!({
            "model": req.model.as_str(),
            "messages": messages,
            "stream": true,
        });
        let budget = req.reasoning.and_then(|e| e.anthropic_budget());
        let (max_tokens, budget) = resolve_max_tokens(req.max_output_tokens, output_limit, budget);
        if let Some(budget) = budget {
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

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        Box::new(AnthropicDecoder::new(session, message, reasoning))
    }
}

/// Resolve the required `max_tokens` and the thinking budget that fits it.
///
/// Without a known limit an unset request falls back to 4096, and a value at or
/// below the budget is raised to `budget + 4096`. A known limit is the default
/// for an unset request and a hard ceiling: when it cannot hold the budget the
/// budget shrinks to leave `min(4096, limit / 2)` answer tokens, and thinking
/// is dropped when that leaves less than Anthropic's 1024-token minimum.
fn resolve_max_tokens(
    requested: Option<u32>,
    output_limit: Option<u32>,
    budget: Option<u32>,
) -> (u32, Option<u32>) {
    let limit = output_limit.filter(|limit| *limit > 0);
    let mut max_tokens = requested.or(limit).unwrap_or(DEFAULT_MAX_TOKENS);
    let Some(mut budget) = budget else {
        return (
            limit.map_or(max_tokens, |limit| max_tokens.min(limit)),
            None,
        );
    };
    if max_tokens <= budget {
        max_tokens = budget.saturating_add(DEFAULT_MAX_TOKENS);
    }
    if let Some(limit) = limit {
        max_tokens = max_tokens.min(limit);
        if budget >= max_tokens {
            budget = max_tokens
                .saturating_sub(DEFAULT_MAX_TOKENS)
                .max(max_tokens / 2);
        }
    }
    let budget = (budget >= MIN_THINKING_BUDGET).then_some(budget);
    (max_tokens, budget)
}

/// Append a message, merging it into the previous entry when the role repeats.
///
/// The Messages API requires alternating roles. Tool-result clusters already end
/// on a `user` entry, so a summary or follow-up landing next to one would
/// otherwise produce two consecutive `user` messages and be rejected.
fn push_coalesced(out: &mut Vec<Value>, role: &str, content: Value) {
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
    {
        let mut blocks = content_blocks(last["content"].take());
        blocks.extend(content_blocks(content));
        last["content"] = Value::Array(blocks);
        return;
    }
    out.push(json!({"role": role, "content": content}));
}

/// Normalize a message `content` field into a content-block array.
fn content_blocks(content: Value) -> Vec<Value> {
    match content {
        Value::Array(blocks) => blocks,
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Null => Vec::new(),
        other => vec![other],
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
                    "type": "image",
                    "source": anthropic_image_source(media_type, data)?,
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

// Anthropic puts tool_use blocks in the assistant message and the matching
// tool_result blocks in the FOLLOWING user message. Segment each `[text?, tool+]`
// cluster into that pair; trailing text becomes a final assistant text message.
fn emit_assistant(out: &mut Vec<Value>, parts: &[Part]) -> Result<(), ProviderError> {
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
            Part::Media { media_type, .. } => {
                return Err(ProviderError::Incompatible(format!(
                    "Anthropic messages does not support assistant media type {media_type}"
                )));
            }
        }
    }
    if tools.is_empty() {
        if !text.is_empty() {
            push_coalesced(out, "assistant", json!([{"type": "text", "text": text}]));
        }
    } else {
        flush_cluster(out, &text, &tools);
    }
    Ok(())
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
    push_coalesced(out, "assistant", Value::Array(content));
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
    push_coalesced(out, "user", Value::Array(results));
}
