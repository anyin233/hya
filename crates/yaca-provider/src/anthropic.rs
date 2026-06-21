use std::collections::BTreeMap;

use serde_json::{Value, json};
use yaca_proto::{
    Event, FinishReason, Message, MessageId, Part, PartId, SessionId, ToolCallId, ToolName,
};

use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

pub struct AnthropicMessagesProtocol;

impl Protocol for AnthropicMessagesProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut messages: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m {
                Message::User { parts, .. } => {
                    messages.push(json!({"role": "user", "content": parts_text(parts)}));
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
        if let Some(effort) = req.reasoning {
            let budget = effort.anthropic_budget();
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

fn parts_text(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        if let Part::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
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

enum BlockKind {
    Text,
    Tool,
}

struct Block {
    kind: BlockKind,
    part: PartId,
    call: ToolCallId,
    name: String,
    args: String,
}

pub struct AnthropicDecoder {
    session: SessionId,
    message: MessageId,
    blocks: BTreeMap<u64, Block>,
    stop_reason: Option<String>,
    finished: bool,
}

impl AnthropicDecoder {
    #[must_use]
    pub fn new(session: SessionId, message: MessageId) -> Self {
        Self {
            session,
            message,
            blocks: BTreeMap::new(),
            stop_reason: None,
            finished: false,
        }
    }

    fn close(&mut self) -> Vec<Event> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();
        for (_, block) in std::mem::take(&mut self.blocks) {
            if matches!(block.kind, BlockKind::Tool) {
                let input = serde_json::from_str(&block.args).unwrap_or(Value::Null);
                out.push(Event::ToolCallRequested {
                    session,
                    message,
                    part: block.part,
                    call: block.call,
                    name: ToolName::new(block.name),
                    input,
                });
            }
        }
        let finish = match self.stop_reason.as_deref() {
            Some("tool_use") => FinishReason::ToolCalls,
            Some("max_tokens") => FinishReason::Length,
            _ => FinishReason::Stop,
        };
        out.push(Event::MessageFinished {
            session,
            message,
            finish,
        });
        out
    }
}

impl Decoder for AnthropicDecoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let value: Value = serde_json::from_str(data)?;
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();

        match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let cb = value.get("content_block");
                match cb.and_then(|c| c.get("type")).and_then(Value::as_str) {
                    Some("text") => {
                        let part = PartId::new();
                        self.blocks.insert(
                            index,
                            Block {
                                kind: BlockKind::Text,
                                part,
                                call: ToolCallId::new(),
                                name: String::new(),
                                args: String::new(),
                            },
                        );
                        out.push(Event::TextStart {
                            session,
                            message,
                            part,
                        });
                    }
                    Some("tool_use") => {
                        let part = PartId::new();
                        let call = ToolCallId::new();
                        let name = cb
                            .and_then(|c| c.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        self.blocks.insert(
                            index,
                            Block {
                                kind: BlockKind::Tool,
                                part,
                                call,
                                name: name.clone(),
                                args: String::new(),
                            },
                        );
                        out.push(Event::ToolInputStart {
                            session,
                            message,
                            part,
                            call,
                            name: ToolName::new(name),
                        });
                    }
                    _ => {}
                }
            }
            Some("content_block_delta") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = self.blocks.get_mut(&index) {
                    let delta = value.get("delta");
                    match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) =
                                delta.and_then(|d| d.get("text")).and_then(Value::as_str)
                            {
                                out.push(Event::TextDelta {
                                    session,
                                    message,
                                    part: block.part,
                                    delta: text.to_string(),
                                });
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(pj) = delta
                                .and_then(|d| d.get("partial_json"))
                                .and_then(Value::as_str)
                            {
                                block.args.push_str(pj);
                                out.push(Event::ToolInputDelta {
                                    session,
                                    message,
                                    part: block.part,
                                    call: block.call,
                                    delta: pj.to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("content_block_stop") => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(block) = self.blocks.get(&index)
                    && matches!(block.kind, BlockKind::Text)
                {
                    out.push(Event::TextEnd {
                        session,
                        message,
                        part: block.part,
                    });
                }
            }
            Some("message_delta") => {
                if let Some(stop) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(stop.to_string());
                }
            }
            Some("message_stop") => {
                out.extend(self.close());
            }
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        Ok(self.close())
    }
}
