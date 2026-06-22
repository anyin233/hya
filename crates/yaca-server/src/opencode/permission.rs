use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, ServerState, parse_session};

use super::{load_session, location};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/permission", get(list_root_requests))
        .route("/permission/:request/reply", post(reply_root_request))
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
    data: Vec<crate::pending::SavedPermissionInfo>,
}

#[derive(Deserialize)]
struct SavedPermissionQuery {
    #[serde(rename = "projectID")]
    project_id: Option<String>,
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

async fn list_root_requests(
    State(st): State<ServerState>,
) -> Json<Vec<crate::pending::PermissionRequestView>> {
    Json(st.permission_requests.list().await)
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
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    match load_session(&st, session, None).await {
        Ok(_) => {}
        Err(error) if error.status == StatusCode::NOT_FOUND => {
            return Ok(super::errors::legacy_session_not_found(session));
        }
        Err(error) => return Err(error),
    }
    if reply_to_pending(&st, session, &request, payload.response, None).await {
        Ok(Json(true).into_response())
    } else {
        Err(ApiError::not_found(format!(
            "permission request not found: {request}"
        )))
    }
}

async fn reply_root_request(
    State(st): State<ServerState>,
    Path(request): Path<String>,
    Json(payload): Json<Value>,
) -> Response {
    if !request.starts_with("per") {
        return ApiError::bad_request("invalid permission request id").into_response();
    }
    let Ok((reply, message)) = parse_reply_payload(&payload) else {
        return ApiError::bad_request("invalid permission reply").into_response();
    };
    if st
        .permission_requests
        .reply_any(&request, pending_reply(reply), message)
        .await
    {
        Json(true).into_response()
    } else {
        permission_not_found(&request).into_response()
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

fn parse_reply_payload(payload: &Value) -> Result<(WirePermissionReply, Option<String>), ()> {
    let reply = match payload.get("reply").and_then(Value::as_str) {
        Some("once") => WirePermissionReply::Once,
        Some("always") => WirePermissionReply::Always,
        Some("reject") => WirePermissionReply::Reject,
        Some(_) | None => return Err(()),
    };
    let message = match payload.get("message") {
        Some(value) => Some(value.as_str().ok_or(())?.to_string()),
        None => None,
    };
    Ok((reply, message))
}

fn pending_reply(reply: WirePermissionReply) -> crate::pending::PermissionReply {
    match reply {
        WirePermissionReply::Once => crate::pending::PermissionReply::Once,
        WirePermissionReply::Always => crate::pending::PermissionReply::Always,
        WirePermissionReply::Reject => crate::pending::PermissionReply::Reject,
    }
}

fn permission_not_found(request: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "PermissionNotFoundError",
            "requestID": request,
            "message": format!("Permission request not found: {request}")
        })),
    )
}

async fn list_saved(
    State(st): State<ServerState>,
    Query(query): Query<SavedPermissionQuery>,
) -> Json<SavedPermissionList> {
    Json(SavedPermissionList {
        data: st
            .permission_requests
            .list_saved(query.project_id.as_deref())
            .await,
    })
}

async fn remove_saved(State(st): State<ServerState>, Path(id): Path<String>) -> StatusCode {
    st.permission_requests.remove_saved(&id).await;
    StatusCode::NO_CONTENT
}
