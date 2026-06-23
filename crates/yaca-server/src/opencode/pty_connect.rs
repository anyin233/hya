use std::collections::BTreeMap;

use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde_json::json;

use crate::ServerState;

use super::pty_state::{PtyEvent, TicketStatus};

const REPLAY_CHUNK: usize = 64 * 1024;

pub(super) async fn connect(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Some(ticket) = query.get("ticket") else {
        if st.pty.get(&id).await.is_none() {
            return super::pty::pty_not_found(&id);
        }
        return super::pty::forbidden();
    };
    match st.pty.consume_ticket(&id, ticket).await {
        TicketStatus::Accepted => {}
        TicketStatus::Invalid => return super::pty::forbidden(),
        TicketStatus::NotFound => return super::pty::pty_not_found(&id),
    }
    let Ok(ws) = ws else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let cursor = query
        .get("cursor")
        .and_then(|value| value.parse::<i64>().ok());
    ws.on_upgrade(move |socket| stream(st, id, cursor, socket))
}

async fn stream(st: ServerState, id: String, cursor: Option<i64>, socket: WebSocket) {
    let Some(mut attachment) = st.pty.attach(&id, cursor).await else {
        return;
    };
    let (mut tx, mut rx) = socket.split();
    for chunk in replay_chunks(&attachment.replay) {
        if tx.send(Message::Text(chunk)).await.is_err() {
            return;
        }
    }
    if tx
        .send(Message::Binary(meta_frame(attachment.cursor)))
        .await
        .is_err()
    {
        return;
    }
    loop {
        tokio::select! {
            message = rx.next() => {
                let Some(Ok(message)) = message else {
                    break;
                };
                match message {
                    Message::Text(text) => {
                        let _ = st.pty.write(&id, &text).await;
                    }
                    Message::Binary(bytes) => {
                        if let Ok(text) = String::from_utf8(bytes) {
                            let _ = st.pty.write(&id, &text).await;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            event = attachment.events.recv() => match event {
                Ok(PtyEvent::Data(chunk)) => {
                    if tx.send(Message::Text(chunk)).await.is_err() {
                        break;
                    }
                }
                Ok(PtyEvent::End) | Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            }
        }
    }
    let _ = tx.send(Message::Close(None)).await;
}

fn meta_frame(cursor: u64) -> Vec<u8> {
    let bytes = json!({ "cursor": cursor }).to_string().into_bytes();
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(0);
    out.extend(bytes);
    out
}

fn replay_chunks(input: &str) -> impl Iterator<Item = String> + '_ {
    let mut start = 0;
    std::iter::from_fn(move || {
        if start >= input.len() {
            return None;
        }
        let mut end = (start + REPLAY_CHUNK).min(input.len());
        while !input.is_char_boundary(end) {
            end -= 1;
        }
        let chunk = input[start..end].to_string();
        start = end;
        Some(chunk)
    })
}
