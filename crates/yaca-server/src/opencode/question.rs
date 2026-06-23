use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ApiError, ServerState, parse_session};

use super::location;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/question", get(list_root_requests))
        .route("/question/:request/reply", post(reply_root_request))
        .route("/question/:request/reject", post(reject_root_request))
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

async fn list_root_requests(
    State(st): State<ServerState>,
) -> Json<Vec<crate::pending::QuestionRequestView>> {
    Json(st.question_requests.list().await)
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
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_v2::load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    Ok(Json(SessionQuestionList {
        data: st.question_requests.list_session(session).await,
    })
    .into_response())
}

async fn reply_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
    Json(payload): Json<ReplyPayload>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_v2::load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    if st
        .question_requests
        .reply(session, &request, payload.answers)
        .await
    {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Ok(question_not_found(&request).into_response())
    }
}

async fn reject_request(
    State(st): State<ServerState>,
    Path((id, request)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let session = parse_session(&id)?;
    if let Err(response) = super::session_v2::load_existing_session(&st, session, &id).await? {
        return Ok(response);
    }
    if st.question_requests.reject(session, &request).await {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Ok(question_not_found(&request).into_response())
    }
}

async fn reply_root_request(
    State(st): State<ServerState>,
    Path(request): Path<String>,
    Json(payload): Json<Value>,
) -> Response {
    if !valid_question_request(&request) {
        return ApiError::bad_request("invalid question request id").into_response();
    }
    if !st.question_requests.contains(&request).await {
        return question_not_found(&request).into_response();
    }
    let Some(answers) = parse_answers(&payload) else {
        return ApiError::bad_request("invalid question reply").into_response();
    };
    if st.question_requests.reply_any(&request, answers).await {
        Json(true).into_response()
    } else {
        question_not_found(&request).into_response()
    }
}

async fn reject_root_request(
    State(st): State<ServerState>,
    Path(request): Path<String>,
) -> Response {
    if !valid_question_request(&request) {
        return ApiError::bad_request("invalid question request id").into_response();
    }
    if st.question_requests.reject_any(&request).await {
        Json(true).into_response()
    } else {
        question_not_found(&request).into_response()
    }
}

fn parse_answers(payload: &Value) -> Option<Vec<Vec<String>>> {
    let answers = payload.get("answers")?.as_array()?;
    answers
        .iter()
        .map(|answer| {
            answer
                .as_array()?
                .iter()
                .map(|label| label.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
        })
        .collect()
}

fn valid_question_request(request: &str) -> bool {
    request.starts_with("que") || request.starts_with("q_")
}

fn question_not_found(request: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "QuestionNotFoundError",
            "requestID": request,
            "message": format!("Question request not found: {request}")
        })),
    )
}
