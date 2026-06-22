use std::collections::BTreeMap;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::routing::get;
use futures::StreamExt;
use futures::stream;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use yaca_proto::{
    Envelope, Event, FinishReason, MessageId, PartId, Role, SessionId, ToolPartState,
};

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/event", get(subscribe))
        .route("/api/event", get(subscribe_api))
        .route("/global/event", get(subscribe_global))
}

#[derive(Serialize)]
struct EventPayload<T> {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    properties: T,
}

async fn subscribe(State(st): State<ServerState>) -> axum::response::Response {
    let connected = json_event(&EventPayload {
        id: event_id(),
        kind: "server.connected",
        properties: json!({}),
    });
    let initial = stream::once(async move { Ok::<_, Infallible>(connected) });
    let live_st = st.clone();
    let live = BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| {
        let st = live_st.clone();
        async move {
            match result {
                Ok(envelope) => Some(Ok(json_event(&envelope_payload(&st, envelope).await))),
                Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
            }
        }
    });
    super::sse::opencode(Sse::new(initial.chain(live)))
}

async fn subscribe_api(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let location = super::location::LocationRef::from_request(&query, &headers);
    let location_info = super::location::info_at(&st, &location);
    let requested_directory = super::location::workdir_at(&st, &location)
        .to_string_lossy()
        .into_owned();
    let connected = json_event(&json!({
        "id": event_id(),
        "type": "server.connected",
        "location": location_info.clone(),
        "data": {},
    }));
    let initial = stream::once(async move { Ok::<_, Infallible>(connected) });
    let live_st = st.clone();
    let live = BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| {
        let st = live_st.clone();
        let location_info = location_info.clone();
        let requested_directory = requested_directory.clone();
        async move {
            match result {
                Ok(envelope) => {
                    if !envelope_matches_location(&st, &requested_directory, &envelope).await {
                        return None;
                    }
                    let payload = envelope_payload(&st, envelope).await;
                    Some(Ok(json_event(&native_event_payload(
                        &location_info,
                        payload,
                    ))))
                }
                Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
            }
        }
    });
    super::sse::opencode(Sse::new(initial.chain(live)))
}

async fn subscribe_global(State(st): State<ServerState>) -> axum::response::Response {
    let connected = json_event(&json!({
        "payload": EventPayload {
            id: event_id(),
            kind: "server.connected",
            properties: json!({}),
        },
    }));
    let initial = stream::once(async move { Ok::<_, Infallible>(connected) });
    let live_st = st.clone();
    let live = BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| {
        let st = live_st.clone();
        async move {
            match result {
                Ok(envelope) => {
                    let payload = envelope_payload(&st, envelope).await;
                    Some(Ok(json_event(&json!({ "payload": payload }))))
                }
                Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
            }
        }
    });
    super::sse::opencode(Sse::new(initial.chain(live)))
}

async fn envelope_matches_location(
    st: &ServerState,
    requested_directory: &str,
    envelope: &Envelope,
) -> bool {
    let Some(session) = envelope.event.session() else {
        return true;
    };
    match super::load_session(st, session, None).await {
        Ok(snapshot) => snapshot.info.directory() == requested_directory,
        Err(_) => false,
    }
}

fn native_event_payload(location: &super::location::LocationInfo, payload: Value) -> Value {
    json!({
        "id": payload.get("id").cloned().unwrap_or_else(|| json!(event_id())),
        "type": payload.get("type").cloned().unwrap_or_else(|| json!("yaca.envelope")),
        "location": location,
        "data": payload.get("properties").cloned().unwrap_or_else(|| json!({})),
    })
}

async fn envelope_payload(st: &ServerState, envelope: Envelope) -> Value {
    match &envelope.event {
        Event::Error {
            session,
            code,
            message,
        } => session_error_payload(&envelope, *session, code, message),
        Event::SessionStatus { session, status } => {
            session_status_payload(&envelope, *session, status)
        }
        Event::SessionCreated { session, .. } => {
            session_payload(st, &envelope, *session, "session.created").await
        }
        Event::SessionTitled { session, .. }
        | Event::SessionMetadataSet { session, .. }
        | Event::SessionPermissionSet { session, .. }
        | Event::SessionArchived { session, .. }
        | Event::SessionShareSet { session, .. }
        | Event::SessionShareCleared { session }
        | Event::AgentSwitched { session, .. }
        | Event::ModelSwitched { session, .. } => {
            session_payload(st, &envelope, *session, "session.updated").await
        }
        Event::CommandExecuted {
            session,
            command,
            arguments,
            message,
        } => command_executed_payload(&envelope, *session, command, arguments, *message),
        Event::MessageStarted {
            session,
            message,
            role,
        } => message_payload(&envelope, *session, *message, *role, None)
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::MessageFinished {
            session,
            message,
            role,
            finish,
        } => message_payload(&envelope, *session, *message, *role, Some(*finish))
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::TextStart {
            session,
            message,
            part,
        } => textual_part_updated_payload(&envelope, *session, *message, *part, "text", ""),
        Event::TextDelta {
            session,
            message,
            part,
            delta,
        } => textual_part_delta_payload(&envelope, *session, *message, *part, delta),
        Event::TextReplace {
            session,
            message,
            part,
            text,
        } => textual_part_updated_payload(&envelope, *session, *message, *part, "text", text),
        Event::TextEnd {
            session,
            message,
            part,
        } => part_snapshot_payload(st, &envelope, *session, *message, *part, "text")
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::ReasoningStart {
            session,
            message,
            part,
        } => textual_part_updated_payload(&envelope, *session, *message, *part, "reasoning", ""),
        Event::ReasoningDelta {
            session,
            message,
            part,
            delta,
        } => textual_part_delta_payload(&envelope, *session, *message, *part, delta),
        Event::ReasoningReplace {
            session,
            message,
            part,
            text,
        } => textual_part_updated_payload(&envelope, *session, *message, *part, "reasoning", text),
        Event::ReasoningEnd {
            session,
            message,
            part,
        } => part_snapshot_payload(st, &envelope, *session, *message, *part, "reasoning")
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name,
        } => tool_part_updated_payload(
            &envelope,
            *session,
            *message,
            *part,
            call.to_string(),
            name.as_str(),
            json!({ "status": "pending", "input": {}, "raw": "" }),
        ),
        Event::ToolInputDelta {
            session,
            message,
            part,
            call,
            name,
            delta,
        } => tool_part_updated_payload(
            &envelope,
            *session,
            *message,
            *part,
            call.to_string(),
            name.as_str(),
            json!({ "status": "pending", "input": {}, "raw": delta }),
        ),
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input,
        } => tool_part_updated_payload(
            &envelope,
            *session,
            *message,
            *part,
            call.to_string(),
            name.as_str(),
            json!({
                "status": "running",
                "input": object_or_empty(input),
                "time": { "start": envelope.ts_millis },
            }),
        ),
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output,
            time_ms,
        } => {
            let elapsed = i64::try_from(*time_ms).unwrap_or(i64::MAX);
            tool_part_updated_payload(
                &envelope,
                *session,
                *message,
                *part,
                call.to_string(),
                "unknown",
                json!({
                    "status": "completed",
                    "input": {},
                    "output": tool_output_text(output),
                    "title": "",
                    "metadata": {},
                    "time": {
                        "start": envelope.ts_millis.saturating_sub(elapsed),
                        "end": envelope.ts_millis,
                    },
                }),
            )
        }
        Event::ToolError {
            session,
            message,
            part,
            call,
            message_text,
        } => tool_part_updated_payload(
            &envelope,
            *session,
            *message,
            *part,
            call.to_string(),
            "unknown",
            json!({
                "status": "error",
                "input": {},
                "error": message_text,
                "time": {
                    "start": envelope.ts_millis,
                    "end": envelope.ts_millis,
                },
            }),
        ),
        Event::ToolPartUpdated {
            session,
            message,
            part,
            state,
        } => tool_part_snapshot_payload(st, &envelope, *session, *message, *part, state)
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::PartDeleted {
            session,
            message,
            part,
        } => part_removed_payload(&envelope, *session, *message, *part),
        Event::MessageDeleted { session, message } => {
            message_removed_payload(&envelope, *session, *message)
        }
        _ => fallback_payload(&envelope),
    }
}

async fn session_payload(
    st: &ServerState,
    envelope: &Envelope,
    session: SessionId,
    kind: &'static str,
) -> Value {
    let session_id = session.to_string();
    let mut properties = json!({ "sessionID": session_id });
    if let Ok(snapshot) = super::load_session(st, session, None).await
        && let Some(object) = properties.as_object_mut()
    {
        object.insert("info".to_string(), json!(snapshot.info));
    }
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": kind,
        "properties": properties,
    })
}

async fn part_snapshot_payload(
    st: &ServerState,
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
    kind: &str,
) -> Option<Value> {
    let snapshot = super::load_session(st, session, None).await.ok()?;
    let part_id = part.to_string();
    let message_id = message.to_string();
    let part_value = snapshot
        .messages
        .iter()
        .find(|item| item.id() == message_id)
        .and_then(|item| item.part(&part_id))?;
    if part_value["type"].as_str() != Some(kind) {
        return None;
    }
    Some(json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.part.updated",
        "properties": {
            "sessionID": session.to_string(),
            "part": part_value,
            "time": envelope.ts_millis,
        },
    }))
}

async fn tool_part_snapshot_payload(
    st: &ServerState,
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
    state: &ToolPartState,
) -> Option<Value> {
    let snapshot = super::load_session(st, session, None).await.ok()?;
    let part_id = part.to_string();
    let message_id = message.to_string();
    let part_value = snapshot
        .messages
        .iter()
        .find(|item| item.id() == message_id)
        .and_then(|item| item.part(&part_id))?;
    if part_value["type"].as_str() != Some("tool") {
        return None;
    }
    let call = part_value["callID"].as_str()?.to_string();
    let tool = part_value["tool"].as_str()?;
    Some(tool_part_updated_payload(
        envelope,
        session,
        message,
        part,
        call,
        tool,
        tool_state_payload(envelope, state),
    ))
}

fn message_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    role: Role,
    finish: Option<FinishReason>,
) -> Option<Value> {
    let session_id = session.to_string();
    let message_id = message.to_string();
    let role = match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => return None,
    };
    let mut info = match role {
        "user" => json!({
            "id": message_id,
            "sessionID": session_id,
            "role": role,
            "time": { "created": envelope.ts_millis },
            "agent": "yaca",
            "model": { "providerID": "yaca", "modelID": "unknown" },
        }),
        _ => json!({
            "id": message_id,
            "sessionID": session_id,
            "role": role,
            "time": { "created": envelope.ts_millis },
            "parentID": "",
            "modelID": "unknown",
            "providerID": "yaca",
            "mode": "build",
            "agent": "yaca",
            "path": { "cwd": "", "root": "" },
            "cost": 0,
            "tokens": {
                "input": 0,
                "output": 0,
                "reasoning": 0,
                "cache": { "read": 0, "write": 0 },
            },
        }),
    };
    if let Some(finish) = finish
        && role == "assistant"
        && let Some(object) = info.as_object_mut()
    {
        object.insert("finish".to_string(), json!(finish_name(finish)));
        if let Some(time) = object.get_mut("time").and_then(Value::as_object_mut) {
            time.insert("completed".to_string(), json!(envelope.ts_millis));
        }
    }
    Some(json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.updated",
        "properties": {
            "sessionID": session.to_string(),
            "info": info,
        },
    }))
}

fn textual_part_updated_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
    kind: &'static str,
    text: &str,
) -> Value {
    let session_id = session.to_string();
    let message_id = message.to_string();
    let part_id = part.to_string();
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.part.updated",
        "properties": {
            "sessionID": session_id,
            "part": {
                "id": part_id,
                "sessionID": session_id,
                "messageID": message_id,
                "type": kind,
                "text": text,
                "time": { "start": envelope.ts_millis },
            },
            "time": envelope.ts_millis,
        },
    })
}

fn textual_part_delta_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
    delta: &str,
) -> Value {
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.part.delta",
        "properties": {
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "partID": part.to_string(),
            "field": "text",
            "delta": delta,
        },
    })
}

fn tool_part_updated_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
    call: String,
    tool: &str,
    state: Value,
) -> Value {
    let session_id = session.to_string();
    let message_id = message.to_string();
    let part_id = part.to_string();
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.part.updated",
        "properties": {
            "sessionID": session_id,
            "part": {
                "id": part_id,
                "sessionID": session_id,
                "messageID": message_id,
                "type": "tool",
                "callID": call,
                "tool": tool,
                "state": state,
            },
            "time": envelope.ts_millis,
        },
    })
}

fn tool_state_payload(envelope: &Envelope, state: &ToolPartState) -> Value {
    match state {
        ToolPartState::Pending { input } => json!({
            "status": "pending",
            "input": object_or_empty(input),
        }),
        ToolPartState::Running { input } => json!({
            "status": "running",
            "input": object_or_empty(input),
            "time": { "start": envelope.ts_millis },
        }),
        ToolPartState::Completed {
            input,
            output,
            time_ms,
        } => {
            let elapsed = i64::try_from(*time_ms).unwrap_or(i64::MAX);
            json!({
                "status": "completed",
                "input": object_or_empty(input),
                "output": tool_output_text(output),
                "title": "",
                "metadata": {},
                "time": {
                    "start": envelope.ts_millis.saturating_sub(elapsed),
                    "end": envelope.ts_millis,
                },
            })
        }
        ToolPartState::Error { input, message } => json!({
            "status": "error",
            "input": object_or_empty(input),
            "error": message,
            "time": {
                "start": envelope.ts_millis,
                "end": envelope.ts_millis,
            },
        }),
    }
}

fn object_or_empty(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
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

fn part_removed_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
) -> Value {
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.part.removed",
        "properties": {
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
            "partID": part.to_string(),
        },
    })
}

fn message_removed_payload(envelope: &Envelope, session: SessionId, message: MessageId) -> Value {
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "message.removed",
        "properties": {
            "sessionID": session.to_string(),
            "messageID": message.to_string(),
        },
    })
}

fn command_executed_payload(
    envelope: &Envelope,
    session: SessionId,
    command: &str,
    arguments: &str,
    message: MessageId,
) -> Value {
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "command.executed",
        "properties": {
            "name": command,
            "sessionID": session.to_string(),
            "arguments": arguments,
            "messageID": message.to_string(),
        },
    })
}

fn fallback_payload(envelope: &Envelope) -> Value {
    serde_json::to_value(EventPayload {
        id: format!("evt_yaca_{}", envelope.seq.0),
        kind: "yaca.envelope",
        properties: envelope,
    })
    .unwrap_or_else(|_| json!({}))
}

fn finish_name(finish: FinishReason) -> &'static str {
    match finish {
        FinishReason::Stop => "stop",
        FinishReason::ToolCalls => "tool-calls",
        FinishReason::Length => "length",
        FinishReason::Cancelled => "cancelled",
        FinishReason::Error => "error",
    }
}

fn session_error_payload(
    envelope: &Envelope,
    session: Option<SessionId>,
    code: &str,
    message: &str,
) -> Value {
    let mut error_data = json!({ "message": message });
    if !code.is_empty()
        && let Some(object) = error_data.as_object_mut()
    {
        object.insert("ref".to_string(), json!(code));
    }
    let mut data = json!({
        "error": {
            "name": "UnknownError",
            "data": error_data,
        },
    });
    if let Some(session) = session
        && let Some(object) = data.as_object_mut()
    {
        object.insert("sessionID".to_string(), json!(session.to_string()));
    }
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "session.error",
        "properties": data,
    })
}

fn session_status_payload(envelope: &Envelope, session: SessionId, status: &Value) -> Value {
    json!({
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "session.status",
        "properties": {
            "sessionID": session.to_string(),
            "status": status,
        },
    })
}

fn json_event<T: Serialize>(payload: &T) -> SseEvent {
    SseEvent::default()
        .json_data(payload)
        .unwrap_or_else(|_| SseEvent::default().data("{}"))
}

fn event_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("evt_{nanos}")
}
