//! Integration tests for `hya-bundle`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod agent_channels;
mod agent_set;
mod apis;
mod catalog;
mod docs_example;
mod extra_bundles;
mod first_party;
mod markdown;
mod namespace;
mod package_inspection;
mod package_prepare;
mod package_staging;
mod permission_modes;
mod prepare;
mod process_resources;
mod validation;
