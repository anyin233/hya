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
use hya_proto::{Envelope, Event, FinishReason, MessageId, PartId, Role, SessionId};
use serde::Serialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;

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
    let permissions =
        BroadcastStream::new(st.permission_requests.subscribe()).filter_map(|result| async move {
            match result {
                Ok(value) => Some(Ok(json_event(&value))),
                Err(_lagged) => None,
            }
        });
    let questions =
        BroadcastStream::new(st.question_requests.subscribe()).filter_map(|result| async move {
            match result {
                Ok(value) => Some(Ok(json_event(&value))),
                Err(_lagged) => None,
            }
        });
    super::sse::compat(Sse::new(initial.chain(stream::select(
        stream::select(stream::select(live, permissions), questions),
        super::event_heartbeat::stream(heartbeat_event),
    ))))
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
    let connected = json_event(&EventPayload {
        id: event_id(),
        kind: "server.connected",
        properties: json!({}),
    });
    let initial = stream::once(async move { Ok::<_, Infallible>(connected) });
    let live_st = st.clone();
    let perm_location = location_info.clone();
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
                    let payload = api_envelope_payload(&st, envelope).await;
                    Some(Ok(json_event(&native_event_payload(
                        &location_info,
                        payload,
                    ))))
                }
                Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
            }
        }
    });
    let question_location = perm_location.clone();
    let permissions =
        BroadcastStream::new(st.permission_requests.subscribe()).filter_map(move |result| {
            let location_info = perm_location.clone();
            async move {
                match result {
                    Ok(value) => Some(Ok(json_event(&native_event_payload(&location_info, value)))),
                    Err(_lagged) => None,
                }
            }
        });
    let questions =
        BroadcastStream::new(st.question_requests.subscribe()).filter_map(move |result| {
            let location_info = question_location.clone();
            async move {
                match result {
                    Ok(value) => Some(Ok(json_event(&native_event_payload(&location_info, value)))),
                    Err(_lagged) => None,
                }
            }
        });
    super::sse::compat(Sse::new(initial.chain(stream::select(
        stream::select(stream::select(live, permissions), questions),
        super::event_heartbeat::stream(heartbeat_event),
    ))))
}

async fn subscribe_global(State(st): State<ServerState>) -> axum::response::Response {
    let directory = super::location::workdir(&st).to_string_lossy().into_owned();
    let connected = json_event(&global_event_payload(
        &directory,
        EventPayload {
            id: event_id(),
            kind: "server.connected",
            properties: json!({}),
        },
    ));
    let initial = stream::once(async move { Ok::<_, Infallible>(connected) });
    let live_st = st.clone();
    let live_directory = directory.clone();
    let live = BroadcastStream::new(st.engine.bus().subscribe()).filter_map(move |result| {
        let st = live_st.clone();
        let directory = live_directory.clone();
        async move {
            match result {
                Ok(envelope) => {
                    let payload = api_envelope_payload(&st, envelope).await;
                    Some(Ok(json_event(&global_event_payload(&directory, payload))))
                }
                Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
            }
        }
    });
    let permission_directory = directory.clone();
    let permissions =
        BroadcastStream::new(st.permission_requests.subscribe()).filter_map(move |result| {
            let directory = permission_directory.clone();
            async move {
                match result {
                    Ok(value) => Some(Ok(json_event(&global_event_payload(&directory, value)))),
                    Err(_lagged) => None,
                }
            }
        });
    let question_directory = directory.clone();
    let questions =
        BroadcastStream::new(st.question_requests.subscribe()).filter_map(move |result| {
            let directory = question_directory.clone();
            async move {
                match result {
                    Ok(value) => Some(Ok(json_event(&global_event_payload(&directory, value)))),
                    Err(_lagged) => None,
                }
            }
        });
    let heartbeat_directory = directory;
    super::sse::compat(Sse::new(initial.chain(stream::select(
        stream::select(stream::select(live, permissions), questions),
        super::event_heartbeat::stream(move || global_heartbeat_event(&heartbeat_directory)),
    ))))
}

fn global_event_payload<T: Serialize>(directory: &str, payload: T) -> Value {
    json!({
        "directory": directory,
        "payload": payload,
    })
}

fn global_heartbeat_event(directory: &str) -> SseEvent {
    json_event(&global_event_payload(
        directory,
        EventPayload {
            id: event_id(),
            kind: "server.heartbeat",
            properties: json!({}),
        },
    ))
}

fn heartbeat_event() -> SseEvent {
    json_event(&EventPayload {
        id: event_id(),
        kind: "server.heartbeat",
        properties: json!({}),
    })
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
        "type": payload.get("type").cloned().unwrap_or_else(|| json!("hya.envelope")),
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
        | Event::SessionMoved { session, .. }
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
        Event::StepStarted {
            session,
            message,
            step,
        } => step_part_updated_payload(
            &envelope,
            *session,
            super::message_parts::step_start_part(*session, *message, *step),
        ),
        Event::StepFinished {
            session,
            message,
            step,
            finish,
        } => step_part_updated_payload(
            &envelope,
            *session,
            super::message_parts::step_finish_part(*session, *message, *step, *finish),
        ),
        Event::MessageStarted {
            session,
            message,
            role,
        } => message_payload(st, &envelope, *session, *message, *role, false)
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::MessageFinished {
            session,
            message,
            role,
            ..
        } => message_payload(st, &envelope, *session, *message, *role, true)
            .await
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
            ..
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
            ..
        } => part_snapshot_payload(st, &envelope, *session, *message, *part, "tool")
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::ToolError {
            session,
            message,
            part,
            ..
        } => part_snapshot_payload(st, &envelope, *session, *message, *part, "tool")
            .await
            .unwrap_or_else(|| fallback_payload(&envelope)),
        Event::ToolPartUpdated {
            session,
            message,
            part,
            ..
        } => part_snapshot_payload(st, &envelope, *session, *message, *part, "tool")
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

async fn api_envelope_payload(st: &ServerState, envelope: Envelope) -> Value {
    match &envelope.event {
        Event::StepStarted {
            session, message, ..
        } => step_started_event_payload(st, &envelope, *session, *message).await,
        Event::StepFinished {
            session,
            message,
            finish,
            ..
        } => step_ended_event_payload(&envelope, *session, *message, *finish),
        Event::Error { .. }
        | Event::SessionStatus { .. }
        | Event::SessionCreated { .. }
        | Event::SessionTitled { .. }
        | Event::SessionMoved { .. }
        | Event::SessionMetadataSet { .. }
        | Event::SessionPermissionSet { .. }
        | Event::SessionArchived { .. }
        | Event::SessionShareSet { .. }
        | Event::SessionShareCleared { .. }
        | Event::AgentSwitched { .. }
        | Event::ModelSwitched { .. }
        | Event::CommandExecuted { .. }
        | Event::MessageStarted { .. }
        | Event::MessageFinished { .. }
        | Event::TextStart { .. }
        | Event::TextDelta { .. }
        | Event::TextReplace { .. }
        | Event::TextEnd { .. }
        | Event::ReasoningStart { .. }
        | Event::ReasoningDelta { .. }
        | Event::ReasoningReplace { .. }
        | Event::ReasoningEnd { .. }
        | Event::ToolInputStart { .. }
        | Event::ToolInputDelta { .. }
        | Event::ToolCallRequested { .. }
        | Event::ToolResult { .. }
        | Event::ToolError { .. }
        | Event::ToolPartUpdated { .. }
        | Event::PartDeleted { .. }
        | Event::MessageDeleted { .. }
        | Event::UserPromptContextRecorded { .. }
        | Event::MemberSpawned { .. }
        | Event::MemberStatusChanged { .. }
        | Event::MemberFinished { .. }
        | Event::AgentRegistered { .. }
        | Event::AgentActivityChanged { .. }
        | Event::MailSent { .. }
        | Event::ChannelJoined { .. }
        | Event::ChannelLeft { .. }
        | Event::Unknown => envelope_payload(st, envelope).await,
    }
}

async fn step_started_event_payload(
    st: &ServerState,
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
) -> Value {
    let (agent, model) = step_agent_model(st, session).await;
    json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": "session.next.step.started",
        "properties": {
            "timestamp": envelope.ts_millis,
            "sessionID": session.to_string(),
            "assistantMessageID": message.to_string(),
            "agent": agent,
            "model": model,
        },
    })
}

fn step_ended_event_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    finish: FinishReason,
) -> Value {
    json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": "session.next.step.ended",
        "properties": {
            "timestamp": envelope.ts_millis,
            "sessionID": session.to_string(),
            "assistantMessageID": message.to_string(),
            "finish": super::message_parts::step_finish_name(finish),
            "cost": 0,
            "tokens": super::message_parts::empty_tokens(),
        },
    })
}

async fn step_agent_model(st: &ServerState, session: SessionId) -> (String, Value) {
    match st.engine.store().read_projection(session).await {
        Ok(projection) => {
            let agent = projection
                .session
                .agent
                .unwrap_or_else(|| st.agent.name.clone())
                .to_string();
            let model = projection
                .session
                .model
                .unwrap_or_else(|| st.agent.model.clone());
            (agent, json!(super::projection::model_info(&model)))
        }
        Err(_) => (
            st.agent.name.to_string(),
            json!(super::projection::model_info(&st.agent.model)),
        ),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": kind,
        "properties": properties,
    })
}

fn step_part_updated_payload(envelope: &Envelope, session: SessionId, part: Value) -> Value {
    json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": "message.part.updated",
        "properties": {
            "sessionID": session.to_string(),
            "part": part,
            "time": envelope.ts_millis,
        },
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
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": "message.part.updated",
        "properties": {
            "sessionID": session.to_string(),
            "part": part_value,
            "time": envelope.ts_millis,
        },
    }))
}

// Reuse the projected REST info so events carry the real agent/model. `finish`/
// `completed` are point-in-time: strip them until the finished event so a late
// snapshot read can't leak a completion into the started event.
async fn message_payload(
    st: &ServerState,
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    role: Role,
    finished: bool,
) -> Option<Value> {
    if matches!(role, Role::System) {
        return None;
    }
    let message_id = message.to_string();
    let mut info = super::load_session(st, session, None)
        .await
        .ok()?
        .messages
        .iter()
        .find(|item| item.id() == message_id)
        .map(|item| item.info())?;
    if !finished && let Some(object) = info.as_object_mut() {
        object.remove("finish");
        if let Some(time) = object.get_mut("time").and_then(Value::as_object_mut) {
            time.remove("completed");
        }
    }
    Some(json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
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

fn object_or_empty(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
}

fn part_removed_payload(
    envelope: &Envelope,
    session: SessionId,
    message: MessageId,
    part: PartId,
) -> Value {
    json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        "id": format!("evt_hya_{}", envelope.seq.0),
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
        id: format!("evt_hya_{}", envelope.seq.0),
        kind: "hya.envelope",
        properties: envelope,
    })
    .unwrap_or_else(|_| json!({}))
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
        "id": format!("evt_hya_{}", envelope.seq.0),
        "type": "session.error",
        "properties": data,
    })
}

fn session_status_payload(envelope: &Envelope, session: SessionId, status: &Value) -> Value {
    json!({
        "id": format!("evt_hya_{}", envelope.seq.0),
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
