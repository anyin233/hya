//! Integration tests for `hya-server`, built as one binary; each module covers one contract area.

#[path = "support/harness.rs"]
mod harness;
#[path = "support/mcp.rs"]
mod mcp_fixture;
mod support;

mod v2_api_core;
mod v2_bundles_and_skills;
mod v2_grpc_parity;
mod v2_interactions_and_tools;
mod v2_projects_and_scope;
mod v2_providers_and_agents;
mod v2_relay_and_host_guard;
mod v2_sessions_lifecycle;
mod v2_streaming_and_shutdown;
