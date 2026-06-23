use std::collections::BTreeMap;

use serde_json::{Value, json};
use yaca_proto::{Envelope, Event, MessageId, PartId, PartProjection, SessionId, ToolPartState};

use super::message_context_parts::tool_attachment_parts;

#[derive(Clone, Copy, Default)]
struct OpenCodePartTime {
    start: Option<u64>,
    end: Option<u64>,
}

pub(super) struct OpenCodePartContext {
    times: BTreeMap<PartId, OpenCodePartTime>,
}

impl OpenCodePartContext {
    pub(super) fn new(envs: &[Envelope]) -> Self {
        Self {
            times: part_times(envs),
        }
    }

    fn optional_time(&self, part: PartId) -> Option<Value> {
        self.times.get(&part).copied().map(time_value)
    }

    fn required_time(&self, part: PartId) -> Value {
        time_value(self.times.get(&part).copied().unwrap_or_default())
    }

    fn time(&self, part: PartId) -> OpenCodePartTime {
        self.times.get(&part).copied().unwrap_or_default()
    }
}

pub(super) fn opencode_part(
    session: SessionId,
    message: MessageId,
    part: &PartProjection,
    context: &OpenCodePartContext,
) -> Value {
    match part {
        PartProjection::Text { id, text } => {
            let mut value = json!({
                "id": id.to_string(),
                "sessionID": session.to_string(),
                "messageID": message.to_string(),
                "type": "text",
                "text": text,
            });
            if let Some(time) = context.optional_time(*id) {
                value["time"] = time;
            }
            value
        }
        PartProjection::Reasoning { id, text } => json!({
            "id": id.to_string(),
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "type": "reasoning",
            "text": text,
            "time": context.required_time(*id),
        }),
        PartProjection::Tool {
            id,
            call,
            name,
            state,
        } => json!({
            "id": id.to_string(),
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "type": "tool",
            "callID": call.to_string(),
            "tool": name.as_str(),
            "state": tool_state_value(session, message, *id, state, context.time(*id)),
        }),
    }
}

fn part_times(envs: &[Envelope]) -> BTreeMap<PartId, OpenCodePartTime> {
    let mut out = BTreeMap::new();
    for env in envs {
        let time = millis(env.ts_millis);
        match &env.event {
            Event::TextStart { part, .. } | Event::ReasoningStart { part, .. } => {
                mark_start(&mut out, *part, time);
            }
            Event::TextEnd { part, .. } | Event::ReasoningEnd { part, .. } => {
                mark_end(&mut out, *part, time);
            }
            Event::ToolInputStart { part, .. } | Event::ToolCallRequested { part, .. } => {
                mark_start(&mut out, *part, time);
            }
            Event::ToolResult { part, .. } | Event::ToolError { part, .. } => {
                mark_end(&mut out, *part, time);
            }
            Event::ToolPartUpdated { part, state, .. } => {
                update_tool_time(&mut out, *part, state, time);
            }
            Event::SessionCreated { .. }
            | Event::SessionMoved { .. }
            | Event::SessionTitled { .. }
            | Event::SessionMetadataSet { .. }
            | Event::SessionPermissionSet { .. }
            | Event::SessionArchived { .. }
            | Event::SessionShareSet { .. }
            | Event::SessionShareCleared { .. }
            | Event::AgentSwitched { .. }
            | Event::ModelSwitched { .. }
            | Event::SessionStatus { .. }
            | Event::UserPromptContextRecorded { .. }
            | Event::CommandExecuted { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::MessageStarted { .. }
            | Event::MessageFinished { .. }
            | Event::MessageDeleted { .. }
            | Event::PartDeleted { .. }
            | Event::TextDelta { .. }
            | Event::TextReplace { .. }
            | Event::ReasoningDelta { .. }
            | Event::ReasoningReplace { .. }
            | Event::ToolInputDelta { .. }
            | Event::Error { .. } => {}
        }
    }
    out
}

fn mark_start(out: &mut BTreeMap<PartId, OpenCodePartTime>, part: PartId, time: u64) {
    out.entry(part).or_default().start.get_or_insert(time);
}

fn mark_end(out: &mut BTreeMap<PartId, OpenCodePartTime>, part: PartId, time: u64) {
    let entry = out.entry(part).or_default();
    entry.start.get_or_insert(time);
    entry.end = Some(time);
}

fn update_tool_time(
    out: &mut BTreeMap<PartId, OpenCodePartTime>,
    part: PartId,
    state: &ToolPartState,
    time: u64,
) {
    match state {
        ToolPartState::Pending { .. } | ToolPartState::Running { .. } => {
            mark_start(out, part, time);
        }
        ToolPartState::Completed { time_ms, .. } => {
            let entry = out.entry(part).or_default();
            entry.start.get_or_insert(time.saturating_sub(*time_ms));
            entry.end = Some(time);
        }
        ToolPartState::Error { .. } => {
            mark_end(out, part, time);
        }
    }
}

fn tool_state_value(
    session: SessionId,
    message: MessageId,
    part: PartId,
    state: &ToolPartState,
    time: OpenCodePartTime,
) -> Value {
    match state {
        ToolPartState::Pending { input } => json!({
            "phase": "pending",
            "status": "pending",
            "input": object_or_empty(input),
            "raw": pending_raw(input),
        }),
        ToolPartState::Running { input } => json!({
            "phase": "running",
            "status": "running",
            "input": object_or_empty(input),
            "time": running_time(time),
        }),
        ToolPartState::Completed {
            input,
            output,
            time_ms,
        } => {
            let mut out = json!({
                "phase": "completed",
                "status": "completed",
                "input": object_or_empty(input),
                "output": tool_output_text(output),
                "title": "",
                "metadata": {},
                "time": completed_time(time, *time_ms),
                "time_ms": time_ms,
            });
            if let Some(attachments) = tool_attachment_parts(session, message, part, output) {
                out["attachments"] = json!(attachments);
            }
            out
        }
        ToolPartState::Error {
            input,
            message,
            value,
        } => {
            let mut out = json!({
                "phase": "error",
                "status": "error",
                "input": object_or_empty(input),
                "message": message,
                "error": message,
                "time": completed_time(time, 0),
            });
            if let Some(value) = value {
                out["value"] = value.clone();
            }
            out
        }
    }
}

fn time_value(time: OpenCodePartTime) -> Value {
    let mut value = json!({ "start": time.start.unwrap_or(0) });
    if let Some(end) = time.end {
        value["end"] = json!(end);
    }
    value
}

fn running_time(time: OpenCodePartTime) -> Value {
    json!({ "start": time.start.unwrap_or(0) })
}

fn completed_time(time: OpenCodePartTime, fallback_elapsed: u64) -> Value {
    let end = time.end.unwrap_or_else(|| time.start.unwrap_or(0));
    let start = time
        .start
        .unwrap_or_else(|| end.saturating_sub(fallback_elapsed));
    json!({ "start": start, "end": end })
}

fn object_or_empty(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
}

fn pending_raw(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

fn tool_output_text(output: &Value) -> String {
    if let Some(text) = output.as_str() {
        return text.to_string();
    }
    if let Some(text) = output.get("output").and_then(Value::as_str) {
        return text.to_string();
    }
    output.to_string()
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
