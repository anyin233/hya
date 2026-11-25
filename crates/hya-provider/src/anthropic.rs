use hya_proto::{Message, MessageId, Part, SessionId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::media::anthropic_image_source;
use crate::wire::{ToolImages, tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError, ReasoningEffort};

mod decoder;

pub use decoder::AnthropicDecoder;

const MAX_PROMPT_CACHE_BREAKPOINTS: usize = 4;

fn merge_anthropic_beta(headers: &mut BTreeMap<String, String>, token: &str) {
    let existing_key = headers
        .keys()
        .find(|k| k.eq_ignore_ascii_case("anthropic-beta"))
        .cloned();
    if let Some(key) = existing_key {
        let current = headers.get(&key).map(String::as_str).unwrap_or("");
        if current.split(',').map(str::trim).any(|item| item == token) {
            return;
        }
        let trimmed = current.trim().trim_end_matches(',').trim();
        let updated = if trimmed.is_empty() {
            token.to_string()
        } else {
            format!("{trimmed}, {token}")
        };
        headers.insert(key, updated);
    } else {
        headers.insert("anthropic-beta".to_string(), token.to_string());
    }
}

/// Add Anthropic ephemeral prompt-cache breakpoints to an encoded request.
pub(crate) fn add_prompt_cache_breakpoints(body: &mut Value) {
    let mut remaining = MAX_PROMPT_CACHE_BREAKPOINTS;
    if let Some(system) = body.get_mut("system") {
        let blocks = match system.take() {
            Value::String(text) => vec![json!({"type": "text", "text": text})],
            Value::Array(blocks) => blocks,
            other => vec![other],
        };
        let mut blocks = blocks;
        if let Some(last) = blocks.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
            remaining -= 1;
        }
        *system = Value::Array(blocks);
    }
    if remaining > 0
        && let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
    {
        last["cache_control"] = json!({"type": "ephemeral"});
        remaining -= 1;
    }
    if remaining > 0
        && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        && let Some(message) = messages.last_mut()
    {
        let mut blocks = content_blocks(message["content"].take());
        if !blocks.is_empty() {
            let index = blocks.len() - 1;
            blocks[index]["cache_control"] = json!({"type": "ephemeral"});
            message["content"] = Value::Array(blocks);
        } else {
            message["content"] = Value::Array(blocks);
        }
    }
}

/// Anthropic Messages API request encoder + stream decoder factory.
pub struct AnthropicMessagesProtocol;

/// Anthropic's beta header for thinking block-binding controls.
pub(crate) const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// Anthropic's minimum accepted `thinking.budget_tokens`.
const MIN_THINKING_BUDGET: u32 = 1024;
/// `max_tokens` when neither the request nor the model declares one, and the
/// answer headroom added above a thinking budget.
const DEFAULT_MAX_TOKENS: u32 = 4096;
/// Default `max_tokens` floor used when adaptive thinking is requested and no
/// caller or model output limit was specified.
const ADAPTIVE_DEFAULT_MAX_TOKENS: u32 = 16_384;

fn parse_claude_version(model_id: &str) -> Option<(u32, u32)> {
    let bare =
        hya_proto::context::bare_model_id(&hya_proto::ModelRef::new(model_id)).to_ascii_lowercase();
    let tokens: Vec<&str> = bare
        .split(['-', '.', '_', '@', ':'])
        .filter(|s| !s.is_empty())
        .collect();
    let idx = tokens.iter().position(|t| *t == "claude")?;
    let mut cursor = idx + 1;
    while matches!(
        tokens.get(cursor).copied(),
        Some("opus" | "sonnet" | "haiku")
    ) {
        cursor += 1;
    }
    let major = tokens
        .get(cursor)
        .and_then(|t| t.parse::<u32>().ok())
        .filter(|&v| (1..=99).contains(&v))?;
    let minor = tokens
        .get(cursor + 1)
        .and_then(|t| t.parse::<u32>().ok())
        .filter(|&v| v <= 99)
        .unwrap_or(0);
    Some((major, minor))
}

fn uses_anthropic_adaptive_thinking(model_id: &str) -> bool {
    parse_claude_version(model_id).is_some_and(|version| version >= (4, 6))
}

impl Protocol for AnthropicMessagesProtocol {
    fn endpoint_url(&self, base: &str, _model: &str) -> String {
        format!("{base}/messages")
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        crate::ReasoningReplayPolicy::SignedCurrentTurn
    }

    fn reasoning_variant_labels(&self) -> &'static [&'static str] {
        &["low", "medium", "high", "xhigh", "max"]
    }

    fn supports_prompt_cache(&self) -> bool {
        true
    }

    fn post_encode(
        &self,
        body: &mut Value,
        headers: &mut BTreeMap<String, String>,
        prompt_caching: bool,
    ) {
        if prompt_caching {
            add_prompt_cache_breakpoints(body);
        }
        if body.pointer("/thinking/block_binding").is_some() {
            merge_anthropic_beta(headers, THINKING_BINDING_BETA);
        }
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
        let images = ToolImages::new(&req.messages, images);
        let mut messages: Vec<Value> = Vec::new();
        let last_message = req.messages.len().checked_sub(1);
        let turn_start = crate::current_turn_start(&req.messages);
        let mut replayed_thinking = false;
        for (message_index, m) in req.messages.iter().enumerate() {
            match m {
                Message::User { parts, .. } => {
                    push_coalesced(&mut messages, "user", user_content(parts)?)
                }
                Message::Assistant { model, parts, .. } => {
                    replayed_thinking |= emit_assistant(
                        &mut messages,
                        turn_start,
                        message_index,
                        parts,
                        Some(message_index) == last_message,
                        hya_proto::context::same_model_id(model, &req.model),
                        &images,
                    )?;
                }
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
        let max_tokens = if uses_anthropic_adaptive_thinking(req.model.as_str()) {
            if let Some(effort) = req.reasoning.and_then(ReasoningEffort::anthropic_effort) {
                let mut thinking = json!({
                    "type": "adaptive",
                    "display": "summarized",
                });
                if replayed_thinking {
                    thinking["block_binding"] = json!({
                        "prefix_mismatch_behavior": "drop_block"
                    });
                }
                body["thinking"] = thinking;
                body["output_config"] = json!({ "effort": effort });
                let fallback_or_requested = req.max_output_tokens.or(Some(
                    output_limit
                        .filter(|limit| *limit > 0)
                        .unwrap_or(ADAPTIVE_DEFAULT_MAX_TOKENS),
                ));
                resolve_max_tokens(fallback_or_requested, output_limit, None).0
            } else {
                resolve_max_tokens(req.max_output_tokens, output_limit, None).0
            }
        } else {
            let budget = req.reasoning.and_then(|e| e.anthropic_budget());
            let (max_tokens, budget) =
                resolve_max_tokens(req.max_output_tokens, output_limit, budget);
            if let Some(budget) = budget {
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            }
            max_tokens
        };
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
    let (text, content, has_media) = crate::wire::user_content(
        parts,
        |text| json!({"type":"text","text":text}),
        |mime, data| Ok(json!({"type":"image","source":anthropic_image_source(mime, data)?})),
    )?;
    if has_media {
        Ok(Value::Array(content))
    } else {
        Ok(Value::String(text))
    }
}

// Anthropic pairs each `[reasoning*, text?, tool_use+]` assistant cluster with
// following user tool results. Replay reasoning only after the latest user
// message containing text, preserving provider signatures or redacted data.
fn emit_assistant(
    out: &mut Vec<Value>,
    turn_start: Option<usize>,
    message_index: usize,
    parts: &[Part],
    is_final_message: bool,
    same_model: bool,
    images: &ToolImages<'_>,
) -> Result<bool, ProviderError> {
    let mut clusters: Vec<(Vec<Value>, String, Vec<&Part>)> = Vec::new();
    for parts in crate::wire::assistant_clusters(parts, |_| true) {
        let mut reasoning = Vec::new();
        let mut text = String::new();
        let mut tools = Vec::new();
        for part in parts {
            match part {
                Part::Reasoning {
                    text: reasoning_text,
                    provider_data,
                    ..
                } => {
                    if same_model
                        && crate::ReasoningReplayPolicy::SignedCurrentTurn.replays(
                            turn_start,
                            message_index,
                            part,
                        )
                        && let Some(data) = provider_data
                    {
                        match (
                            data.get("type").and_then(Value::as_str),
                            data.get("signature").and_then(Value::as_str),
                            data.get("data").and_then(Value::as_str),
                        ) {
                            (Some("thinking"), Some(signature), _) => {
                                reasoning.push(json!({
                                    "type": "thinking",
                                    "thinking": reasoning_text,
                                    "signature": signature,
                                }));
                            }
                            (Some("redacted_thinking"), _, Some(data)) => {
                                reasoning.push(json!({
                                    "type": "redacted_thinking",
                                    "data": data,
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                Part::Text { text: t, .. } => {
                    text.push_str(t);
                }
                Part::Tool { .. } => tools.push(part),
                Part::Media { media_type, .. } => {
                    return Err(ProviderError::Incompatible(format!(
                        "Anthropic messages does not support assistant media type {media_type}"
                    )));
                }
            }
        }
        clusters.push((reasoning, text, tools));
    }
    // Legacy final text after tools would be an Anthropic assistant prefill;
    // retain it before that step's tool_use blocks instead.
    if is_final_message && clusters.len() > 1 {
        let last = clusters.len() - 1;
        if clusters[last].2.is_empty()
            && !clusters[last].1.is_empty()
            && !clusters[last - 1].2.is_empty()
            && let Some((_, trailing, _)) = clusters.pop()
        {
            clusters[last - 1].1.push_str(&trailing);
        }
    }
    let mut replayed_thinking = false;
    for (reasoning, text, tools) in clusters {
        if !reasoning.is_empty() {
            replayed_thinking = true;
        }
        if tools.is_empty() {
            let mut content = reasoning;
            if !text.is_empty() {
                content.push(json!({"type":"text","text":text}));
            }
            if !content.is_empty() {
                push_coalesced(out, "assistant", Value::Array(content));
            }
        } else {
            flush_cluster(out, &reasoning, &text, &tools, same_model, images)?;
        }
    }
    Ok(replayed_thinking)
}

fn flush_cluster(
    out: &mut Vec<Value>,
    reasoning: &[Value],
    text: &str,
    tools: &[&Part],
    same_model: bool,
    images: &ToolImages<'_>,
) -> Result<(), ProviderError> {
    let mut content = reasoning.to_vec();
    if !text.is_empty() {
        content.push(json!({"type":"text","text":text}));
    }
    for &p in tools {
        if let Part::Tool {
            call_id,
            name,
            state,
            provider_data,
            ..
        } = p
        {
            let input = tool_input(state);
            let wire_id =
                crate::wire::wire_tool_call_id(call_id, provider_data.as_ref(), same_model);
            content.push(json!({"type":"tool_use","id":wire_id.as_ref(),"name":name.as_str(),"input":if input.is_null(){json!({})}else{input.clone()}}));
        }
    }
    push_coalesced(out, "assistant", Value::Array(content));
    let mut results = Vec::new();
    for &p in tools {
        let Part::Tool {
            id,
            call_id,
            state,
            provider_data,
            ..
        } = p
        else {
            continue;
        };
        let wire_id = crate::wire::wire_tool_call_id(call_id, provider_data.as_ref(), same_model);
        let result = tool_result(id, state, images);
        let is_error = result.is_error;
        results.push(json!({
            "type": "tool_result",
            "tool_use_id": wire_id.as_ref(),
            "content": tool_result_content(result)?,
            "is_error": is_error,
        }));
    }
    push_coalesced(out, "user", Value::Array(results));
    Ok(())
}

/// A result without images stays a string; images go as `image` blocks
/// inside the `tool_result` content.
fn tool_result_content(result: crate::wire::ToolResult<'_>) -> Result<Value, ProviderError> {
    if result.images.is_empty() {
        return Ok(Value::String(result.text));
    }
    let mut blocks = Vec::new();
    if !result.text.is_empty() {
        blocks.push(json!({"type": "text", "text": result.text}));
    }
    for (mime, data) in result.images {
        blocks.push(json!({"type": "image", "source": anthropic_image_source(mime, data)?}));
    }
    Ok(Value::Array(blocks))
}
