use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ServerState, parse_session};

use super::{load_session, location};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/permission/request", get(list_requests))
        .route("/api/permission/saved", get(list_saved))
        .route("/api/permission/saved/:id", delete(remove_saved))
        .route("/api/session/:id/permission", get(list_session_requests))
        .route(
            "/api/session/:id/permission/:request/reply",
            post(reply_request),
        )
        .route(
            "/session/:id/permissions/:request",
            post(reply_legacy_request),
        )
}

#[derive(Serialize)]
struct SavedPermissionList {
    data: Vec<SavedPermissionInfo>,
}

#[derive(Serialize)]
struct SavedPermissionInfo {
    id: String,
    #[serde(rename = "projectID")]
    project_id: String,
    action: String,
    resource: String,
}

#[derive(Serialize)]
struct SessionPermissionList {
    data: Vec<crate::pending::PermissionRequestView>,
}

#[derive(Deserialize)]
struct ReplyPayload {
    reply: WirePermissionReply,
    message: Option<String>,
}

#[derive(Deserialize)]
struct LegacyReplyPayload {
    response: WirePermissionReply,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum WirePermissionReply {
    Once,
    Always,
    Reject,
}

async fn list_requests(
    State(st): State<ServerState>,
) -> Json<location::LocationResponse<Vec<crate::pending::PermissionRequestView>>> {
    let requests = st.permission_requests.list().await;
    Json(location::response(&st, requests))
}

async fn list_session_requests(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<SessionPermissionList>, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    Ok(Json(SessionPermissionList {
        data: st.permission_requests.list_session(session).await,
    }))
}

async fn reply_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
    Json(payload): Json<ReplyPayload>,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    if reply_to_pending(&st, session, &request, payload.reply, payload.message).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "permission request not found: {request}"
        )))
    }
}

async fn reply_legacy_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
    Json(payload): Json<LegacyReplyPayload>,
) -> Result<Json<bool>, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    if reply_to_pending(&st, session, &request, payload.response, None).await {
        Ok(Json(true))
    } else {
        Err(ApiError::not_found(format!(
            "permission request not found: {request}"
        )))
    }
}

async fn reply_to_pending(
    st: &ServerState,
    session: yaca_proto::SessionId,
    request: &str,
    reply: WirePermissionReply,
    message: Option<String>,
) -> bool {
    let reply = match reply {
        WirePermissionReply::Once => crate::pending::PermissionReply::Once,
        WirePermissionReply::Always => crate::pending::PermissionReply::Always,
        WirePermissionReply::Reject => crate::pending::PermissionReply::Reject,
    };
    st.permission_requests
        .reply(session, request, reply, message)
        .await
}

async fn list_saved() -> Json<SavedPermissionList> {
    Json(SavedPermissionList { data: Vec::new() })
}

async fn remove_saved(Path(_id): Path<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}
