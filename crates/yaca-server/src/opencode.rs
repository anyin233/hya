use axum::Router;

use crate::ServerState;

mod catalog;
mod control;
mod event;
mod file;
mod health;
mod instance;
mod integration;
mod location;
mod mcp;
mod metadata;
mod model_ref;
mod permission;
mod project;
mod project_copy;
mod projection;
mod pty;
mod question;
mod session_context;
mod session_delete;
mod session_diff;
mod session_fork;
mod session_legacy;
mod session_list;
mod session_part_update;
mod session_prompt;
mod session_share;
mod session_summarize;
mod session_switch;
mod session_update;
mod session_v2;
mod tui;

pub(crate) use mcp::McpHttpState;
pub(crate) use project::ProjectState;
pub(in crate::opencode) use session_legacy::load_session;
pub(crate) use tui::TuiState;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(catalog::router())
        .merge(control::router())
        .merge(event::router())
        .merge(file::router())
        .merge(health::router())
        .merge(instance::router())
        .merge(integration::router())
        .merge(metadata::router())
        .merge(mcp::router())
        .merge(permission::router())
        .merge(project::router())
        .merge(project_copy::router())
        .merge(pty::router())
        .merge(question::router())
        .merge(session_context::router())
        .merge(session_prompt::router())
        .merge(session_v2::router())
        .merge(session_switch::router())
        .merge(session_legacy::router())
        .merge(tui::router())
}
