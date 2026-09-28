//! Integration tests for `hya-server`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod support;

mod v1_agent_models_api;
mod v1_api;
mod v1_bundle_apis;
mod v1_catalog_scope;
mod v1_grpc_parity;
mod v1_host_guard;
mod v1_interaction_stream;
mod v1_live_stream;
mod v1_message_attribution;
mod v1_one_server;
mod v1_permission_modes;
mod v1_project;
mod v1_project_sessions;
mod v1_prompt_attachments;
mod v1_providers_api;
mod v1_reasoning_effort;
mod v1_relay_host;
mod v1_revert_fork;
mod v1_saved_rules;
mod v1_scope;
mod v1_session_archive;
mod v1_session_ephemeral;
mod v1_session_list_stream;
mod v1_session_title;
mod v1_status_data;
mod v1_stream_shutdown;
mod v1_tool_parts;
