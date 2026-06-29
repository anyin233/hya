use std::collections::{BTreeMap, BTreeSet};

use hya_proto::{
    Envelope, Event, FinishReason, MessageId, PartId, PartProjection, SessionId, ToolPartState,
};
use serde_json::{Value, json};
use uuid::Uuid;

use super::message_context_parts::tool_attachment_parts;

#[derive(Clone, Copy, Default)]
struct OpenCodePartTime {
    start: Option<u64>,
    end: Option<u64>,
}

pub(super) struct OpenCodePartContext {
    times: BTreeMap<PartId, OpenCodePartTime>,
    timeline: BTreeMap<MessageId, Vec<OpenCodePartCursor>>,
    deleted: BTreeMap<MessageId, BTreeSet<PartId>>,
}

#[derive(Clone, Copy)]
enum OpenCodePartCursor {
    Part(PartId),
    StepStart { step: u32 },
    StepFinish { step: u32, finish: FinishReason },
}

impl OpenCodePartContext {
    pub(super) fn new(envs: &[Envelope]) -> Self {
        Self {
            times: part_times(envs),
            timeline: part_timeline(envs),
            deleted: deleted_parts(envs),
        }
    }

    fn optional_time(&self, part: PartId) -> Option<Value> {
        self.times.get(&part).copied().map(time_value)
    }

    pub(super) fn required_time(&self, part: PartId) -> Value {
        time_value(self.times.get(&part).copied().unwrap_or_default())
    }

    fn time(&self, part: PartId) -> OpenCodePartTime {
        self.times.get(&part).copied().unwrap_or_default()
    }

    fn deleted(&self, message: MessageId, part: PartId) -> bool {
        deleted_part(&self.deleted, message, part)
    }
}

pub(super) fn opencode_parts(
    session: SessionId,
    message: MessageId,
    parts: &[PartProjection],
    context: &OpenCodePartContext,
) -> Vec<Value> {
    let parts_by_id = parts
        .iter()
        .map(|part| (part.id(), part))
        .collect::<BTreeMap<_, _>>();
    let mut emitted = BTreeSet::new();
    let mut out = Vec::new();
    if let Some(timeline) = context.timeline.get(&message) {
        for item in timeline {
            match *item {
                OpenCodePartCursor::Part(part_id) => {
                    if emitted.insert(part_id)
                        && let Some(part) = parts_by_id.get(&part_id)
                    {
                        out.push(opencode_part(session, message, part, context));
                    }
                }
                OpenCodePartCursor::StepStart { step } => {
                    let part = step_start_part_id(message, step);
                    if !context.deleted(message, part) {
                        out.push(step_start_part(session, message, step));
                    }
                }
                OpenCodePartCursor::StepFinish { step, finish } => {
                    let part = step_finish_part_id(message, step);
                    if !context.deleted(message, part) {
                        out.push(step_finish_part(session, message, step, finish));
                    }
                }
            }
        }
    }
    for part in parts {
        if emitted.insert(part.id()) {
            out.push(opencode_part(session, message, part, context));
        }
    }
    out
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

fn part_timeline(envs: &[Envelope]) -> BTreeMap<MessageId, Vec<OpenCodePartCursor>> {
    let mut out: BTreeMap<MessageId, Vec<OpenCodePartCursor>> = BTreeMap::new();
    for env in envs {
        match &env.event {
            Event::StepStarted { message, step, .. } => {
                out.entry(*message)
                    .or_default()
                    .push(OpenCodePartCursor::StepStart { step: *step });
            }
            Event::StepFinished {
                message,
                step,
                finish,
                ..
            } => {
                out.entry(*message)
                    .or_default()
                    .push(OpenCodePartCursor::StepFinish {
                        step: *step,
                        finish: *finish,
                    });
            }
            Event::TextStart { message, part, .. }
            | Event::ReasoningStart { message, part, .. }
            | Event::ToolInputStart { message, part, .. }
            | Event::ToolCallRequested { message, part, .. }
            | Event::ToolPartUpdated { message, part, .. } => {
                out.entry(*message)
                    .or_default()
                    .push(OpenCodePartCursor::Part(*part));
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
            | Event::MessageStarted { .. }
            | Event::MessageFinished { .. }
            | Event::MessageDeleted { .. }
            | Event::PartDeleted { .. }
            | Event::TextDelta { .. }
            | Event::TextReplace { .. }
            | Event::TextEnd { .. }
            | Event::ReasoningDelta { .. }
            | Event::ReasoningEnd { .. }
            | Event::ReasoningReplace { .. }
            | Event::ToolInputDelta { .. }
            | Event::ToolResult { .. }
            | Event::ToolError { .. }
            | Event::Error { .. } => {}
        }
    }
    out
}

pub(super) fn step_start_part(session: SessionId, message: MessageId, step: u32) -> Value {
    json!({
        "id": step_start_part_id(message, step).to_string(),
        "sessionID": session.to_string(),
        "messageID": message.to_string(),
        "type": "step-start",
    })
}

pub(super) fn step_finish_part(
    session: SessionId,
    message: MessageId,
    step: u32,
    finish: FinishReason,
) -> Value {
    json!({
        "id": step_finish_part_id(message, step).to_string(),
        "sessionID": session.to_string(),
        "messageID": message.to_string(),
        "type": "step-finish",
        "reason": step_finish_name(finish),
        "cost": 0,
        "tokens": empty_tokens(),
    })
}

pub(super) fn step_finish_name(finish: FinishReason) -> &'static str {
    match finish {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool-calls",
        FinishReason::Length => "length",
        FinishReason::Cancelled => "error",
        FinishReason::Error => "error",
    }
}

pub(super) fn empty_tokens() -> Value {
    json!({
        "input": 0,
        "output": 0,
        "reasoning": 0,
        "cache": {"read": 0, "write": 0},
    })
}

#[derive(Clone, Copy)]
enum StepBoundary {
    Start,
    Finish,
}

pub(super) fn step_start_part_id(message: MessageId, step: u32) -> PartId {
    step_part_id(message, step, StepBoundary::Start)
}

pub(super) fn step_finish_part_id(message: MessageId, step: u32) -> PartId {
    step_part_id(message, step, StepBoundary::Finish)
}

fn step_part_id(message: MessageId, step: u32, boundary: StepBoundary) -> PartId {
    const STEP_PART_NAMESPACE: u128 = 0x5a71_aa5d_9e6d_4b81_8b66_2a2d_55e7_1000;
    let boundary_value = match boundary {
        StepBoundary::Start => 0x0101_u128,
        StepBoundary::Finish => 0x0202_u128,
    };
    let raw = message.as_uuid().as_u128()
        ^ STEP_PART_NAMESPACE
        ^ (u128::from(step) << 48)
        ^ boundary_value;
    PartId::from_uuid(Uuid::from_u128(raw))
}

pub(super) fn deleted_parts(envs: &[Envelope]) -> BTreeMap<MessageId, BTreeSet<PartId>> {
    let mut out: BTreeMap<MessageId, BTreeSet<PartId>> = BTreeMap::new();
    for env in envs {
        if let Event::PartDeleted { message, part, .. } = env.event {
            out.entry(message).or_default().insert(part);
        }
    }
    out
}

pub(super) fn deleted_part(
    deleted: &BTreeMap<MessageId, BTreeSet<PartId>>,
    message: MessageId,
    part: PartId,
) -> bool {
    deleted
        .get(&message)
        .is_some_and(|parts| parts.contains(&part))
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
            Event::ToolInputStart { .. } => {}
            Event::ToolCallRequested { part, .. } => {
                mark_start(&mut out, *part, time);
            }
            Event::ToolResult { part, .. } => {
                mark_completion_end(&mut out, *part, time);
            }
            Event::ToolError { part, .. } => {
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

fn mark_completion_end(out: &mut BTreeMap<PartId, OpenCodePartTime>, part: PartId, time: u64) {
    out.entry(part).or_default().end = Some(time);
}

fn update_tool_time(
    out: &mut BTreeMap<PartId, OpenCodePartTime>,
    part: PartId,
    state: &ToolPartState,
    time: u64,
) {
    match state {
        ToolPartState::Pending { .. } => {}
        ToolPartState::Running { .. } => {
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
            "status": "pending",
            "input": object_or_empty(input),
            "raw": pending_raw(input),
        }),
        ToolPartState::Running { input } => json!({
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
                "status": "completed",
                "input": object_or_empty(input),
                "output": tool_output_text(output),
                "title": tool_output_title(output),
                "metadata": tool_output_metadata(output),
                "time": completed_time(time, *time_ms),
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

fn tool_output_title(output: &Value) -> String {
    output
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_output_metadata(output: &Value) -> Value {
    match output.get("metadata") {
        Some(metadata) if metadata.is_object() => metadata.clone(),
        _ => json!({}),
    }
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
