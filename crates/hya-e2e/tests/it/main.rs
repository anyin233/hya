//! Integration tests for `hya-e2e`, built as one binary; each module covers one
//! consolidated product-path seam and is safe to run in parallel (`--test-threads=4`).

mod p01_core_session;
mod p02_permissions_and_questions;
mod p03_mcp_and_custom_resources;
mod p04_subagents_and_mailbox;
mod p05_workflows;
mod p06_bundle_lifecycle_and_runtime;
mod p07_bundle_policies_and_extras;
mod p09_exec_headless;
mod p10_model_catalog_discovery;
mod p38_relay;
mod p43_store_robustness;
mod p44_v2_bare_bundles_and_migration;
