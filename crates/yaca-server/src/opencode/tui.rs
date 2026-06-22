use std::collections::VecDeque;
use std::sync::Arc;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify};

use crate::{ApiError, ServerState};

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/tui/publish", post(publish))
        .route("/tui/control/next", get(control_next))
        .route("/tui/control/response", post(control_response))
}

#[derive(Clone)]
pub(crate) struct TuiState {
    inner: Arc<TuiQueues>,
}

struct TuiQueues {
    requests: Mutex<VecDeque<TuiRequest>>,
    responses: Mutex<VecDeque<Value>>,
    request_ready: Notify,
}

#[derive(Clone, Debug, Serialize)]
struct TuiRequest {
    path: String,
    body: Value,
}

#[derive(Deserialize, Serialize)]
struct PublishPayload {
    #[serde(rename = "type")]
    kind: String,
    properties: Value,
}

impl TuiState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(TuiQueues {
                requests: Mutex::new(VecDeque::new()),
                responses: Mutex::new(VecDeque::new()),
                request_ready: Notify::new(),
            }),
        }
    }

    async fn push_request(&self, request: TuiRequest) {
        self.inner.requests.lock().await.push_back(request);
        self.inner.request_ready.notify_one();
    }

    async fn next_request(&self) -> TuiRequest {
        loop {
            if let Some(request) = self.inner.requests.lock().await.pop_front() {
                return request;
            }
            self.inner.request_ready.notified().await;
        }
    }

    async fn push_response(&self, response: Value) {
        self.inner.responses.lock().await.push_back(response);
    }
}

async fn publish(
    State(st): State<ServerState>,
    Json(payload): Json<PublishPayload>,
) -> Result<Json<bool>, ApiError> {
    validate_publish_type(&payload.kind)?;
    let body = serde_json::to_value(payload).map_err(|e| ApiError::internal(e.to_string()))?;
    st.tui
        .push_request(TuiRequest {
            path: "/tui/publish".to_string(),
            body,
        })
        .await;
    Ok(Json(true))
}

async fn control_next(State(st): State<ServerState>) -> Json<TuiRequest> {
    Json(st.tui.next_request().await)
}

async fn control_response(State(st): State<ServerState>, Json(payload): Json<Value>) -> Json<bool> {
    st.tui.push_response(payload).await;
    Json(true)
}

fn validate_publish_type(kind: &str) -> Result<(), ApiError> {
    match kind {
        "tui.prompt.append" | "tui.command.execute" | "tui.toast.show" | "tui.session.select" => {
            Ok(())
        }
        _ => Err(ApiError::bad_request("unsupported TUI event type")),
    }
}
