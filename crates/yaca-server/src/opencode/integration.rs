use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::location::LocationResponse;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/reference", get(reference_list))
        .route("/api/integration", get(integration_list))
        .route("/api/integration/:integration_id", get(integration_get))
        .route(
            "/api/integration/:integration_id/connect/key",
            post(integration_connect_key),
        )
        .route(
            "/api/integration/:integration_id/connect/oauth",
            post(integration_connect_oauth),
        )
        .route(
            "/api/integration/attempt/:attempt_id",
            get(integration_attempt_status).delete(integration_attempt_cancel),
        )
        .route(
            "/api/integration/attempt/:attempt_id/complete",
            post(integration_attempt_complete),
        )
        .route(
            "/api/credential/:credential_id",
            patch(credential_update).delete(credential_remove),
        )
}

async fn reference_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<Value>>> {
    Json(super::location::response(&st, Vec::new()))
}

async fn integration_list(State(st): State<ServerState>) -> Json<LocationResponse<Vec<Value>>> {
    Json(super::location::response(&st, Vec::new()))
}

async fn integration_get(State(st): State<ServerState>) -> Json<LocationResponse<Option<Value>>> {
    Json(super::location::response(&st, None))
}

async fn integration_connect_key(
    AxumPath(_integration_id): AxumPath<String>,
    Json(_payload): Json<Value>,
) -> Response {
    integration_authorization_error("Authentication failed")
}

async fn integration_connect_oauth(
    AxumPath(_integration_id): AxumPath<String>,
    Json(_payload): Json<Value>,
) -> Response {
    integration_authorization_error("Authentication failed")
}

async fn integration_attempt_status(
    AxumPath(_attempt_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    Err(ApiError::internal("integration attempt is unavailable"))
}

async fn integration_attempt_complete(
    AxumPath(_attempt_id): AxumPath<String>,
    Json(_payload): Json<Value>,
) -> Response {
    integration_authorization_error("Authentication failed")
}

async fn integration_attempt_cancel(AxumPath(_attempt_id): AxumPath<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn credential_update(
    AxumPath(_credential_id): AxumPath<String>,
    Json(_payload): Json<Value>,
) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn credential_remove(AxumPath(_credential_id): AxumPath<String>) -> StatusCode {
    StatusCode::NO_CONTENT
}

fn integration_authorization_error(message: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "_tag": "InvalidRequestError",
            "message": message,
            "kind": "integration_authorization",
        })),
    )
        .into_response()
}
