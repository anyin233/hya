use std::collections::BTreeMap;

use hya_proto::{
    Event, FinishReason, MessageId, PartId, Role, SessionId, TokenUsage, ToolCallId, ToolName,
};
use serde_json::Value;

use crate::{Decoder, ProviderError};

struct PartAsm {
    part: PartId,
    started: bool,
    ended: bool,
}

impl PartAsm {
    fn new() -> Self {
        Self {
            part: PartId::new(),
            started: false,
            ended: false,
        }
    }
}

struct ToolAsm {
    part: PartId,
    call: ToolCallId,
    name: String,
    args: String,
    started: bool,
    requested: bool,
}

impl ToolAsm {
    fn new() -> Self {
        Self {
            part: PartId::new(),
            call: ToolCallId::new(),
            name: String::new(),
            args: String::new(),
            started: false,
            requested: false,
        }
    }
}

pub struct OpenAiResponsesDecoder {
    session: SessionId,
    message: MessageId,
    reasoning: BTreeMap<usize, PartAsm>,
    text: BTreeMap<usize, PartAsm>,
    tools: BTreeMap<usize, ToolAsm>,
    usage: TokenUsage,
    saw_tool_call: bool,
    finished: bool,
}

impl OpenAiResponsesDecoder {
    #[must_use]
    pub fn new(session: SessionId, message: MessageId) -> Self {
        Self {
            session,
            message,
            reasoning: BTreeMap::new(),
            text: BTreeMap::new(),
            tools: BTreeMap::new(),
            usage: TokenUsage::default(),
            saw_tool_call: false,
            finished: false,
        }
    }

    fn close(&mut self, finish: FinishReason) -> Vec<Event> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();
        for entry in self.reasoning.values_mut() {
            if entry.started && !entry.ended {
                entry.ended = true;
                out.push(Event::ReasoningEnd {
                    session,
                    message,
                    part: entry.part,
                    provider_data: None,
                });
            }
        }
        for entry in self.text.values_mut() {
            if entry.started && !entry.ended {
                entry.ended = true;
                out.push(Event::TextEnd {
                    session,
                    message,
                    part: entry.part,
                });
            }
        }
        for entry in self.tools.values_mut() {
            if entry.started && !entry.requested {
                entry.requested = true;
                out.push(Event::ToolCallRequested {
                    session,
                    message,
                    part: entry.part,
                    call: entry.call,
                    name: ToolName::new(&entry.name),
                    input: serde_json::from_str(&entry.args).unwrap_or(Value::Null),
                });
            }
        }
        out.push(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish,
            tokens: (!self.usage.is_zero()).then_some(self.usage),
        });
        out
    }

    fn reasoning_delta(&mut self, index: usize, delta: &str) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.reasoning.entry(index).or_insert_with(PartAsm::new);
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push(Event::ReasoningStart {
                session,
                message,
                part: entry.part,
            });
        }
        if !delta.is_empty() {
            out.push(Event::ReasoningDelta {
                session,
                message,
                part: entry.part,
                delta: delta.to_string(),
            });
        }
        out
    }

    fn reasoning_done(&mut self, index: usize, item: &Value) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.reasoning.entry(index).or_insert_with(PartAsm::new);
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push(Event::ReasoningStart {
                session,
                message,
                part: entry.part,
            });
        }
        if !entry.ended {
            entry.ended = true;
            out.push(Event::ReasoningEnd {
                session,
                message,
                part: entry.part,
                provider_data: Some(item.clone()),
            });
        }
        out
    }

    fn tool_added(&mut self, index: usize, item: &Value) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.tools.entry(index).or_insert_with(ToolAsm::new);
        if let Some(name) = item.get("name").and_then(Value::as_str) {
            entry.name = name.to_string();
        }
        if let Some(args) = item.get("arguments").and_then(Value::as_str)
            && entry.args.is_empty()
        {
            entry.args.push_str(args);
        }
        if entry.started {
            return Vec::new();
        }
        entry.started = true;
        vec![Event::ToolInputStart {
            session,
            message,
            part: entry.part,
            call: entry.call,
            name: ToolName::new(&entry.name),
        }]
    }

    fn tool_delta(&mut self, index: usize, delta: &str) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.tools.entry(index).or_insert_with(ToolAsm::new);
        entry.args.push_str(delta);
        if !entry.started || delta.is_empty() {
            return Vec::new();
        }
        vec![Event::ToolInputDelta {
            session,
            message,
            part: entry.part,
            call: entry.call,
            name: ToolName::new(&entry.name),
            delta: delta.to_string(),
        }]
    }

    fn tool_done(&mut self, index: usize, item: &Value) -> Vec<Event> {
        let mut out = self.tool_added(index, item);
        let (session, message) = (self.session, self.message);
        let entry = self.tools.entry(index).or_insert_with(ToolAsm::new);
        if let Some(args) = item.get("arguments").and_then(Value::as_str)
            && entry.args.is_empty()
        {
            entry.args = args.to_string();
        }
        if !entry.requested {
            entry.requested = true;
            self.saw_tool_call = true;
            out.push(Event::ToolCallRequested {
                session,
                message,
                part: entry.part,
                call: entry.call,
                name: ToolName::new(&entry.name),
                input: serde_json::from_str(&entry.args).unwrap_or(Value::Null),
            });
        }
        out
    }

    fn text_delta(&mut self, index: usize, delta: &str) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.text.entry(index).or_insert_with(PartAsm::new);
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push(Event::TextStart {
                session,
                message,
                part: entry.part,
            });
        }
        if !delta.is_empty() {
            out.push(Event::TextDelta {
                session,
                message,
                part: entry.part,
                delta: delta.to_string(),
            });
        }
        out
    }

    fn text_done(&mut self, index: usize) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let entry = self.text.entry(index).or_insert_with(PartAsm::new);
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push(Event::TextStart {
                session,
                message,
                part: entry.part,
            });
        }
        if !entry.ended {
            entry.ended = true;
            out.push(Event::TextEnd {
                session,
                message,
                part: entry.part,
            });
        }
        out
    }

    fn record_usage(&mut self, event: &Value) {
        let Some(usage) = event.pointer("/response/usage") else {
            return;
        };
        self.usage.merge(TokenUsage {
            input: usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning: usage
                .pointer("/output_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_read: usage
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cache_write: 0,
        });
    }
}

impl Decoder for OpenAiResponsesDecoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError> {
        if self.finished {
            return Ok(Vec::new());
        }
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            let finish = if self.saw_tool_call {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            };
            return Ok(self.close(finish));
        }
        let event: Value = serde_json::from_str(data)?;
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let out = match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.reasoning_summary_text.delta" => self.reasoning_delta(
                index,
                event.get("delta").and_then(Value::as_str).unwrap_or(""),
            ),
            "response.output_item.added"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                self.tool_added(index, &event["item"])
            }
            "response.output_item.done"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
            {
                self.reasoning_done(index, &event["item"])
            }
            "response.output_item.done"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                self.tool_done(index, &event["item"])
            }
            "response.function_call_arguments.delta" => self.tool_delta(
                index,
                event.get("delta").and_then(Value::as_str).unwrap_or(""),
            ),
            "response.output_text.delta" => self.text_delta(
                index,
                event.get("delta").and_then(Value::as_str).unwrap_or(""),
            ),
            "response.output_text.done" => self.text_done(index),
            "response.completed" => {
                self.record_usage(&event);
                let finish = if self.saw_tool_call {
                    FinishReason::ToolCalls
                } else {
                    FinishReason::Stop
                };
                self.close(finish)
            }
            "response.incomplete" => {
                self.record_usage(&event);
                self.close(FinishReason::Length)
            }
            "response.failed" => {
                let message = event
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("provider response failed");
                return Err(ProviderError::Http(message.to_string()));
            }
            "error" => {
                let message = event
                    .pointer("/error/message")
                    .or_else(|| event.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("provider returned an error");
                return Err(ProviderError::Http(message.to_string()));
            }
            _ => Vec::new(),
        };
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        let finish = if self.saw_tool_call {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        Ok(self.close(finish))
    }
}
