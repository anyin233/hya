//! Integration tests for `hya-backend`, built as one binary; each module covers one area.

mod bare_launch;
mod bare_runtime;
mod bridge_cli;
mod bundle_api;
mod bundle_cli;
mod common;
mod compat_agent_cli;
mod db_ownership;
mod db_writers;
mod lsp_runtime;
mod provider_cli;
mod proxy_cli;
mod relay_doctor;
mod serve_daemon;
mod serve_grpc;
mod serve_lock;
mod serve_relay;
mod sessions_cli;
mod workflow_cli;
