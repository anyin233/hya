use std::collections::BTreeMap;

use serde_json::Value;
use yaca_proto::{
    Event, FinishReason, MessageId, PartId, Role, SessionId, TokenUsage, ToolCallId, ToolName,
};

use crate::{Decoder, ProviderError};

struct ToolAsm {
    part: PartId,
    call: ToolCallId,
    name: String,
    args: String,
    started: bool,
}

impl ToolAsm {
    fn new() -> Self {
        Self {
            part: PartId::new(),
            call: ToolCallId::new(),
            name: String::new(),
            args: String::new(),
            started: false,
        }
    }
}

pub struct OpenAiChatDecoder {
    session: SessionId,
    message: MessageId,
    text_part: Option<PartId>,
    tools: BTreeMap<usize, ToolAsm>,
    finish_reason: Option<String>,
    usage: TokenUsage,
    finished: bool,
}

impl OpenAiChatDecoder {
    #[must_use]
    pub fn new(session: SessionId, message: MessageId) -> Self {
        Self {
            session,
            message,
            text_part: None,
            tools: BTreeMap::new(),
            finish_reason: None,
            usage: TokenUsage::default(),
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
        if let Some(part) = self.text_part.take() {
            out.push(Event::TextEnd {
                session,
                message,
                part,
            });
        }
        for (_, entry) in std::mem::take(&mut self.tools) {
            let input = serde_json::from_str(&entry.args).unwrap_or(Value::Null);
            out.push(Event::ToolCallRequested {
                session,
                message,
                part: entry.part,
                call: entry.call,
                name: ToolName::new(entry.name),
                input,
            });
        }
        let finish = match self.finish_reason.as_deref().unwrap_or("stop") {
            "tool_calls" => FinishReason::ToolCalls,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::Error,
            _ => FinishReason::Stop,
        };
        out.push(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish,
            tokens: (!self.usage.is_zero()).then_some(self.usage),
        });
        out
    }

    fn record_usage(&mut self, chunk: &Value) {
        if let Some(usage) = openai_usage(chunk) {
            self.usage.merge(usage);
        }
    }
}

impl Decoder for OpenAiChatDecoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        if data == "[DONE]" {
            return Ok(self.close());
        }
        let chunk: Value = serde_json::from_str(data)?;
        self.record_usage(&chunk);
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();

        let Some(choice) = chunk
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return Ok(out);
        };

        if let Some(content) = choice.pointer("/delta/content").and_then(Value::as_str)
            && !content.is_empty()
        {
            let part = match self.text_part {
                Some(p) => p,
                None => {
                    let p = PartId::new();
                    self.text_part = Some(p);
                    out.push(Event::TextStart {
                        session,
                        message,
                        part: p,
                    });
                    p
                }
            };
            out.push(Event::TextDelta {
                session,
                message,
                part,
                delta: content.to_string(),
            });
        }

        if let Some(tool_calls) = choice
            .pointer("/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for tc in tool_calls {
                let index = usize::try_from(tc.get("index").and_then(Value::as_u64).unwrap_or(0))
                    .unwrap_or(0);
                let entry = self.tools.entry(index).or_insert_with(ToolAsm::new);
                if let Some(name) = tc.pointer("/function/name").and_then(Value::as_str)
                    && !entry.started
                {
                    entry.started = true;
                    entry.name = name.to_string();
                    out.push(Event::ToolInputStart {
                        session,
                        message,
                        part: entry.part,
                        call: entry.call,
                        name: ToolName::new(name),
                    });
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(Value::as_str)
                    && !args.is_empty()
                {
                    entry.args.push_str(args);
                    out.push(Event::ToolInputDelta {
                        session,
                        message,
                        part: entry.part,
                        call: entry.call,
                        name: ToolName::new(&entry.name),
                        delta: args.to_string(),
                    });
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(finish_reason.to_string());
        }

        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        Ok(self.close())
    }
}

fn openai_usage(chunk: &Value) -> Option<TokenUsage> {
    let usage = chunk.get("usage")?;
    Some(TokenUsage {
        input: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning: usage
            .pointer("/completion_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read: usage
            .pointer("/prompt_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write: usage
            .pointer("/prompt_tokens_details/cache_creation_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}
