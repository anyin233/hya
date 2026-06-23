use std::collections::BTreeMap;

use serde_json::{Value, json};
use yaca_proto::{Envelope, Event, MessageId, PartId, PartProjection, SessionId};

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
            "state": state,
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
            | Event::ToolInputStart { .. }
            | Event::ToolInputDelta { .. }
            | Event::ToolCallRequested { .. }
            | Event::ToolResult { .. }
            | Event::ToolError { .. }
            | Event::ToolPartUpdated { .. }
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

fn time_value(time: OpenCodePartTime) -> Value {
    let mut value = json!({ "start": time.start.unwrap_or(0) });
    if let Some(end) = time.end {
        value["end"] = json!(end);
    }
    value
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
