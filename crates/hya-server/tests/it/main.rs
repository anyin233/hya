//! Integration tests for `hya-server`, built as one binary; each module covers one contract area.

#[path = "support/harness.rs"]
mod harness;
#[path = "support/mcp.rs"]
mod mcp_fixture;
mod support;

mod v1_api_core;
mod v1_bundles_and_skills;
mod v1_grpc_parity;
mod v1_interactions_and_tools;
mod v1_projects_and_scope;
mod v1_providers_and_agents;
mod v1_relay_and_host_guard;
mod v1_sessions_lifecycle;
mod v1_streaming_and_shutdown;
