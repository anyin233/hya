//! Integration tests for `hya-backend`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod bare_launch;
mod bridge_cli;
mod bundle_cli;
mod compat_agent_cli;
mod db_writers;
mod lsp_runtime;
mod proxy_cli;
mod relay_doctor;
mod serve_daemon;
mod serve_grpc;
mod serve_lock;
mod serve_relay;
mod sessions_cli;
mod workflow_cli;
