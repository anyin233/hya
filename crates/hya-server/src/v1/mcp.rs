//! `/v1` MCP domain: desired-state registry and connection control over
//! the app-owned MCP control handle.

use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::routing::{get, post};

use super::Json;
use hya_mcp::{McpServerConfig, McpStatus};

use crate::ServerState;
use hya_api::v1 as pb;
use hya_api::v1::add_mcp_server_request::Transport;

use super::V1Error;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/mcp", get(get_status).post(add_server))
        .route("/v1/mcp/:name/connect", post(connect))
        .route("/v1/mcp/:name/disconnect", post(disconnect))
        .route("/v1/mcp/:name/auth", post(start_auth).delete(remove_auth))
        .route("/v1/mcp/:name/auth/complete", post(complete_auth))
}

/// One server's wire status; `tools` (the server's namespaced tool names)
/// is reported only while it is `CONNECTED`.
fn server_status(
    name: &str,
    status: &McpStatus,
    tools: Option<&Vec<String>>,
) -> pb::McpServerStatus {
    let (state, error, auth_required) = match status {
        McpStatus::Connecting => (pb::McpServerState::Desired as i32, String::new(), false),
        McpStatus::Connected => (pb::McpServerState::Connected as i32, String::new(), false),
        McpStatus::Disabled => (
            pb::McpServerState::Disconnected as i32,
            String::new(),
            false,
        ),
        McpStatus::Failed { error } => (pb::McpServerState::Failed as i32, error.clone(), false),
        McpStatus::NeedsAuth => (pb::McpServerState::Desired as i32, String::new(), true),
        McpStatus::NeedsClientRegistration { error } => {
            (pb::McpServerState::Failed as i32, error.clone(), true)
        }
    };
    let tools = match status {
        McpStatus::Connected => tools.cloned().unwrap_or_default(),
        _ => Vec::new(),
    };
    pb::McpServerStatus {
        name: name.to_owned(),
        state,
        tools,
        error,
        auth_required,
    }
}

async fn named_status(st: &ServerState, name: &str, status: McpStatus) -> pb::McpServerStatus {
    let tools = st.mcp_control.tools().await;
    server_status(name, &status, tools.get(name))
}

async fn get_status(
    State(st): State<ServerState>,
) -> Result<Json<pb::GetMcpStatusResponse>, V1Error> {
    let statuses = st.mcp_control.status().await;
    let tools = st.mcp_control.tools().await;
    let servers = statuses
        .iter()
        .map(|(name, status)| server_status(name, status, tools.get(name)))
        .collect();
    Ok(Json(pb::GetMcpStatusResponse { servers }))
}

async fn add_server(
    State(st): State<ServerState>,
    Json(request): Json<pb::AddMcpServerRequest>,
) -> Result<Json<pb::McpServerStatus>, V1Error> {
    let config = match request.transport {
        Some(Transport::Command(command)) => McpServerConfig {
            command: {
                let mut argv = vec![command.command.clone()];
                argv.extend(command.args);
                argv
            },
            env: if command.env.is_empty() {
                None
            } else {
                Some(command.env.into_iter().collect())
            },
            url: None,
            transport: None,
            enabled: request.enabled,
            timeout_ms: None,
        },
        Some(Transport::Url(url)) => McpServerConfig {
            command: Vec::new(),
            env: None,
            url: Some(url.url),
            // Streamable HTTP is the default remote transport; classic SSE
            // stays configurable through the config file `transport:` field.
            transport: None,
            enabled: request.enabled,
            timeout_ms: None,
        },
        None => return Err(V1Error::invalid_argument("missing mcp transport")),
    };
    st.mcp_control
        .upsert(request.name.clone(), config)
        .await
        .map_err(|error| {
            if error.starts_with("duplicate tool name") {
                V1Error::unavailable(error)
            } else {
                V1Error::internal(error)
            }
        })?;
    let statuses = st.mcp_control.status().await;
    let status = statuses
        .get(&request.name)
        .cloned()
        .unwrap_or(McpStatus::Disabled);
    Ok(Json(named_status(&st, &request.name, status).await))
}

async fn connect(
    State(st): State<ServerState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<pb::McpServerStatus>, V1Error> {
    let enabled = st
        .mcp_control
        .set_enabled(name.clone(), true)
        .await
        .map_err(V1Error::unavailable)?;
    if !enabled {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("unknown mcp server: {name}"),
        ));
    }
    let statuses = st.mcp_control.status().await;
    let status = statuses.get(&name).cloned().unwrap_or(McpStatus::Disabled);
    Ok(Json(named_status(&st, &name, status).await))
}

async fn disconnect(
    State(st): State<ServerState>,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<pb::McpServerStatus>, V1Error> {
    let disabled = st
        .mcp_control
        .set_enabled(name.clone(), false)
        .await
        .map_err(V1Error::unavailable)?;
    if !disabled {
        return Err(V1Error::new(
            hya_api::error::Code::NotFound,
            format!("unknown mcp server: {name}"),
        ));
    }
    let statuses = st.mcp_control.status().await;
    let status = statuses.get(&name).cloned().unwrap_or(McpStatus::Disabled);
    Ok(Json(named_status(&st, &name, status).await))
}

async fn start_auth(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<pb::StartMcpAuthResponse>, V1Error> {
    Err(V1Error::unavailable(format!(
        "mcp oauth start is not wired for {name}"
    )))
}

async fn complete_auth(
    AxumPath(name): AxumPath<String>,
) -> Result<Json<pb::McpServerStatus>, V1Error> {
    Err(V1Error::unavailable(format!(
        "mcp oauth completion is not wired for {name}"
    )))
}

async fn remove_auth(
    AxumPath(_name): AxumPath<String>,
) -> Result<Json<pb::RemoveMcpAuthResponse>, V1Error> {
    Err(V1Error::unavailable("mcp credential removal is not wired"))
}
