use std::collections::BTreeMap;

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use yaca_mcp::{McpServerConfig, McpStatus};

use crate::{ApiError, ServerState};

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
    let config = parse_config(config)?;

    let status = st.mcp_http.add_config(name.clone(), config).await;

    let mut out = BTreeMap::new();
    out.insert(name, status);
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
    if st.mcp_http.connect_config(&name).await.is_some() || is_known(&st, &name).await {
        Json(json!(true)).into_response()
    } else {
        not_found(&name).into_response()
    }
}

async fn disconnect(State(st): State<ServerState>, AxumPath(name): AxumPath<String>) -> Response {
    if st.mcp_http.disconnect_config(&name).await || is_known(&st, &name).await {
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

fn parse_config(config: &Value) -> Result<McpServerConfig, ApiError> {
    let kind = config
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("missing MCP config type"))?;

    match kind {
        "local" => parse_local_config(config),
        "remote" => parse_remote_config(config),
        _ => Err(ApiError::bad_request("invalid MCP config type")),
    }
}

fn parse_local_config(config: &Value) -> Result<McpServerConfig, ApiError> {
    let command = config
        .get("command")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("missing MCP local command"))?;
    let command = command
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| ApiError::bad_request("invalid MCP local command"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(McpServerConfig {
        command,
        env: string_map(config, "environment")?,
        enabled: enabled(config),
        timeout_ms: optional_u64(config, "timeout")?,
    })
}

fn parse_remote_config(config: &Value) -> Result<McpServerConfig, ApiError> {
    let _url = config
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("missing MCP remote url"))?;
    Ok(McpServerConfig {
        enabled: enabled(config),
        timeout_ms: optional_u64(config, "timeout")?,
        ..McpServerConfig::default()
    })
}

fn enabled(config: &Value) -> Option<bool> {
    if config.get("disabled") == Some(&Value::Bool(true)) {
        Some(false)
    } else {
        config.get("enabled").and_then(Value::as_bool)
    }
}

fn optional_u64(config: &Value, field: &str) -> Result<Option<u64>, ApiError> {
    config
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| ApiError::bad_request(format!("invalid MCP {field}")))
        })
        .transpose()
}

fn string_map(config: &Value, field: &str) -> Result<Option<BTreeMap<String, String>>, ApiError> {
    config
        .get(field)
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| ApiError::bad_request(format!("invalid MCP {field}")))?;
            object
                .iter()
                .map(|(key, value)| {
                    value
                        .as_str()
                        .map(|value| (key.clone(), value.to_string()))
                        .ok_or_else(|| ApiError::bad_request(format!("invalid MCP {field}")))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()
        })
        .transpose()
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
