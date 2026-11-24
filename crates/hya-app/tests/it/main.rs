//! Integration tests for `hya-app`, built as one binary; each module covers one area.

mod support;

mod agent_model_control;
mod config_minimal_edit;
mod installed_bundle_refresh;
mod nested_spawn_tree;
mod plugin_adapter;
mod preset_inventory;
mod project_bundle_install;
mod project_bundles;
mod project_plugins;
mod public_delete;
mod resident_lifecycle;
mod resident_loss_review;
mod runtime;
mod spawn_admission;
mod support_fixture_guard;
mod workflow_control;
