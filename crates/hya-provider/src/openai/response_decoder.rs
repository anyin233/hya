use std::collections::BTreeMap;

use hya_proto::{Event, FinishReason, MessageId, PartId, Role, SessionId, TokenUsage, ToolName};
use serde_json::Value;

use crate::{Decoder, ProviderError, ReasoningEffort, stream_error::classify_error_frame};

const MISSING_TYPED_TERMINAL: &str =
    "Responses stream ended without response.completed or response.incomplete";

struct PartAsm {
    part: PartId,
    started: bool,
    ended: bool,
    summary_index: Option<usize>,
    pending_summary_index: Option<usize>,
    /// A summary-only `output_item.done` item, kept open in case the terminal
    /// output carries the item with its `encrypted_content`.
    pending_provider_data: Option<Value>,
}

impl PartAsm {
    fn new() -> Self {
        Self {
            part: PartId::new(),
            started: false,
            ended: false,
            summary_index: None,
            pending_summary_index: None,
            pending_provider_data: None,
        }
    }
}

use crate::wire::ToolAsm;

/// Stateful decoder for OpenAI Responses (and Grok typed-terminal) SSE events.
pub struct OpenAiResponsesDecoder {
    session: SessionId,
    message: MessageId,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning: BTreeMap<usize, PartAsm>,
    text: BTreeMap<usize, PartAsm>,
    tools: BTreeMap<usize, ToolAsm>,
    usage: TokenUsage,
    saw_tool_call: bool,
    finished: bool,
}

impl OpenAiResponsesDecoder {
    /// Bind a decoder that requires a typed terminal event before stream end.
    #[must_use]
    pub fn new(
        session: SessionId,
        message: MessageId,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Self {
        Self {
            session,
            message,
            reasoning_effort,
            reasoning: BTreeMap::new(),
            text: BTreeMap::new(),
            tools: BTreeMap::new(),
            usage: TokenUsage::default(),
            saw_tool_call: false,
            finished: false,
        }
    }

    fn close(&mut self, finish: FinishReason) -> Result<Vec<Event>, ProviderError> {
        if self.finished {
            return Ok(Vec::new());
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
                    provider_data: entry.pending_provider_data.take(),
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
                    input: entry.input()?,
                });
            }
        }
        out.push(Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish,
            tokens: (!self.usage.is_zero()).then_some(self.usage),
            cause: None,
        });
        Ok(out)
    }

    fn reasoning_delta(
        &mut self,
        index: usize,
        summary_index: Option<usize>,
        delta: &str,
    ) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let reason = self
            .reasoning_effort
            .map(|effort| effort.as_str().to_string());
        let entry = self.reasoning.entry(index).or_insert_with(PartAsm::new);
        let mut out = Vec::new();
        if !entry.started {
            entry.started = true;
            out.push(Event::ReasoningStart {
                session,
                message,
                part: entry.part,
                reason,
            });
        }
        if !delta.is_empty() {
            if let Some(summary_index) = summary_index.or(entry.pending_summary_index) {
                if entry
                    .summary_index
                    .is_some_and(|previous| previous != summary_index)
                {
                    out.push(Event::ReasoningDelta {
                        session,
                        message,
                        part: entry.part,
                        delta: "\n\n".to_string(),
                    });
                }
                entry.summary_index = Some(summary_index);
                entry.pending_summary_index = None;
            }
            out.push(Event::ReasoningDelta {
                session,
                message,
                part: entry.part,
                delta: delta.to_string(),
            });
        }
        out
    }

    fn reasoning_summary_part_added(&mut self, index: usize, summary_index: usize) {
        self.reasoning
            .entry(index)
            .or_insert_with(PartAsm::new)
            .pending_summary_index = Some(summary_index);
    }

    /// Open the reasoning part at `index`, so it keeps its place before any
    /// later output item.
    fn reasoning_added(&mut self, index: usize) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let reason = self
            .reasoning_effort
            .map(|effort| effort.as_str().to_string());
        let entry = self.reasoning.entry(index).or_insert_with(PartAsm::new);
        if entry.started {
            return Vec::new();
        }
        entry.started = true;
        vec![Event::ReasoningStart {
            session,
            message,
            part: entry.part,
            reason,
        }]
    }

    /// `output_item.done` for a reasoning item: close it, unless it lacks
    /// `encrypted_content` that the terminal output may still supply.
    fn reasoning_item_done(&mut self, index: usize, item: &Value) -> Vec<Event> {
        if has_encrypted_content(item) {
            return self.reasoning_done(index, item);
        }
        let out = self.reasoning_added(index);
        if let Some(entry) = self.reasoning.get_mut(&index) {
            entry.pending_provider_data = Some(item.clone());
        }
        out
    }

    fn reasoning_done(&mut self, index: usize, item: &Value) -> Vec<Event> {
        let (session, message) = (self.session, self.message);
        let mut out = self.reasoning_added(index);
        let entry = self.reasoning.entry(index).or_insert_with(PartAsm::new);
        if !entry.ended {
            entry.ended = true;
            entry.pending_provider_data = None;
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
        let event = entry.delta(session, message, delta);
        if !entry.started || delta.is_empty() {
            return Vec::new();
        }
        vec![event]
    }

    fn tool_done(&mut self, index: usize, item: &Value) -> Result<Vec<Event>, ProviderError> {
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
                input: entry.input()?,
            });
        }
        Ok(out)
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

    /// Whether an output item after `index` has started a part.
    fn has_started_after(&self, index: usize) -> bool {
        let after = index + 1..;
        self.reasoning
            .range(after.clone())
            .any(|(_, entry)| entry.started)
            || self
                .text
                .range(after.clone())
                .any(|(_, entry)| entry.started)
            || self.tools.range(after).any(|(_, entry)| entry.started)
    }

    /// Close each open reasoning item with its terminal `/response/output`
    /// item, and log reasoning the upstream removed from the input.
    /// `reasoning_done` leaves an ended item alone: a second `ReasoningEnd`
    /// would blank its text. An item never started is skipped once a later
    /// item streamed, since its part would follow that item's and replay out
    /// of order.
    fn finalize_terminal_response(&mut self, event: &Value) -> Vec<Event> {
        let mut out = Vec::new();
        let output = event.pointer("/response/output").and_then(Value::as_array);
        for (index, item) in output.into_iter().flatten().enumerate() {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                continue;
            }
            let entry = self.reasoning.get_mut(&index);
            let started = entry.as_ref().is_some_and(|entry| entry.started);
            let pending = entry.and_then(|entry| entry.pending_provider_data.take());
            if !started && self.has_started_after(index) {
                tracing::warn!(
                    session = %self.session,
                    index,
                    "responses terminal reasoning item follows streamed output; it is not kept"
                );
                continue;
            }
            let item = match pending {
                Some(pending) if !has_encrypted_content(item) => pending,
                _ => item.clone(),
            };
            // The kernel records text at `TextEnd`: end earlier open text
            // first, so it is not recorded after this reasoning.
            let (session, message) = (self.session, self.message);
            for entry in self.text.range_mut(..index).map(|(_, entry)| entry) {
                if entry.started && !entry.ended {
                    entry.ended = true;
                    out.push(Event::TextEnd {
                        session,
                        message,
                        part: entry.part,
                    });
                }
            }
            out.extend(self.reasoning_done(index, &item));
        }
        let reasons = removed_reasoning_reasons(event);
        if !reasons.is_empty() {
            tracing::warn!(
                session = %self.session,
                removed = reasons.len(),
                reasons = %reasons.join(", "),
                "responses upstream removed reasoning items from input"
            );
        }
        out
    }

    /// Normalize Responses usage to the [`TokenUsage`] invariant.
    ///
    /// `input_tokens` includes cached tokens, which are subtracted
    /// (saturating); `output_tokens` already includes reasoning. A missing
    /// `output_tokens_details.reasoning_tokens` leaves the thinking split
    /// unknown.
    fn record_usage(&mut self, event: &Value) {
        let Some(usage) = event.pointer("/response/usage") else {
            return;
        };
        let cache_read = usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let reasoning = usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64);
        self.usage.merge(TokenUsage {
            input: usage
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                .saturating_sub(cache_read),
            output: usage
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning: reasoning.unwrap_or(0),
            cache_read,
            cache_write: 0,
            reasoning_unknown: reasoning.is_none(),
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
            return Err(ProviderError::Decode(MISSING_TYPED_TERMINAL.to_string()));
        }
        let event: Value = serde_json::from_str(data)?;
        let index = event
            .get("output_index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let out = match event.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.reasoning_summary_part.added" => {
                if let Some(summary_index) = event
                    .get("summary_index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    self.reasoning_summary_part_added(index, summary_index);
                }
                Vec::new()
            }
            "response.reasoning_summary_text.delta" => self.reasoning_delta(
                index,
                event
                    .get("summary_index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok()),
                event.get("delta").and_then(Value::as_str).unwrap_or(""),
            ),
            "response.reasoning_text.delta" => self.reasoning_delta(
                index,
                None,
                event.get("delta").and_then(Value::as_str).unwrap_or(""),
            ),
            "response.output_item.added"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                self.tool_added(index, &event["item"])
            }
            "response.output_item.added"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
            {
                self.reasoning_added(index)
            }
            "response.output_item.done"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("reasoning") =>
            {
                self.reasoning_item_done(index, &event["item"])
            }
            "response.output_item.done"
                if event.pointer("/item/type").and_then(Value::as_str) == Some("function_call") =>
            {
                self.tool_done(index, &event["item"])?
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
                let mut out = self.finalize_terminal_response(&event);
                out.extend(self.close(finish)?);
                out
            }
            "response.incomplete" => {
                self.record_usage(&event);
                let mut out = self.finalize_terminal_response(&event);
                out.extend(self.close(FinishReason::Length)?);
                out
            }
            "response.failed" => {
                let error = event.pointer("/response/error").unwrap_or(&Value::Null);
                return Err(classify_error_frame(
                    error,
                    &event,
                    "provider response failed",
                ));
            }
            "error" => {
                let error = event
                    .get("error")
                    .filter(|error| error.is_object())
                    .unwrap_or(&event);
                return Err(classify_error_frame(
                    error,
                    &event,
                    "provider returned an error",
                ));
            }
            _ => Vec::new(),
        };
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<Event>, ProviderError> {
        if !self.finished {
            return Err(ProviderError::Decode(MISSING_TYPED_TERMINAL.to_string()));
        }
        let finish = if self.saw_tool_call {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        };
        self.close(finish)
    }
}

fn has_encrypted_content(item: &Value) -> bool {
    item.get("encrypted_content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty())
}

/// The `reason` of each reasoning item `/response/input_transformations`
/// reports removed (`unspecified` when absent).
fn removed_reasoning_reasons(event: &Value) -> Vec<&str> {
    event
        .pointer("/response/input_transformations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item.get("action").and_then(Value::as_str) == Some("removed")
                && item.get("type").and_then(Value::as_str) == Some("reasoning")
        })
        .map(|item| {
            item.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unspecified")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::removed_reasoning_reasons;

    #[test]
    fn removed_reasoning_reasons_lists_only_removed_reasoning_items() {
        let event = json!({"response": {"input_transformations": [
            {"action": "removed", "type": "reasoning", "reason": "prefix_mismatch"},
            {"action": "removed", "type": "reasoning"},
            {"action": "removed", "type": "message", "reason": "other"},
            {"action": "rewritten", "type": "reasoning", "reason": "other"},
        ]}});
        assert_eq!(
            removed_reasoning_reasons(&event),
            ["prefix_mismatch", "unspecified"]
        );
        assert!(removed_reasoning_reasons(&json!({"response": {}})).is_empty());
    }
}
