use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use yaca_mcp::McpStatus;

use crate::{ApiError, ServerState};

#[derive(Clone)]
pub(crate) struct McpHttpState {
    added: Arc<RwLock<BTreeMap<String, McpStatus>>>,
}

impl McpHttpState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            added: Arc::new(RwLock::new(BTreeMap::new())),
        }
    }

    async fn status(&self, manager: &yaca_mcp::McpManager) -> BTreeMap<String, McpStatus> {
        let mut status = manager.status();
        status.extend(self.added.read().await.clone());
        status
    }

    async fn insert_disabled(&self, name: String) {
        self.added.write().await.insert(name, McpStatus::Disabled);
    }
}

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .route("/mcp", get(status).post(add))
        .route("/mcp/:name/auth", post(auth_start).delete(auth_remove))
        .route("/mcp/:name/auth/authenticate", post(auth_authenticate))
        .route("/mcp/:name/auth/callback", post(auth_callback))
        .route("/mcp/:name/connect", post(connect))
        .route("/mcp/:name/disconnect", post(disconnect))
}

async fn status(State(st): State<ServerState>) -> Json<BTreeMap<String, McpStatus>> {
    Json(st.mcp_http.status(&st.mcp_manager).await)
}

async fn add(
    State(st): State<ServerState>,
    Json(payload): Json<Value>,
) -> Result<Json<BTreeMap<String, McpStatus>>, ApiError> {
    let name = required_string(&payload, "name")?;
    let config = payload
        .get("config")
        .ok_or_else(|| ApiError::bad_request("missing MCP config"))?;
    validate_disabled_config(config)?;

    st.mcp_http.insert_disabled(name.clone()).await;

    let mut out = BTreeMap::new();
    out.insert(name, McpStatus::Disabled);
    Ok(Json(out))
}

async fn auth_start(State(st): State<ServerState>, AxumPath(name): AxumPath<String>) -> Response {
    if is_known(&st, &name).await {
        unsupported_oauth(&name).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn auth_authenticate(
    State(st): State<ServerState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if is_known(&st, &name).await {
        unsupported_oauth(&name).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn auth_callback(
    State(st): State<ServerState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if is_known(&st, &name).await {
        unsupported_oauth(&name).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn auth_remove(State(st): State<ServerState>, AxumPath(name): AxumPath<String>) -> Response {
    if is_known(&st, &name).await {
        Json(json!({"success": true})).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn connect(State(st): State<ServerState>, AxumPath(name): AxumPath<String>) -> Response {
    if is_known(&st, &name).await {
        Json(json!(true)).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn disconnect(State(st): State<ServerState>, AxumPath(name): AxumPath<String>) -> Response {
    if is_known(&st, &name).await {
        Json(json!(true)).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn is_known(st: &ServerState, name: &str) -> bool {
    st.mcp_http.status(&st.mcp_manager).await.contains_key(name)
}

fn required_string(payload: &Value, field: &str) -> Result<String, ApiError> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("missing MCP {field}")))
}

fn validate_disabled_config(config: &Value) -> Result<(), ApiError> {
    let kind = config
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("missing MCP config type"))?;

    match kind {
        "local" => validate_local_config(config)?,
        "remote" => validate_remote_config(config)?,
        _ => return Err(ApiError::bad_request("invalid MCP config type")),
    }

    if config.get("enabled") == Some(&Value::Bool(false))
        || config.get("disabled") == Some(&Value::Bool(true))
    {
        Ok(())
    } else {
        Err(ApiError::service_unavailable(
            "dynamic MCP add is only available for disabled servers",
        ))
    }
}

fn validate_local_config(config: &Value) -> Result<(), ApiError> {
    let command = config
        .get("command")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("missing MCP local command"))?;
    if command.iter().all(Value::is_string) {
        Ok(())
    } else {
        Err(ApiError::bad_request("invalid MCP local command"))
    }
}

fn validate_remote_config(config: &Value) -> Result<(), ApiError> {
    if config.get("url").and_then(Value::as_str).is_some() {
        Ok(())
    } else {
        Err(ApiError::bad_request("missing MCP remote url"))
    }
}

fn unsupported_oauth(name: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": format!("MCP server {name} does not support OAuth")})),
    )
}

fn not_found(name: &str) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "_tag": "McpServerNotFoundError",
            "name": name,
            "message": format!("MCP server not found: {name}")
        })),
    )
}
