//! Conversions between the event-sourced domain types (`hya-proto`) and
//! the generated `hya.v1` wire types, plus the curated envelope →
//! `StreamEvent` projection used by replay and both live streams.

use hya_api::v1 as pb;
use hya_proto::projection::{MessageProjection, PartProjection, Projection};
use hya_proto::{Envelope, Event, Role};
use hya_proto::{FinishReason, ToolPartState};

/// Map a domain finish reason to the wire enum.
pub(crate) fn finish_reason(finish: FinishReason) -> i32 {
    match finish {
        FinishReason::Stop => pb::FinishReason::Stop as i32,
        FinishReason::ToolCalls => pb::FinishReason::ToolCalls as i32,
        FinishReason::Length => pb::FinishReason::Length as i32,
        FinishReason::Cancelled => pb::FinishReason::Cancelled as i32,
        FinishReason::Error => pb::FinishReason::Error as i32,
    }
}

/// Map a domain role to the wire enum.
pub(crate) fn wire_role(role: Role) -> i32 {
    match role {
        Role::User => pb::Role::User as i32,
        Role::Assistant => pb::Role::Assistant as i32,
        Role::System => pb::Role::System as i32,
    }
}

/// Map a domain tool-part state to the wire enum.
pub(crate) fn tool_state(state: &ToolPartState) -> i32 {
    match state {
        ToolPartState::Pending { .. } => pb::ToolExecutionState::Pending as i32,
        ToolPartState::Running { .. } => pb::ToolExecutionState::Running as i32,
        ToolPartState::Completed { .. } => pb::ToolExecutionState::Ok as i32,
        ToolPartState::Error { .. } => pb::ToolExecutionState::Error as i32,
    }
}

/// Map a projected session to the wire session summary.
///
/// `info` supplies wall-clock timestamps from the store's session listing.
pub(crate) fn session_info(
    projection: &Projection,
    started_millis: i64,
    updated_millis: i64,
) -> pb::SessionInfo {
    let session = &projection.session;
    pb::SessionInfo {
        id: session
            .id
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        parent: session
            .parent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        title: session.title.clone().unwrap_or_default(),
        agent: session
            .agent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        model: session
            .model
            .as_ref()
            .map(|model| model.to_string())
            .and_then(|reference| super::catalog::model_ref(&reference)),
        workdir: session.workdir.clone().unwrap_or_default(),
        background: false,
        time_created: timestamp(started_millis),
        time_updated: timestamp(updated_millis),
        last_seq: projection.last_seq,
    }
}

/// Unix milliseconds → wire timestamp.
pub(crate) fn timestamp(millis: i64) -> Option<pbjson_types::Timestamp> {
    Some(pbjson_types::Timestamp {
        seconds: millis.div_euclid(1000),
        nanos: (millis.rem_euclid(1000) * 1_000_000) as i32,
    })
}

/// Convert a JSON object into the wire `Struct` form.
pub(crate) fn to_struct(value: serde_json::Value) -> pbjson_types::Struct {
    let map = match value {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    pbjson_types::Struct {
        fields: map
            .into_iter()
            .map(|(key, value)| (key, to_pb_value(value)))
            .collect(),
    }
}

fn to_pb_value(value: serde_json::Value) -> pbjson_types::Value {
    use pbjson_types::value::Kind;
    use serde_json::Value as Json;
    let kind = match value {
        Json::Null => Kind::NullValue(0),
        Json::Bool(flag) => Kind::BoolValue(flag),
        Json::Number(number) => Kind::NumberValue(number.as_f64().unwrap_or(0.0)),
        Json::String(text) => Kind::StringValue(text),
        Json::Array(items) => Kind::ListValue(pbjson_types::ListValue {
            values: items.into_iter().map(to_pb_value).collect(),
        }),
        Json::Object(map) => Kind::StructValue(to_struct(Json::Object(map))),
    };
    pbjson_types::Value { kind: Some(kind) }
}

/// Convert a wire `Struct` back into a JSON object.
pub(crate) fn from_struct(value: &pbjson_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(
        value
            .fields
            .iter()
            .map(|(key, value)| (key.clone(), from_pb_value(value)))
            .collect(),
    )
}

fn from_pb_value(value: &pbjson_types::Value) -> serde_json::Value {
    use pbjson_types::value::Kind;
    use serde_json::Value as Json;
    match value.kind.as_ref() {
        Some(Kind::NullValue(_)) | None => Json::Null,
        Some(Kind::BoolValue(flag)) => Json::Bool(*flag),
        Some(Kind::NumberValue(number)) => serde_json::Number::from_f64(*number)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Some(Kind::StringValue(text)) => Json::String(text.clone()),
        Some(Kind::ListValue(list)) => Json::Array(list.values.iter().map(from_pb_value).collect()),
        Some(Kind::StructValue(map)) => from_struct(map),
    }
}

/// Map a projected message to the wire message view.
pub(crate) fn message(message: &MessageProjection) -> pb::MessageInfo {
    pb::MessageInfo {
        id: message.id.to_string(),
        session: String::new(),
        role: wire_role(message.role),
        agent: String::new(),
        model: String::new(),
        finish: message.finish.map(finish_reason).unwrap_or(0),
        parts: message.parts.iter().filter_map(part).collect(),
        time_created: None,
        time_updated: None,
    }
}

/// Map a projected part; media-free projections always map.
fn part(part: &PartProjection) -> Option<pb::PartInfo> {
    use hya_api::v1::part_info::Kind as K;
    let (id, kind) = match part {
        PartProjection::Text { id, text } => {
            (id.to_string(), K::Text(pb::TextPart { text: text.clone() }))
        }
        PartProjection::Reasoning {
            id, text, reason, ..
        } => (
            id.to_string(),
            K::Reasoning(pb::ReasoningPart {
                text: text.clone(),
                variant: reason.clone().unwrap_or_default(),
            }),
        ),
        PartProjection::Tool {
            id, name, state, ..
        } => (
            id.to_string(),
            K::ToolCall(pb::ToolCallPart {
                call_id: String::new(),
                tool: name.to_string(),
                input_json: String::new(),
                state: tool_state(state),
            }),
        ),
    };
    Some(pb::PartInfo {
        id,
        kind: Some(kind),
    })
}

/// Project one envelope onto the curated wire event stream.
///
/// Returns `None` for internal-only events that the v1 surface does not
/// expose. Both transports serialize the result identically.
pub(crate) fn stream_event(envelope: &Envelope) -> Option<pb::StreamEvent> {
    use hya_api::v1::stream_event::Payload as P;
    let session = envelope
        .event
        .session()
        .map(|id| id.to_string())
        .unwrap_or_default();
    let payload = match &envelope.event {
        Event::SessionCreated {
            agent,
            model,
            workdir,
            parent,
            ..
        } => P::SessionStarted(pb::SessionStarted {
            agent: agent.to_string(),
            model: model.to_string(),
            workdir: workdir.clone(),
            parent: parent.as_ref().map(ToString::to_string).unwrap_or_default(),
        }),
        Event::SessionTitled { title, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: Some(title.clone()),
            model: None,
            agent: None,
            background: None,
        }),
        Event::AgentSwitched { agent, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: None,
            model: None,
            agent: Some(agent.to_string()),
            background: None,
        }),
        Event::ModelSwitched { model, .. } => P::SessionUpdated(pb::SessionUpdated {
            title: None,
            model: Some(model.to_string()),
            agent: None,
            background: None,
        }),
        Event::MessageStarted { message, role, .. } => P::MessageStarted(pb::MessageStarted {
            message: message.to_string(),
            role: wire_role(*role),
            agent: String::new(),
            model: String::new(),
        }),
        Event::MessageFinished {
            message, finish, ..
        } => P::MessageFinished(pb::MessageFinished {
            message: message.to_string(),
            finish: finish_reason(*finish),
            usage: None,
        }),
        Event::TextStart { message, part, .. } => {
            P::PartStarted(part_started(message, part, "text"))
        }
        Event::TextDelta {
            message,
            part,
            delta,
            ..
        } => P::PartAppended(pb::PartAppended {
            message: message.to_string(),
            part: part.to_string(),
            text_delta: delta.clone(),
        }),
        Event::TextEnd { message, part, .. } => P::PartCompleted(pb::PartCompleted {
            message: message.to_string(),
            part: part.to_string(),
        }),
        Event::ReasoningStart { message, part, .. } => {
            P::PartStarted(part_started(message, part, "reasoning"))
        }
        Event::ReasoningDelta {
            message,
            part,
            delta,
            ..
        } => P::PartAppended(pb::PartAppended {
            message: message.to_string(),
            part: part.to_string(),
            text_delta: delta.clone(),
        }),
        Event::ReasoningEnd { message, part, .. } => P::PartCompleted(pb::PartCompleted {
            message: message.to_string(),
            part: part.to_string(),
        }),
        Event::ToolInputStart { message, part, .. } => {
            P::PartStarted(part_started(message, part, "tool_call"))
        }
        Event::ToolPartUpdated {
            message,
            part,
            state,
            ..
        } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: String::new(),
            state: tool_state(state),
            error_code: String::new(),
        }),
        Event::ToolResult { message, part, .. } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: String::new(),
            state: pb::ToolExecutionState::Ok as i32,
            error_code: String::new(),
        }),
        Event::ToolError { message, part, .. } => P::ToolStateChanged(pb::ToolStateChanged {
            message: message.to_string(),
            part: part.to_string(),
            call_id: String::new(),
            state: pb::ToolExecutionState::Error as i32,
            error_code: "tool_error".to_owned(),
        }),
        Event::ContextCompacted { strategy, .. } => P::CompactionApplied(pb::CompactionApplied {
            until_seq: envelope.seq.0,
            strategy: format!("{strategy:?}"),
        }),
        Event::WorkflowSelected { .. }
        | Event::WorkflowRunStarted { .. }
        | Event::WorkflowStageStarted { .. }
        | Event::WorkflowStageFinished { .. }
        | Event::WorkflowRunFinished { .. } => {
            P::WorkflowUpdated(pb::WorkflowUpdated { state: None })
        }
        _ => return None,
    };
    Some(pb::StreamEvent {
        seq: envelope.seq.0,
        session,
        time_recorded: timestamp(envelope.ts_millis),
        payload: Some(payload),
    })
}

fn part_started(
    message: &hya_proto::MessageId,
    part: &hya_proto::PartId,
    kind: &str,
) -> pb::PartStarted {
    pb::PartStarted {
        message: message.to_string(),
        part: part.to_string(),
        kind: kind.to_owned(),
    }
}
