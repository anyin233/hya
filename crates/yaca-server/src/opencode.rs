use axum::Router;

use crate::ServerState;

mod catalog;
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
mod project_copy;
mod projection;
mod question;
mod session_context;
mod session_legacy;
mod session_prompt;
mod session_switch;
mod session_v2;

pub(in crate::opencode) use session_legacy::load_session;

pub(super) fn router() -> Router<ServerState> {
    Router::new()
        .merge(catalog::router())
        .merge(event::router())
        .merge(file::router())
        .merge(health::router())
        .merge(instance::router())
        .merge(integration::router())
        .merge(metadata::router())
        .merge(mcp::router())
        .merge(permission::router())
        .merge(project_copy::router())
        .merge(question::router())
        .merge(session_context::router())
        .merge(session_prompt::router())
        .merge(session_v2::router())
        .merge(session_switch::router())
        .merge(session_legacy::router())
}
