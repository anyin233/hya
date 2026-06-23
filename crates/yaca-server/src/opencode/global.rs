use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::RwLock;

use crate::{ApiError, ServerState};

#[derive(Clone)]
pub(crate) struct GlobalState {
    config: Arc<RwLock<Value>>,
}

impl GlobalState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(json!({}))),
        }
    }

    pub(in crate::opencode) async fn config(&self) -> Value {
        self.config.read().await.clone()
    }

    pub(in crate::opencode) async fn update_config(&self, config: Value) {
        *self.config.write().await = config;
    }
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/global/config", get(config_get).patch(config_update))
        .route("/global/dispose", post(dispose))
        .route("/global/upgrade", post(upgrade))
}

async fn config_get(State(st): State<ServerState>) -> Json<Value> {
    Json(st.global.config().await)
}

async fn config_update(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(map) = payload.as_object() else {
        return Err(ApiError::bad_request(
            "global config payload must be an object",
        ));
    };
    if let Some(username) = map.get("username")
        && !username.is_string()
    {
        return Err(ApiError::bad_request("username must be a string"));
    }
    st.global.update_config(payload.clone()).await;
    Ok(Json(payload))
}

async fn dispose() -> Json<bool> {
    Json(true)
}

async fn upgrade(body: Bytes) -> Response {
    match parse_upgrade_payload(&body) {
        Ok(()) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"success": false, "error": "Unknown installation method"})),
        )
            .into_response(),
        Err(()) => invalid_upgrade_body().into_response(),
    }
}

fn parse_upgrade_payload(body: &[u8]) -> Result<(), ()> {
    let payload = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice::<Value>(body).map_err(|_| ())?
    };
    let Some(map) = payload.as_object() else {
        return Err(());
    };
    if let Some(target) = map.get("target")
        && !target.is_string()
    {
        return Err(());
    }
    Ok(())
}

fn invalid_upgrade_body() -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"success": false, "error": "Invalid request body"})),
    )
}
