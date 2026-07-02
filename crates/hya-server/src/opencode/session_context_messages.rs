use std::collections::{BTreeMap, BTreeSet};

use hya_proto::model::{AgentName, ModelRef};
use hya_proto::{
    Envelope, Event, FinishReason, MessageId, MessageProjection, PartId, PartProjection,
    Projection, Role, SessionId,
};
use serde_json::{Value, json};

use super::session_context_tool_time::ToolTime;

#[derive(Clone, Copy, Default)]
struct MessageTime {
    created: Option<u64>,
    completed: Option<u64>,
}

#[derive(Clone, Copy)]
enum ContentCursor {
    Part(PartId),
    StepStart { step: u32 },
    StepFinish { step: u32, finish: FinishReason },
}

pub(in crate::opencode) fn v2_messages(envs: &[Envelope], projection: &Projection) -> Vec<Value> {
    let times = message_times(envs);
    let part_context = super::message_parts::OpenCodePartContext::new(envs);
    let tool_times = super::session_context_tool_time::tool_times(envs);
    let content_events = message_content_events(envs);
    let deleted_parts = super::message_parts::deleted_parts(envs);
    let messages = projection
        .session
        .messages
        .iter()
        .map(|message| (message.id, message))
        .collect::<BTreeMap<_, _>>();
    let mut emitted = BTreeSet::new();
    let mut out = Vec::new();
    for env in envs {
        match &env.event {
            Event::AgentSwitched {
                message: Some(message),
                agent,
                ..
            } => out.push(agent_switch_message(*message, agent, env.ts_millis)),
            Event::ModelSwitched {
                message: Some(message),
                model,
                ..
            } => out.push(model_switch_message(*message, model, env.ts_millis)),
            Event::MessageStarted { message, .. } => {
                if emitted.insert(*message)
                    && let Some(message) = messages.get(message)
                {
                    out.push(message_json(
                        message,
                        projection,
                        &times,
                        &part_context,
                        &tool_times,
                        &content_events,
                        &deleted_parts,
                    ));
                }
            }
            _ => {}
        }
    }
    for message in &projection.session.messages {
        if emitted.insert(message.id) {
            out.push(message_json(
                message,
                projection,
                &times,
                &part_context,
                &tool_times,
                &content_events,
                &deleted_parts,
            ));
        }
    }
    out
}

fn agent_switch_message(message: MessageId, agent: &AgentName, ts_millis: i64) -> Value {
    json!({
        "id": message.to_string(),
        "time": { "created": millis(ts_millis) },
        "type": "agent-switched",
        "agent": agent.as_str(),
    })
}

fn model_switch_message(message: MessageId, model: &ModelRef, ts_millis: i64) -> Value {
    json!({
        "id": message.to_string(),
        "time": { "created": millis(ts_millis) },
        "type": "model-switched",
        "model": super::projection::model_info(model),
    })
}

fn message_json(
    message: &MessageProjection,
    projection: &Projection,
    times: &BTreeMap<MessageId, MessageTime>,
    part_context: &super::message_parts::OpenCodePartContext,
    tool_times: &BTreeMap<PartId, ToolTime>,
    content_events: &BTreeMap<MessageId, Vec<ContentCursor>>,
    deleted_parts: &BTreeMap<MessageId, BTreeSet<PartId>>,
) -> Value {
    let time = times.get(&message.id).copied().unwrap_or_default();
    match message.role {
        Role::User => json!({
            "id": message.id.to_string(),
            "time": { "created": time.created.unwrap_or(0) },
            "text": text_content(&message.parts),
            "files": message.files,
            "agents": message.agents,
            "type": "user",
        }),
        Role::Assistant => assistant_message(
            message,
            projection,
            time,
            part_context,
            tool_times,
            content_events.get(&message.id),
            deleted_parts,
        ),
        Role::System => json!({
            "id": message.id.to_string(),
            "time": { "created": time.created.unwrap_or(0) },
            "type": "system",
            "text": text_content(&message.parts),
        }),
    }
}

fn assistant_message(
    message: &MessageProjection,
    projection: &Projection,
    time: MessageTime,
    part_context: &super::message_parts::OpenCodePartContext,
    tool_times: &BTreeMap<PartId, ToolTime>,
    content_events: Option<&Vec<ContentCursor>>,
    deleted_parts: &BTreeMap<MessageId, BTreeSet<PartId>>,
) -> Value {
    let mut time_value = json!({ "created": time.created.unwrap_or(0) });
    if let Some(completed) = time.completed {
        time_value["completed"] = json!(completed);
    }
    let mut value = json!({
        "id": message.id.to_string(),
        "time": time_value,
        "type": "assistant",
        "agent": projection
            .session
            .agent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "build".to_string()),
        "model": projection
            .session
            .model
            .as_ref()
            .map(super::projection::model_info),
        "content": content_json(
            projection.session.id,
            message,
            part_context,
            tool_times,
            content_events,
            deleted_parts,
        ),
    });
    if let Some(finish) = message.finish {
        value["finish"] = json!(finish_name(finish));
    }
    value
}

fn content_json(
    session: Option<SessionId>,
    message: &MessageProjection,
    part_context: &super::message_parts::OpenCodePartContext,
    tool_times: &BTreeMap<PartId, ToolTime>,
    content_events: Option<&Vec<ContentCursor>>,
    deleted_parts: &BTreeMap<MessageId, BTreeSet<PartId>>,
) -> Vec<Value> {
    let parts_by_id = message
        .parts
        .iter()
        .map(|part| (part.id(), part))
        .collect::<BTreeMap<_, _>>();
    let mut emitted = BTreeSet::new();
    let mut out = Vec::new();
    if let (Some(session), Some(events)) = (session, content_events) {
        for event in events {
            match *event {
                ContentCursor::Part(part_id) => {
                    if emitted.insert(part_id)
                        && let Some(part) = parts_by_id.get(&part_id)
                    {
                        out.push(part_json(
                            Some(session),
                            message.id,
                            part,
                            part_context,
                            tool_times,
                        ));
                    }
                }
                ContentCursor::StepStart { step } => {
                    let part = super::message_parts::step_start_part_id(message.id, step);
                    if !super::message_parts::deleted_part(deleted_parts, message.id, part) {
                        out.push(super::message_parts::step_start_part(
                            session, message.id, step,
                        ));
                    }
                }
                ContentCursor::StepFinish { step, finish } => {
                    let part = super::message_parts::step_finish_part_id(message.id, step);
                    if !super::message_parts::deleted_part(deleted_parts, message.id, part) {
                        out.push(super::message_parts::step_finish_part(
                            session, message.id, step, finish,
                        ));
                    }
                }
            }
        }
    }
    for part in &message.parts {
        if emitted.insert(part.id()) {
            out.push(part_json(
                session,
                message.id,
                part,
                part_context,
                tool_times,
            ));
        }
    }
    out
}

fn part_json(
    session: Option<SessionId>,
    message: MessageId,
    part: &PartProjection,
    part_context: &super::message_parts::OpenCodePartContext,
    tool_times: &BTreeMap<PartId, ToolTime>,
) -> Value {
    match part {
        PartProjection::Text { id, text } => json!({
            "type": "text",
            "id": id.to_string(),
            "text": text,
        }),
        PartProjection::Reasoning { id, text } => match session {
            Some(session) => json!({
                "id": id.to_string(),
                "sessionID": session.to_string(),
                "messageID": message.to_string(),
                "type": "reasoning",
                "text": text,
                "time": part_context.required_time(*id),
            }),
            None => json!({
                "type": "reasoning",
                "id": id.to_string(),
                "text": text,
                "time": part_context.required_time(*id),
            }),
        },
        PartProjection::Tool {
            id,
            call,
            name,
            state,
        } => {
            let mut value = json!({
                "type": "tool",
                "id": call.to_string(),
                "name": name.as_str(),
                "state": super::session_context_tool_state::tool_state(state),
                "time": super::session_context_tool_time::tool_time(tool_times.get(id).copied()),
            });
            if let Some(provider) = super::session_context_tool_state::tool_provider(state) {
                value["provider"] = provider;
            }
            value
        }
    }
}

fn text_content(parts: &[PartProjection]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message_content_events(envs: &[Envelope]) -> BTreeMap<MessageId, Vec<ContentCursor>> {
    let mut out: BTreeMap<MessageId, Vec<ContentCursor>> = BTreeMap::new();
    for env in envs {
        match &env.event {
            Event::StepStarted { message, step, .. } => {
                out.entry(*message)
                    .or_default()
                    .push(ContentCursor::StepStart { step: *step });
            }
            Event::StepFinished {
                message,
                step,
                finish,
                ..
            } => {
                out.entry(*message)
                    .or_default()
                    .push(ContentCursor::StepFinish {
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
                    .push(ContentCursor::Part(*part));
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
            | Event::MemberSpawned { .. }
            | Event::MemberStatusChanged { .. }
            | Event::MemberFinished { .. }
            | Event::Error { .. }
            | Event::Unknown => {}
        }
    }
    out
}

fn message_times(envs: &[Envelope]) -> BTreeMap<MessageId, MessageTime> {
    let mut out = BTreeMap::new();
    for env in envs {
        match env.event {
            Event::MessageStarted { message, .. } => {
                out.entry(message).or_insert(MessageTime {
                    created: Some(millis(env.ts_millis)),
                    completed: None,
                });
            }
            Event::MessageFinished { message, .. } => {
                out.entry(message).or_default().completed = Some(millis(env.ts_millis));
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
            | Event::MessageDeleted { .. }
            | Event::PartDeleted { .. }
            | Event::TextStart { .. }
            | Event::TextDelta { .. }
            | Event::TextReplace { .. }
            | Event::TextEnd { .. }
            | Event::ReasoningStart { .. }
            | Event::ReasoningDelta { .. }
            | Event::ReasoningEnd { .. }
            | Event::ReasoningReplace { .. }
            | Event::ToolInputStart { .. }
            | Event::ToolInputDelta { .. }
            | Event::ToolCallRequested { .. }
            | Event::ToolResult { .. }
            | Event::ToolError { .. }
            | Event::ToolPartUpdated { .. }
            | Event::MemberSpawned { .. }
            | Event::MemberStatusChanged { .. }
            | Event::MemberFinished { .. }
            | Event::Error { .. }
            | Event::Unknown => {}
        }
    }
    out
}

fn finish_name(finish: FinishReason) -> &'static str {
    match finish {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::Length => "length",
        FinishReason::Cancelled => "cancelled",
        FinishReason::Error => "error",
    }
}

fn millis(ts: i64) -> u64 {
    u64::try_from(ts).unwrap_or(0)
}
