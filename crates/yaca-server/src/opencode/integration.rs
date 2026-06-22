use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;

use crate::ServerState;

use super::location::LocationResponse;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/api/reference", get(reference_list))
        .route("/api/integration", get(integration_list))
        .route("/api/integration/:integration_id", get(integration_get))
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
