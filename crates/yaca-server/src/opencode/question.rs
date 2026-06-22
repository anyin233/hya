use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::{ApiError, ServerState, parse_session};

use super::{load_session, location};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/question/request", get(list_requests))
        .route("/api/session/:id/question", get(list_session_requests))
        .route(
            "/api/session/:id/question/:request/reply",
            post(reply_request),
        )
        .route(
            "/api/session/:id/question/:request/reject",
            post(reject_request),
        )
}

#[derive(Serialize)]
struct SessionQuestionList {
    data: Vec<crate::pending::QuestionRequestView>,
}

#[derive(Deserialize)]
struct ReplyPayload {
    answers: Vec<Vec<String>>,
}

async fn list_requests(
    State(st): State<ServerState>,
) -> Json<location::LocationResponse<Vec<crate::pending::QuestionRequestView>>> {
    let requests = st.question_requests.list().await;
    Json(location::response(&st, requests))
}

async fn list_session_requests(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Json<SessionQuestionList>, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    Ok(Json(SessionQuestionList {
        data: st.question_requests.list_session(session).await,
    }))
}

async fn reply_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
    Json(payload): Json<ReplyPayload>,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    if st
        .question_requests
        .reply(session, &request, payload.answers)
        .await
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "question request not found: {request}"
        )))
    }
}

async fn reject_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let session = parse_session(&id)?;
    load_session(&st, session, None).await?;
    if st.question_requests.reject(session, &request).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "question request not found: {request}"
        )))
    }
}
