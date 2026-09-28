//! Integration tests for `hya-proto`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

#[path = "support/event_script.rs"]
mod event_script;

mod legacy_flat_mailbox;
mod projection;
mod projection_snapshot;
mod revert_projection;
mod session_archive;
mod session_ephemeral;
mod session_project;
mod workflow_projection;
