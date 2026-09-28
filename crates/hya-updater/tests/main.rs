//! Integration tests for `hya-updater`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod break_glass;
mod fault_matrix;
mod independence;
mod metadata_verify;
mod stage_and_activate;
