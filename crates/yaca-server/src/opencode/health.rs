use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/api/health", get(health))
}

#[derive(Serialize)]
struct Health {
    healthy: bool,
}

async fn health() -> Json<Health> {
    Json(Health { healthy: true })
}
