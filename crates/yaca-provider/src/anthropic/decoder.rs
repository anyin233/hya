use std::collections::BTreeMap;

use serde_json::Value;
use yaca_proto::{Event, FinishReason, MessageId, PartId, SessionId, ToolCallId, ToolName};

use crate::{Decoder, ProviderError};

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
                                    name: ToolName::new(&block.name),
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
