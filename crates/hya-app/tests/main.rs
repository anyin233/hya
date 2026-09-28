//! Integration tests for `hya-app`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod support;

mod agent_model_control;
mod config_minimal_edit;
mod first_party_subagent_runtime;
mod installed_bundle_refresh;
mod nested_spawn_tree;
mod preset_inventory;
mod project_bundle_install;
mod project_bundles;
mod project_plugins;
mod spawn_admission;
mod support_fixture_guard;
mod workflow_control;
