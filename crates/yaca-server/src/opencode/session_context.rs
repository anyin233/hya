use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};
use yaca_proto::{
    Envelope, Event, FinishReason, MessageId, MessageProjection, PartProjection, Projection, Role,
    ToolPartState,
};

use crate::{ApiError, ServerState, parse_session};

#[derive(Serialize)]
struct ContextResponse {
    data: Vec<Value>,
}

#[derive(Clone, Copy, Default)]
struct MessageTime {
    created: Option<u64>,
    completed: Option<u64>,
}

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/api/session/:id/context", get(context))
}

async fn context(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<ContextResponse>, ApiError> {
    let session = parse_session(&id)?;
    let envs = st.engine.replay(session).await?;
    if envs.is_empty() {
        return Err(ApiError::not_found("session not found"));
    }
    let projection = Projection::from_events(&envs);
    Ok(Json(ContextResponse {
        data: v2_messages(&envs, &projection),
    }))
}

pub(in crate::opencode) fn v2_messages(envs: &[Envelope], projection: &Projection) -> Vec<Value> {
    let times = message_times(envs);
    projection
        .session
        .messages
        .iter()
        .map(|message| message_json(message, projection, &times))
        .collect()
}

fn message_json(
    message: &MessageProjection,
    projection: &Projection,
    times: &BTreeMap<MessageId, MessageTime>,
) -> Value {
    let time = times.get(&message.id).copied().unwrap_or_default();
    match message.role {
        Role::User => json!({
            "id": message.id.to_string(),
            "time": { "created": time.created.unwrap_or(0) },
            "text": text_content(&message.parts),
            "files": [],
            "agents": [],
            "type": "user",
        }),
        Role::Assistant => assistant_message(message, projection, time),
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
) -> Value {
    let mut value = json!({
        "id": message.id.to_string(),
        "time": {
            "created": time.created.unwrap_or(0),
            "completed": time.completed,
        },
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
        "content": message.parts.iter().map(part_json).collect::<Vec<_>>(),
    });
    if let Some(finish) = message.finish {
        value["finish"] = json!(finish_name(finish));
    }
    value
}

fn part_json(part: &PartProjection) -> Value {
    match part {
        PartProjection::Text { id, text } => json!({
            "type": "text",
            "id": id.to_string(),
            "text": text,
        }),
        PartProjection::Reasoning { id, text } => json!({
            "type": "reasoning",
            "id": id.to_string(),
            "text": text,
        }),
        PartProjection::Tool {
            id, name, state, ..
        } => json!({
            "type": "tool",
            "id": id.to_string(),
            "name": name.as_str(),
            "state": tool_state(state),
            "time": { "created": 0 },
        }),
    }
}

fn tool_state(state: &ToolPartState) -> Value {
    match state {
        ToolPartState::Pending { input } => json!({
            "status": "pending",
            "input": input.to_string(),
        }),
        ToolPartState::Running { input } => json!({
            "status": "running",
            "input": input,
            "structured": {},
            "content": [],
        }),
        ToolPartState::Completed { input, output, .. } => json!({
            "status": "completed",
            "input": input,
            "content": [],
            "structured": {},
            "result": output,
        }),
        ToolPartState::Error { input, message } => json!({
            "status": "error",
            "input": input,
            "content": [],
            "structured": {},
            "error": { "name": "ToolError", "message": message },
        }),
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
            | Event::SessionTitled { .. }
            | Event::SessionMetadataSet { .. }
            | Event::SessionPermissionSet { .. }
            | Event::SessionArchived { .. }
            | Event::SessionShareSet { .. }
            | Event::SessionShareCleared { .. }
            | Event::AgentSwitched { .. }
            | Event::ModelSwitched { .. }
            | Event::SessionStatus { .. }
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
            | Event::Error { .. } => {}
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
