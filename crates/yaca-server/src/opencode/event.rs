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

use super::location::LocationInfo;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/event", get(subscribe))
        .route("/global/event", get(subscribe))
}

#[derive(Serialize)]
struct EventPayload<T> {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<LocationInfo>,
    data: T,
}

async fn subscribe(
    State(st): State<ServerState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let connected = json_event(&EventPayload {
        id: event_id(),
        kind: "server.connected",
        location: Some(super::location::info(&st)),
        data: json!({}),
    });
    let initial = stream::once(async move { Ok(connected) });
    let live = BroadcastStream::new(st.engine.bus().subscribe()).filter_map(|result| async move {
        match result {
            Ok(envelope) => Some(Ok(json_event(&envelope_payload(envelope)))),
            Err(_lagged) => Some(Ok(SseEvent::default().event("resync"))),
        }
    });
    Sse::new(initial.chain(live))
}

fn envelope_payload(envelope: Envelope) -> Value {
    if let Event::Error {
        session,
        code,
        message,
    } = &envelope.event
    {
        return session_error_payload(&envelope, *session, code, message);
    }
    serde_json::to_value(EventPayload {
        id: format!("evt_yaca_{}", envelope.seq.0),
        kind: "yaca.envelope",
        location: None,
        data: envelope,
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
        "id": format!("evt_yaca_{}", envelope.seq.0),
        "type": "session.error",
        "properties": data,
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
