use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::response::sse::{Event as SseEvent, Sse};
use axum::routing::get;
use futures::stream;
use futures::{Stream, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use yaca_proto::{Envelope, Event, SessionId};

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/event", get(subscribe))
        .route("/api/event", get(subscribe))
        .route("/global/event", get(subscribe))
}

#[derive(Serialize)]
struct EventPayload<T> {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    properties: T,
}

async fn subscribe(
    State(st): State<ServerState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let connected = json_event(&EventPayload {
        id: event_id(),
        kind: "server.connected",
        properties: json!({}),
    });
    let initial = stream::once(async move { Ok(connected) });
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
    Sse::new(initial.chain(live))
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
        Event::SessionTitled { session, .. } => {
            session_payload(st, &envelope, *session, "session.updated").await
        }
        _ => serde_json::to_value(EventPayload {
            id: format!("evt_yaca_{}", envelope.seq.0),
            kind: "yaca.envelope",
            properties: envelope,
        })
        .unwrap_or_else(|_| json!({})),
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
