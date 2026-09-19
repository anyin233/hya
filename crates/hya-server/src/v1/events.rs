//! `/v1` events domain: replay, session SSE stream, and the global stream.
//!
//! All three surfaces emit the same curated `StreamFrame` protojson; the
//! typed `resync` frame replaces the legacy SSE event-name signal.

use std::collections::BTreeMap;
use std::convert::Infallible;

use axum::extract::{Path as AxumPath, Query, State};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::StreamExt;
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;

use crate::ServerState;
use hya_api::v1 as pb;
use hya_proto::SessionId;

use super::V1Error;
use super::convert::stream_event;
use super::session::parse_session;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions/:id/events", get(list_events))
        .route("/v1/sessions/:id/events/stream", get(stream_session))
        .route("/v1/events/stream", get(stream_global))
}

async fn list_events(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListEventsResponse>, V1Error> {
    let request: pb::ListEventsRequest = super::query_request(&[("session", id.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    let envelopes = st.engine.replay(session).await?;
    if envelopes.is_empty() {
        return Err(V1Error::session_not_found(&request.session));
    }
    let limit = if request.limit == 0 {
        usize::MAX
    } else {
        request.limit as usize
    };
    let mut events = Vec::new();
    let mut next_seq = request.since_seq;
    for envelope in envelopes
        .into_iter()
        .filter(|envelope| envelope.seq.0 > request.since_seq)
        .take(limit)
    {
        next_seq = next_seq.max(envelope.seq.0);
        if let Some(event) = stream_event(&envelope) {
            events.push(event);
        }
    }
    Ok(Json(pb::ListEventsResponse {
        session: session.to_string(),
        events,
        next_seq,
    }))
}

async fn stream_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, V1Error> {
    let request: pb::StreamSessionEventsRequest =
        super::query_request(&[("session", id.as_str())], &query)?;
    let session = parse_session(&request.session)?;
    if !st.engine.session_exists(session).await? {
        return Err(V1Error::session_not_found(&request.session));
    }
    Ok(session_stream(st, Some(session), request.since_seq).into_response())
}

async fn stream_global(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Response, V1Error> {
    let request: pb::StreamGlobalEventsRequest = super::query_request(&[], &query)?;
    Ok(session_stream(st, None, request.since_seq).into_response())
}

/// Build the SSE stream shared by both scopes.
fn session_stream(
    st: ServerState,
    session: Option<SessionId>,
    since_seq: u64,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events = frame_stream(st, session, since_seq).map(|frame| {
        let event = match frame {
            Ok(frame) => SseEvent::default().json_data(frame).unwrap_or_default(),
            Err(status) => SseEvent::default().event("error").data(status.message()),
        };
        Ok(event)
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// The shared live frame producer backing both SSE and gRPC streams.
pub(crate) fn frame_stream(
    st: ServerState,
    session: Option<SessionId>,
    since_seq: u64,
) -> impl Stream<Item = Result<pb::StreamFrame, tonic::Status>> {
    let rx = st.engine.bus().subscribe();
    BroadcastStream::new(rx).filter_map(move |result| async move {
        match result {
            Ok(envelope) => {
                if let Some(session) = session
                    && envelope.event.session() != Some(session)
                {
                    return None;
                }
                if envelope.seq.0 <= since_seq {
                    return None;
                }
                let frame = match stream_event(&envelope) {
                    Some(event) => pb::stream_frame::Frame::Event(event),
                    None => return None,
                };
                Some(Ok(pb::StreamFrame { frame: Some(frame) }))
            }
            Err(_lagged) => Some(Ok(pb::StreamFrame {
                frame: Some(pb::stream_frame::Frame::Resync(pb::ResyncFrame {
                    last_seq: since_seq,
                })),
            })),
        }
    })
}
