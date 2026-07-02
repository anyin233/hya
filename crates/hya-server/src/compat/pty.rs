use axum::extract::{Path as AxumPath, State};
use axum::http::header::HeaderMap;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location;
use super::pty_state::PtyInfo;

use super::external_protocol::CONNECT_TOKEN_HEADER;
const CONNECT_TOKEN_HEADER_VALUE: &str = "1";

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/pty/shells", get(super::pty_shell::shells))
        .route("/pty", get(list_legacy).post(create_legacy))
        .route(
            "/pty/:id",
            get(get_legacy).put(update_legacy).delete(remove_legacy),
        )
        .route("/pty/:id/connect-token", post(connect_token))
        .route("/pty/:id/connect", get(super::pty_connect::connect))
        .route("/api/pty/shells", get(super::pty_shell::shells))
        .route("/api/pty", get(list_api).post(create_api))
        .route(
            "/api/pty/:id",
            get(get_api).put(update_api).delete(remove_api),
        )
        .route("/api/pty/:id/connect-token", post(connect_token_api))
        .route("/api/pty/:id/connect", get(super::pty_connect::connect))
}

async fn list_legacy(State(st): State<ServerState>) -> Json<Vec<PtyInfo>> {
    Json(running_only(st.pty.list().await))
}

async fn list_api(State(st): State<ServerState>) -> Json<location::LocationResponse<Vec<PtyInfo>>> {
    Json(location::response(&st, st.pty.list().await))
}

async fn create_legacy(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<PtyInfo>, ApiError> {
    let payload = super::pty_payload::create(payload, default_cwd(&st))?;
    Ok(Json(
        st.pty.create(payload).await.map_err(ApiError::internal)?,
    ))
}

async fn create_api(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<location::LocationResponse<PtyInfo>>, ApiError> {
    let payload = super::pty_payload::create(payload, default_cwd(&st))?;
    let info = st.pty.create(payload).await.map_err(ApiError::internal)?;
    Ok(Json(location::response(&st, info)))
}

async fn get_legacy(State(st): State<ServerState>, AxumPath(id): AxumPath<String>) -> Response {
    match running(st.pty.get(&id).await) {
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
    if running(st.pty.get(&id).await).is_none() {
        return Ok(pty_not_found(&id));
    }
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
    if running(st.pty.get(&id).await).is_none() {
        return pty_not_found(&id);
    }
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
        || !allowed_request_origin(headers)
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

fn default_cwd(st: &ServerState) -> String {
    location::workdir(st).to_string_lossy().into_owned()
}

fn running_only(items: Vec<PtyInfo>) -> Vec<PtyInfo> {
    items
        .into_iter()
        .filter(|info| info.status == "running")
        .collect()
}

fn running(info: Option<PtyInfo>) -> Option<PtyInfo> {
    info.filter(|info| info.status == "running")
}

fn allowed_request_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = header_str(headers, "origin") else {
        return true;
    };
    if header_str(headers, "host").is_some_and(|host| same_host(origin, host)) {
        return true;
    }
    allowed_cors_origin(origin)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn same_host(origin: &str, host: &str) -> bool {
    origin
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        == Some(host)
}

fn allowed_cors_origin(origin: &str) -> bool {
    origin.starts_with("http://localhost:")
        || origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("oc://renderer")
        || matches!(
            origin,
            "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
        )
        || compat_origin(origin)
}

fn compat_origin(origin: &str) -> bool {
    let Some(host) = origin
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
    else {
        return false;
    };
    host == "compat.ai" || host.ends_with(".opencode.ai")
}

pub(super) fn pty_not_found(id: &str) -> Response {
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

pub(super) fn forbidden() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "_tag": "ForbiddenError",
            "message": "Invalid PTY connect token request",
        })),
    )
        .into_response()
}
