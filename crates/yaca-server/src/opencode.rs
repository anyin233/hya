use axum::Router;

use crate::ServerState;

mod catalog;
mod control;
mod errors;
mod event;
mod experimental;
mod experimental_sync;
mod experimental_worktree;
mod file;
mod global;
mod health;
mod instance;
mod integration;
mod location;
mod mcp;
mod mcp_state;
mod message_context_parts;
mod message_parts;
mod message_projection;
mod metadata;
mod model_ref;
mod openapi_doc;
mod permission;
mod project;
mod project_copy;
mod projection;
mod pty;
mod pty_payload;
mod pty_state;
mod question;
mod session_context;
mod session_context_messages;
mod session_context_tool_state;
mod session_context_tool_time;
mod session_create_legacy;
mod session_delete;
mod session_diff;
mod session_fork;
mod session_legacy;
mod session_list;
mod session_message_v2;
mod session_message_v2_before;
mod session_part_update;
mod session_prompt;
mod session_prompt_legacy;
mod session_revert;
mod session_share;
mod session_summarize;
mod session_switch;
mod session_unavailable;
mod session_update;
mod session_v2;
mod session_v2_cursor;
mod sse;
mod tui;
mod worktree_git;

pub(crate) use global::GlobalState;
pub(crate) use mcp_state::McpHttpState;
pub(crate) use project::ProjectState;
pub(crate) use pty_state::PtyState;
pub(in crate::opencode) use session_legacy::load_session;
pub(crate) use tui::TuiState;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(catalog::router())
        .merge(control::router())
        .merge(event::router())
        .merge(experimental::router())
        .merge(experimental_worktree::router())
        .merge(file::router())
        .merge(global::router())
        .merge(health::router())
        .merge(instance::router())
        .merge(integration::router())
        .merge(metadata::router())
        .merge(mcp::router())
        .merge(openapi_doc::router())
        .merge(permission::router())
        .merge(project::router())
        .merge(project_copy::router())
        .merge(pty::router())
        .merge(question::router())
        .merge(session_create_legacy::router())
        .merge(session_context::router())
        .merge(session_message_v2::router())
        .merge(session_prompt::router())
        .merge(session_prompt_legacy::router())
        .merge(session_revert::router())
        .merge(session_v2::router())
        .merge(session_switch::router())
        .merge(session_legacy::router())
        .merge(tui::router())
}
