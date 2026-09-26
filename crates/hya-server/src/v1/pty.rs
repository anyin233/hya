//! `/v1` PTY domain: session management, one-time connect tokens, and the
//! WebSocket terminal stream carrying protojson `PtyClientFrame` /
//! `PtyServerFrame` messages (the HTTP form of `StreamPty`).

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use super::Json;
use futures::{SinkExt, StreamExt};

use crate::ServerState;
use hya_api::v1 as pb;
use hya_api::v1::pty_client_frame::Frame as ClientFrame;
use hya_api::v1::pty_server_frame::Frame as ServerFrame;

use super::{V1Error, scope_directory};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/pty/shells", get(list_shells))
        .route("/v1/pty", post(create_pty))
        .route(
            "/v1/pty/:id",
            get(get_pty)
                .put(axum::routing::put(update_pty))
                .delete(delete_pty),
        )
        .route("/v1/pty/:id/connect-token", post(connect_token))
        .route("/v1/pty/:id/connect", get(connect))
}

fn pty_session(info: &crate::support::pty_state::PtyInfo) -> pb::PtySession {
    let value = serde_json::to_value(info).unwrap_or(serde_json::Value::Null);
    let field = |name: &str| {
        value
            .get(name)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or_default()
    };
    pb::PtySession {
        id: info.id.clone(),
        shell: field("command"),
        cols: field("cols").parse().unwrap_or(80),
        rows: field("rows").parse().unwrap_or(24),
        cwd: field("cwd"),
    }
}

async fn list_shells() -> Result<Json<pb::ListShellsResponse>, V1Error> {
    let shells = crate::support::pty_shell::shell_paths()
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    Ok(Json(pb::ListShellsResponse { shells }))
}

async fn create_pty(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    Json(request): Json<pb::CreatePtyRequest>,
) -> Result<Json<pb::PtySession>, V1Error> {
    let scope: pb::ListWorktreesRequest = super::query_request(&[], &query)?;
    let cwd = if request.cwd.is_empty() {
        scope_directory(&headers, &scope.directory)
            .to_string_lossy()
            .into_owned()
    } else {
        request.cwd.clone()
    };
    let payload = crate::support::pty_state::CreatePayload {
        command: if request.shell.is_empty() {
            default_shell()
        } else {
            request.shell.clone()
        },
        args: Vec::new(),
        cwd,
        title: String::new(),
    };
    let info = st.pty.create(payload).await.map_err(V1Error::internal)?;
    Ok(Json(pty_session(&info)))
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned())
}

async fn get_pty(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::PtySession>, V1Error> {
    match st.pty.get(&id).await {
        Some(info) => Ok(Json(pty_session(&info))),
        None => Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("pty session not found: {id}"),
        )),
    }
}

async fn update_pty(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(_request): Json<pb::UpdatePtyRequest>,
) -> Result<Json<pb::PtySession>, V1Error> {
    // The runtime supports title updates; terminal resize is applied by the
    // shell itself through the stream (SIGWINCH) and is a documented no-op
    // here until PtyState grows a resize API.
    let payload = crate::support::pty_state::UpdatePayload { title: None };
    match st.pty.update(&id, payload).await {
        Some(info) => Ok(Json(pty_session(&info))),
        None => Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("pty session not found: {id}"),
        )),
    }
}

async fn delete_pty(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::DeletePtyResponse>, V1Error> {
    match st.pty.remove(&id).await {
        true => Ok(Json(pb::DeletePtyResponse {})),
        false => Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("pty session not found: {id}"),
        )),
    }
}

async fn connect_token(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    _headers: HeaderMap,
) -> Result<Json<pb::CreateConnectTokenResponse>, V1Error> {
    if st.pty.get(&id).await.is_none() {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("pty session not found: {id}"),
        ));
    }
    let Some((ticket, _expires_in)) = st.pty.issue_ticket(&id).await else {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("pty session not found: {id}"),
        ));
    };
    Ok(Json(pb::CreateConnectTokenResponse {
        url: format!("/v1/pty/{id}/connect?ticket={ticket}"),
        token: ticket,
    }))
}

async fn connect(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    use crate::support::pty_state::TicketStatus;
    let Some(ticket) = query.get("ticket") else {
        if st.pty.get(&id).await.is_none() {
            return not_found(&id);
        }
        return StatusCode::FORBIDDEN.into_response();
    };
    match st.pty.consume_ticket(&id, ticket).await {
        TicketStatus::Accepted => {}
        TicketStatus::Invalid => return StatusCode::FORBIDDEN.into_response(),
        TicketStatus::NotFound => return not_found(&id),
    }
    let Ok(ws) = ws else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let cursor = query
        .get("cursor")
        .and_then(|value| value.parse::<i64>().ok());
    ws.on_upgrade(move |socket| stream(st, id, cursor, socket))
}

fn not_found(id: &str) -> Response {
    let error = V1Error::new(
        hya_api::error::Code::NotFound,
        format!("pty session not found: {id}"),
    );
    error.into_response()
}

/// WebSocket bridge speaking protojson `PtyServerFrame` / `PtyClientFrame`.
async fn stream(st: ServerState, id: String, cursor: Option<i64>, socket: WebSocket) {
    use crate::support::pty_state::PtyEvent;
    let Some(mut attachment) = st.pty.attach(&id, cursor).await else {
        return;
    };
    let (mut tx, mut rx) = socket.split();
    if send_frame(
        &mut tx,
        ServerFrame::Output(attachment.replay.clone().into_bytes()),
    )
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
                        if handle_client_frame(&st, &id, &text).await.is_err() {
                            break;
                        }
                    }
                    Message::Binary(bytes) => {
                        // Legacy compat clients send raw terminal bytes.
                        let _ = st.pty.write(&id, &String::from_utf8_lossy(&bytes)).await;
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            event = attachment.events.recv() => match event {
                Ok(PtyEvent::Data(chunk)) => {
                    if send_frame(&mut tx, ServerFrame::Output(chunk.into_bytes())).await.is_err() {
                        break;
                    }
                }
                Ok(PtyEvent::End) => {
                    let _ = send_frame(&mut tx, ServerFrame::Exit(0)).await;
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            }
        }
    }
    let _ = tx.send(Message::Close(None)).await;
}

async fn handle_client_frame(st: &ServerState, id: &str, text: &str) -> Result<(), ()> {
    let Ok(frame) = serde_json::from_str::<pb::PtyClientFrame>(text) else {
        return Ok(());
    };
    match frame.frame {
        Some(ClientFrame::Input(bytes)) => {
            let _ = st.pty.write(id, &String::from_utf8_lossy(&bytes)).await;
        }
        Some(ClientFrame::Resize(_)) | Some(ClientFrame::Attach(_)) => {}
        Some(ClientFrame::Ping(_)) | None => {}
    }
    Ok(())
}

async fn send_frame(
    tx: &mut futures::stream::SplitSink<WebSocket, Message>,
    frame: ServerFrame,
) -> Result<(), axum::Error> {
    let message = pb::PtyServerFrame { frame: Some(frame) };
    let text = serde_json::to_string(&message).unwrap_or_default();
    tx.send(Message::Text(text)).await
}
