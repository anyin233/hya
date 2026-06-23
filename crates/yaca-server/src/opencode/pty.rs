use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use axum::extract::{Path as AxumPath, Query, State};
use axum::http::header::HeaderMap;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location;
use super::pty_state::{PtyInfo, TicketStatus};

const CONNECT_TOKEN_HEADER: &str = "x-opencode-ticket";
const CONNECT_TOKEN_HEADER_VALUE: &str = "1";

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/pty/shells", get(shells))
        .route("/pty", get(list_legacy).post(create_legacy))
        .route(
            "/pty/:id",
            get(get_legacy).put(update_legacy).delete(remove_legacy),
        )
        .route("/pty/:id/connect-token", post(connect_token))
        .route("/pty/:id/connect", get(connect))
        .route("/api/pty/shells", get(shells))
        .route("/api/pty", get(list_api).post(create_api))
        .route(
            "/api/pty/:id",
            get(get_api).put(update_api).delete(remove_api),
        )
        .route("/api/pty/:id/connect-token", post(connect_token_api))
        .route("/api/pty/:id/connect", get(connect))
}

#[derive(Serialize)]
struct ShellItem {
    path: String,
    name: String,
    acceptable: bool,
}

async fn shells() -> Json<Vec<ShellItem>> {
    Json(shell_candidates().into_iter().map(shell_item).collect())
}

async fn list_legacy(State(st): State<ServerState>) -> Json<Vec<PtyInfo>> {
    Json(st.pty.list().await)
}

async fn list_api(State(st): State<ServerState>) -> Json<location::LocationResponse<Vec<PtyInfo>>> {
    Json(location::response(&st, st.pty.list().await))
}

async fn create_legacy(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<PtyInfo>, ApiError> {
    let payload = super::pty_payload::create(payload, default_cwd(&st))?;
    Ok(Json(st.pty.create(payload).await))
}

async fn create_api(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<location::LocationResponse<PtyInfo>>, ApiError> {
    let payload = super::pty_payload::create(payload, default_cwd(&st))?;
    let info = st.pty.create(payload).await;
    Ok(Json(location::response(&st, info)))
}

async fn get_legacy(State(st): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    match st.pty.get(&id).await {
        Some(info) => Json(info).into_response(),
        None => pty_not_found(&id),
    }
}

async fn get_api(State(st): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    match st.pty.get(&id).await {
        Some(info) => Json(location::response(&st, info)).into_response(),
        None => pty_not_found(&id),
    }
}

async fn update_legacy(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<Value>,
) -> Result<Response, ApiError> {
    let payload = super::pty_payload::update(payload)?;
    Ok(match st.pty.update(&id, payload).await {
        Some(info) => Json(info).into_response(),
        None => pty_not_found(&id),
    })
}

async fn update_api(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(payload): Json<Value>,
) -> Result<Response, ApiError> {
    let payload = super::pty_payload::update(payload)?;
    Ok(match st.pty.update(&id, payload).await {
        Some(info) => Json(location::response(&st, info)).into_response(),
        None => pty_not_found(&id),
    })
}

async fn remove_legacy(State(st): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    if st.pty.remove(&id).await {
        Json(true).into_response()
    } else {
        pty_not_found(&id)
    }
}

async fn remove_api(State(st): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    if st.pty.remove(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        pty_not_found(&id)
    }
}

async fn connect_token(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    connect_token_response(&st, &id, &headers, false).await
}

async fn connect_token_api(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
) -> Response {
    connect_token_response(&st, &id, &headers, true).await
}

async fn connect_token_response(
    st: &ServerState,
    id: &str,
    headers: &HeaderMap,
    wrap: bool,
) -> Response {
    if headers.get(CONNECT_TOKEN_HEADER)
        != Some(&HeaderValue::from_static(CONNECT_TOKEN_HEADER_VALUE))
    {
        return forbidden();
    }
    let Some((ticket, expires_in)) = st.pty.issue_ticket(id).await else {
        return pty_not_found(id);
    };
    let token = json!({"ticket": ticket, "expires_in": expires_in});
    if wrap {
        Json(location::response(st, token)).into_response()
    } else {
        Json(token).into_response()
    }
}

async fn connect(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    let Some(ticket) = query.get("ticket") else {
        if st.pty.get(&id).await.is_none() {
            return pty_not_found(&id);
        }
        return forbidden();
    };
    match st.pty.consume_ticket(&id, ticket).await {
        TicketStatus::Accepted => StatusCode::NOT_IMPLEMENTED.into_response(),
        TicketStatus::Invalid => forbidden(),
        TicketStatus::NotFound => pty_not_found(&id),
    }
}

fn default_cwd(st: &ServerState) -> String {
    location::workdir(st).to_string_lossy().into_owned()
}

fn shell_candidates() -> Vec<String> {
    let mut paths = BTreeSet::new();
    if let Some(shell) = std::env::var_os("SHELL").and_then(|value| value.into_string().ok()) {
        paths.insert(shell);
    }
    for path in [
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/sh",
        "/usr/bin/sh",
    ] {
        paths.insert(path.to_string());
    }
    paths.into_iter().collect()
}

fn shell_item(path: String) -> ShellItem {
    let acceptable = is_executable(&path);
    let name = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path.as_str())
        .to_string();
    ShellItem {
        path,
        name,
        acceptable,
    }
}

fn is_executable(path: &str) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn pty_not_found(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "PtyNotFoundError",
            "ptyID": id,
            "message": format!("PTY session not found: {id}"),
        })),
    )
        .into_response()
}

fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "_tag": "ForbiddenError",
            "message": "Invalid PTY connect token request",
        })),
    )
        .into_response()
}
