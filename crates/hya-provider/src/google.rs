use hya_proto::context::{GOOGLE_TEXT_SIGNATURE_TYPE, GOOGLE_THOUGHT_TYPE, same_model_id};
use hya_proto::{
    Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, Role, SessionId, TokenUsage,
    ToolCallId, ToolName,
};
use serde_json::{Map, Value, json};

use crate::media::google_inline_data;

use crate::catalog_discovery::{LABEL_KEYS, append_query, row_metadata};
use crate::wire::{ToolImages, tool_input, tool_result};
use crate::{
    CatalogFailure, CompletionRequest, Decoder, DiscoveredModel, Protocol, ProviderError,
    ReasoningEffort, RequestHeader, RequestPurpose, RouteCredential,
};

/// Gemini generateContent request encoder + stream decoder factory.
pub struct GoogleProtocol;

fn uses_google_thinking_level(model_id: &str) -> bool {
    let bare = hya_proto::context::bare_model_id(&ModelRef::new(model_id)).to_ascii_lowercase();
    let Some(rest) = bare.strip_prefix("gemini-") else {
        return false;
    };
    let major = rest.split(['.', '-']).next().unwrap_or_default();
    major.parse::<u32>().is_ok_and(|v| v >= 3)
}

fn is_google_reasoning_data(data: &Value) -> bool {
    matches!(
        data.get("type").and_then(Value::as_str),
        Some(t) if t == GOOGLE_THOUGHT_TYPE || t == GOOGLE_TEXT_SIGNATURE_TYPE
    )
}

fn tool_thought_signature(provider_data: Option<&Value>) -> Option<&str> {
    provider_data
        .and_then(|d| {
            d.get("thought_signature")
                .or_else(|| d.get("thoughtSignature"))
        })
        .and_then(Value::as_str)
        .filter(|sig| !sig.is_empty())
}

fn extract_thought_signature(part: &Value) -> Option<&str> {
    part.get("thoughtSignature")
        .or_else(|| part.get("thought_signature"))
        .and_then(Value::as_str)
        .filter(|sig| !sig.is_empty())
}

fn push_coalesced(out: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role)
        && let Some(existing) = last.get_mut("parts").and_then(Value::as_array_mut)
    {
        existing.extend(parts);
        return;
    }
    out.push(json!({ "role": role, "parts": parts }));
}

fn google_assistant_clusters(parts: &[Part], same_model: bool) -> Vec<&[Part]> {
    let mut clusters = Vec::new();
    let mut start = 0;
    let mut tools = false;
    for (index, part) in parts.iter().enumerate() {
        let boundary = match part {
            Part::Text { .. } => true,
            Part::Reasoning { provider_data, .. } => {
                same_model && provider_data.as_ref().is_some_and(is_google_reasoning_data)
            }
            Part::Tool { provider_data, .. } => {
                same_model && tool_thought_signature(provider_data.as_ref()).is_some()
            }
            Part::Media { .. } => false,
        };
        if tools && boundary {
            clusters.push(&parts[start..index]);
            start = index;
            tools = false;
        }
        if matches!(part, Part::Tool { .. }) {
            tools = true;
        }
    }
    clusters.push(&parts[start..]);
    clusters
}

fn attach_text_signature(model_parts: &mut Vec<Value>, sig: &str) {
    if let Some(last_text) = model_parts.iter_mut().rev().find(|part| {
        part.get("text").is_some()
            && part.get("thought").and_then(Value::as_bool) != Some(true)
            && part.get("thoughtSignature").is_none()
    }) {
        last_text["thoughtSignature"] = json!(sig);
    } else {
        model_parts.push(json!({ "text": "", "thoughtSignature": sig }));
    }
}

fn emit_assistant(
    out: &mut Vec<Value>,
    parts: &[Part],
    same_model: bool,
) -> Result<(), ProviderError> {
    for cluster in google_assistant_clusters(parts, same_model) {
        let mut model_parts: Vec<Value> = Vec::new();
        let mut responses: Vec<Value> = Vec::new();
        for part in cluster {
            match part {
                Part::Text { text, .. } => {
                    if !text.is_empty() {
                        model_parts.push(json!({ "text": text }));
                    }
                }
                Part::Tool {
                    id,
                    name,
                    state,
                    provider_data,
                    ..
                } => {
                    let input = tool_input(state);
                    let args = if input.is_null() {
                        json!({})
                    } else {
                        input.clone()
                    };
                    let mut fc = json!({ "name": name.as_str(), "args": args });
                    let upstream_id = if same_model {
                        provider_data
                            .as_ref()
                            .and_then(|d| d.get("call_id"))
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                    } else {
                        None
                    };
                    if let Some(uid) = upstream_id {
                        fc["id"] = json!(uid);
                    }
                    let mut call_part = json!({ "functionCall": fc });
                    if same_model && let Some(sig) = tool_thought_signature(provider_data.as_ref())
                    {
                        call_part["thoughtSignature"] = json!(sig);
                    }
                    model_parts.push(call_part);

                    // This route sends no tool-result image; the text says so.
                    let result = tool_result(id, state, &ToolImages::new(&[], false)).text;
                    let mut fr = json!({ "name": name.as_str(), "response": { "result": result } });
                    if let Some(uid) = upstream_id {
                        fr["id"] = json!(uid);
                    }
                    responses.push(json!({ "functionResponse": fr }));
                }
                Part::Reasoning {
                    text,
                    provider_data,
                    ..
                } => {
                    if !same_model {
                        continue;
                    }
                    let Some(data) = provider_data else {
                        continue;
                    };
                    let sig = data
                        .get("thought_signature")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty());
                    match data.get("type").and_then(Value::as_str) {
                        Some(t) if t == GOOGLE_THOUGHT_TYPE => {
                            if !text.is_empty() || sig.is_some() {
                                let mut entry = json!({ "thought": true, "text": text });
                                if let Some(sig) = sig {
                                    entry["thoughtSignature"] = json!(sig);
                                }
                                model_parts.push(entry);
                            }
                        }
                        Some(t) if t == GOOGLE_TEXT_SIGNATURE_TYPE => {
                            if let Some(sig) = sig {
                                attach_text_signature(&mut model_parts, sig);
                            }
                        }
                        _ => {}
                    }
                }
                Part::Media { media_type, .. } => {
                    return Err(ProviderError::Incompatible(format!(
                        "Google does not support assistant media type {media_type}"
                    )));
                }
            }
        }
        push_coalesced(out, "model", model_parts);
        push_coalesced(out, "user", responses);
    }
    Ok(())
}

fn user_parts(parts: &[Part]) -> Result<Vec<Value>, ProviderError> {
    let (_, blocks, _) =
        crate::wire::user_content(parts, |text| json!({"text":text}), google_inline_data)?;
    Ok(blocks)
}

impl Protocol for GoogleProtocol {
    fn kind(&self) -> &str {
        "google"
    }

    fn description(&self) -> &str {
        "Google Gemini generateContent (<base>/v1beta/models)"
    }

    fn endpoint_url(&self, base: &str, model: &str) -> String {
        format!("{base}/v1beta/models/{model}:streamGenerateContent?alt=sse")
    }

    fn auth_headers(
        &self,
        credential: Option<&RouteCredential>,
        _purpose: RequestPurpose,
        _model: Option<&str>,
    ) -> Vec<RequestHeader> {
        credential
            .map(|credential| RequestHeader::new("x-goog-api-key", &credential.token, true))
            .into_iter()
            .collect()
    }

    fn models_page_url(
        &self,
        endpoint: &str,
        cursor: Option<&str>,
    ) -> Result<String, CatalogFailure> {
        match cursor {
            Some(cursor) => append_query(endpoint, "pageToken", cursor),
            None => Ok(endpoint.to_owned()),
        }
    }

    fn models_next_cursor(&self, payload: &Value) -> Result<Option<String>, CatalogFailure> {
        payload
            .get("nextPageToken")
            .map(|token| {
                token
                    .as_str()
                    .filter(|cursor| !cursor.is_empty())
                    .map(str::to_owned)
                    .ok_or(CatalogFailure::PaginationLimit)
            })
            .transpose()
    }

    fn parse_models(&self, payload: &Value) -> Result<Vec<DiscoveredModel>, CatalogFailure> {
        let array = payload
            .get("models")
            .and_then(Value::as_array)
            .ok_or(CatalogFailure::Schema)?;
        let mut models = Vec::new();
        for row in array {
            let object = row.as_object().ok_or(CatalogFailure::Schema)?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or(CatalogFailure::Schema)?;
            let Some(id) = name.strip_prefix("models/") else {
                return Err(CatalogFailure::Schema);
            };
            let methods = object
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .ok_or(CatalogFailure::Schema)?;
            if methods
                .iter()
                .any(|method| method.as_str() == Some("generateContent"))
            {
                // `name` is the `models/<id>` resource name, not a label.
                let (display_name, context_limit, output_limit) = row_metadata(object, LABEL_KEYS);
                models.push(DiscoveredModel {
                    id: id.to_string(),
                    display_name: display_name.filter(|name| name != id),
                    context_limit,
                    output_limit,
                    ..DiscoveredModel::default()
                });
            }
        }
        Ok(models)
    }

    fn reasoning_replay_policy(&self) -> crate::ReasoningReplayPolicy {
        crate::ReasoningReplayPolicy::GoogleThought
    }

    fn reasoning_variant_labels(&self) -> &'static [&'static str] {
        &["minimal", "low", "medium", "high", "max"]
    }

    fn supports_tool_images(&self) -> bool {
        false
    }
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        let mut contents: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m {
                Message::System { content, .. } => {
                    if !content.is_empty() {
                        push_coalesced(&mut contents, "user", vec![json!({ "text": content })]);
                    }
                }
                Message::User { parts, .. } => {
                    push_coalesced(&mut contents, "user", user_parts(parts)?);
                }
                Message::Assistant { model, parts, .. } => {
                    emit_assistant(&mut contents, parts, same_model_id(model, &req.model))?;
                }
            }
        }
        let mut body = json!({ "contents": contents });
        if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
            body["systemInstruction"] = json!({"parts":[{"text": system}]});
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
        if uses_google_thinking_level(req.model.as_str()) {
            if let Some(level) = req
                .reasoning
                .and_then(|e| e.google_thinking_level(req.model.as_str()))
            {
                gen_config.insert(
                    "thinkingConfig".to_string(),
                    json!({ "thinkingLevel": level, "includeThoughts": true }),
                );
            }
        } else if let Some(budget) = req
            .reasoning
            .and_then(|e| e.google_budget(req.model.as_str()))
        {
            gen_config.insert(
                "thinkingConfig".to_string(),
                json!({ "thinkingBudget": budget, "includeThoughts": true }),
            );
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }
        Ok(body)
    }

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        Box::new(GoogleDecoder::new(session, message).with_reasoning(reasoning))
    }
}

/// Stateful decoder for Gemini stream generateContent JSON fragments.
pub struct GoogleDecoder {
    session: SessionId,
    message: MessageId,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_part: Option<PartId>,
    reasoning_signature: Option<String>,
    text_part: Option<PartId>,
    saw_tool: bool,
    usage: TokenUsage,
    finished: bool,
}

impl GoogleDecoder {
    /// Bind a new decoder to the turn's session and assistant message ids.
    #[must_use]
    pub fn new(session: SessionId, message: MessageId) -> Self {
        Self {
            session,
            message,
            reasoning_effort: None,
            reasoning_part: None,
            reasoning_signature: None,
            text_part: None,
            saw_tool: false,
            usage: TokenUsage::default(),
            finished: false,
        }
    }

    /// Attach the request's reasoning effort so decoded thought parts record `reason`.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning;
        self
    }

    fn end_reasoning(&mut self, out: &mut Vec<Event>) {
        if let Some(part) = self.reasoning_part.take() {
            let mut data = json!({ "type": GOOGLE_THOUGHT_TYPE });
            if let Some(sig) = self.reasoning_signature.take() {
                data["thought_signature"] = Value::String(sig);
            }
            out.push(Event::ReasoningEnd {
                session: self.session,
                message: self.message,
                part,
                provider_data: Some(data),
            });
        } else {
            self.reasoning_signature = None;
        }
    }

    fn end_text(&mut self, out: &mut Vec<Event>) {
        if let Some(part) = self.text_part.take() {
            out.push(Event::TextEnd {
                session: self.session,
                message: self.message,
                part,
            });
        }
    }

    fn close(&mut self, reason: &str) -> Vec<Event> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        let (session, message) = (self.session, self.message);
        let mut out = Vec::new();
        self.end_reasoning(&mut out);
        self.end_text(&mut out);
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
            role: Role::Assistant,
            finish,
            tokens: (!self.usage.is_zero()).then_some(self.usage),
            cause: None,
        });
        out
    }

    fn record_usage(&mut self, chunk: &Value) {
        if let Some(usage) = google_usage(chunk) {
            self.usage.merge(usage);
        }
    }
}

impl Decoder for GoogleDecoder {
    fn push(&mut self, data: &str) -> Result<Vec<Event>, ProviderError> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(Vec::new());
        }
        let chunk: Value = serde_json::from_str(data)?;
        self.record_usage(&chunk);
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
                let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
                let sig = extract_thought_signature(part);
                if thought {
                    self.end_text(&mut out);
                    let text = part.get("text").and_then(Value::as_str).unwrap_or_default();
                    if !text.is_empty() || sig.is_some() {
                        let reason = self
                            .reasoning_effort
                            .map(|effort| effort.as_str().to_string());
                        let p = match self.reasoning_part {
                            Some(p) => p,
                            None => {
                                let p = PartId::new();
                                self.reasoning_part = Some(p);
                                out.push(Event::ReasoningStart {
                                    session,
                                    message,
                                    part: p,
                                    reason,
                                });
                                p
                            }
                        };
                        if !text.is_empty() {
                            out.push(Event::ReasoningDelta {
                                session,
                                message,
                                part: p,
                                delta: text.to_string(),
                            });
                        }
                        if let Some(sig) = sig {
                            self.reasoning_signature = Some(sig.to_string());
                        }
                    }
                } else if let Some(fc) = part.get("functionCall") {
                    self.end_reasoning(&mut out);
                    self.end_text(&mut out);
                    self.saw_tool = true;
                    let name = fc.get("name").and_then(Value::as_str).unwrap_or_default();
                    let args = fc.get("args").cloned().unwrap_or(Value::Null);
                    let upstream_id = fc
                        .get("id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty());
                    let mut meta = Map::new();
                    if let Some(id) = upstream_id {
                        meta.insert("call_id".to_string(), Value::String(id.to_string()));
                    }
                    if let Some(sig) = sig {
                        meta.insert(
                            "thought_signature".to_string(),
                            Value::String(sig.to_string()),
                        );
                    }
                    let provider_data = (!meta.is_empty()).then_some(Value::Object(meta));
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
                        provider_data,
                    });
                } else if part.get("text").is_some() || sig.is_some() {
                    self.end_reasoning(&mut out);
                    if let Some(text) = part.get("text").and_then(Value::as_str)
                        && !text.is_empty()
                    {
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
                    if let Some(sig) = sig {
                        self.end_text(&mut out);
                        let p = PartId::new();
                        let reason = self
                            .reasoning_effort
                            .map(|effort| effort.as_str().to_string());
                        out.push(Event::ReasoningStart {
                            session,
                            message,
                            part: p,
                            reason,
                        });
                        out.push(Event::ReasoningEnd {
                            session,
                            message,
                            part: p,
                            provider_data: Some(json!({
                                "type": GOOGLE_TEXT_SIGNATURE_TYPE,
                                "thought_signature": sig,
                            })),
                        });
                    }
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

/// Normalize Gemini usage metadata to the [`TokenUsage`] invariant.
///
/// `promptTokenCount` includes cached content, which is subtracted
/// (saturating). `candidatesTokenCount` excludes thoughts, so thinking
/// (`thoughtsTokenCount`, 0 when absent) is added back into `output`. Gemini
/// reports the split exactly, so it is never unknown.
fn google_usage(chunk: &Value) -> Option<TokenUsage> {
    let usage = chunk.get("usageMetadata")?;
    let cache_read = usage
        .get("cachedContentTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let thoughts = usage
        .get("thoughtsTokenCount")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some(TokenUsage {
        input: usage
            .get("promptTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_sub(cache_read),
        output: usage
            .get("candidatesTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(thoughts),
        reasoning: thoughts,
        cache_read,
        cache_write: 0,
        reasoning_unknown: false,
    })
}
