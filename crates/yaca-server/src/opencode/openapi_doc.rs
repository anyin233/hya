use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/doc", get(doc))
}

async fn doc() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "yaca OpenCode compatibility API",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "paths": {
            "/global/health": {"get": {}},
            "/global/event": {"get": {}},
            "/event": {"get": {}},
            "/session": {"get": {}, "post": {}},
            "/session/status": {"get": {}},
            "/api/session": {"get": {}, "post": {}},
            "/api/event": {"get": {}},
        },
    }))
}
