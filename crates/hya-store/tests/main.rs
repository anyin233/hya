//! Integration tests for `hya-store`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

#[path = "../../hya-proto/tests/support/event_script.rs"]
mod event_script;

mod admission;
mod agent_model_preference;
mod bundle_registry;
mod file_blob;
mod interrupted_recovery;
mod persistence;
mod project;
mod projection_cache;
mod r10_certification;
mod resident_claim;
mod saved_permission_scope;
mod store;
mod workflow_recovery;
