use serde_json::{Map, Value, json};
use yaca_proto::{
    Event, FinishReason, Message, MessageId, Part, PartId, SessionId, ToolCallId, ToolName,
};

use crate::wire::{tool_input, tool_result};
use crate::{CompletionRequest, Decoder, Protocol, ProviderError};

pub struct GoogleProtocol;

fn parts_text(parts: &[Part]) -> String {
    let mut s = String::new();
    for p in parts {
        if let Part::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

fn emit_assistant(out: &mut Vec<Value>, parts: &[Part]) {
    let mut model_parts: Vec<Value> = Vec::new();
    let mut responses: Vec<Value> = Vec::new();
    for part in parts {
        match part {
            Part::Text { text, .. } => {
                if !text.is_empty() {
                    model_parts.push(json!({ "text": text }));
                }
            }
            Part::Tool { name, state, .. } => {
                let input = tool_input(state);
                let args = if input.is_null() {
                    json!({})
                } else {
                    input.clone()
                };
                model_parts.push(json!({
                    "functionCall": { "name": name.as_str(), "args": args }
                }));
                let (result, _is_error) = tool_result(state);
                responses.push(json!({
                    "functionResponse": { "name": name.as_str(), "response": { "result": result } }
                }));
            }
            Part::Reasoning { .. } => {}
        }
    }
    if !model_parts.is_empty() {
        out.push(json!({ "role": "model", "parts": model_parts }));
    }
    if !responses.is_empty() {
        out.push(json!({ "role": "user", "parts": responses }));
    }
}

impl Protocol for GoogleProtocol {
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut system_text = req.system.clone().unwrap_or_default();
        let mut contents: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m {
                Message::System { content, .. } => {
                    if !system_text.is_empty() {
                        system_text.push('\n');
                    }
                    system_text.push_str(content);
                }
                Message::User { parts, .. } => {
                    contents.push(json!({"role":"user","parts":[{"text": parts_text(parts)}]}));
                }
                Message::Assistant { parts, .. } => emit_assistant(&mut contents, parts),
            }
        }
        let mut body = json!({ "contents": contents });
        if !system_text.is_empty() {
            body["systemInstruction"] = json!({"parts":[{"text": system_text}]});
        }
        let decls: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name.as_str(),
                    "description": t.description,
                    "parameters": t.input_schema,
                })
            })
            .collect();
        if !decls.is_empty() {
            body["tools"] = json!([{ "functionDeclarations": decls }]);
        }
        let mut gen_config = Map::new();
        if let Some(t) = req.temperature {
            gen_config.insert("temperature".to_string(), json!(t));
        }
        if let Some(m) = req.max_output_tokens {
            gen_config.insert("maxOutputTokens".to_string(), json!(m));
        }
        if let Some(effort) = req.reasoning {
            gen_config.insert(
                "thinkingConfig".to_string(),
                json!({ "thinkingBudget": effort.google_budget() }),
            );
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }
        Ok(body)
    }

    fn decoder(&self, session: SessionId, message: MessageId) -> Box<dyn Decoder> {
        Box::new(GoogleDecoder::new(session, message))
    }
}

pub struct GoogleDecoder {
    session: SessionId,
    message: MessageId,
    text_part: Option<PartId>,
    saw_tool: bool,
    finished: bool,
}

impl GoogleDecoder {
    #[must_use]
    pub fn new(session: SessionId, message: MessageId) -> Self {
        Self {
            session,
            message,
            text_part: None,
            saw_tool: false,
            finished: false,
        }
    }

    fn close(&mut self, reason: &str) -> Vec<Event> {
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
        let finish = if self.saw_tool {
            FinishReason::ToolCalls
        } else {
            match reason {
                "MAX_TOKENS" => FinishReason::Length,
                "SAFETY" | "RECITATION" => FinishReason::Error,
                _ => FinishReason::Stop,
            }
        };
        out.push(Event::MessageFinished {
            session,
            message,
            finish,
        });
        out
    }
}

impl Decoder for GoogleDecoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let chunk: Value = serde_json::from_str(data)?;
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();
        let Some(cand) = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
        else {
            return Ok(out);
        };
        if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        let p = match self.text_part {
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
                            part: p,
                            delta: text.to_string(),
                        });
                    }
                } else if let Some(fc) = part.get("functionCall") {
                    self.saw_tool = true;
                    let name = fc.get("name").and_then(Value::as_str).unwrap_or_default();
                    let args = fc.get("args").cloned().unwrap_or(Value::Null);
                    let part_id = PartId::new();
                    let call = ToolCallId::new();
                    out.push(Event::ToolInputStart {
                        session,
                        message,
                        part: part_id,
                        call,
                        name: ToolName::new(name),
                    });
                    out.push(Event::ToolCallRequested {
                        session,
                        message,
                        part: part_id,
                        call,
                        name: ToolName::new(name),
                        input: args,
                    });
                }
            }
        }
        if let Some(reason) = cand.get("finishReason").and_then(Value::as_str) {
            out.extend(self.close(reason));
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        Ok(self.close("STOP"))
    }
}
