use std::collections::BTreeMap;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use yaca_mcp::McpStatus;

use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/mcp", get(status))
}

async fn status(State(st): State<ServerState>) -> Json<BTreeMap<String, McpStatus>> {
    Json(st.mcp_manager.status())
}
