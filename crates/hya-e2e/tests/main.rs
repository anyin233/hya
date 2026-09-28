//! Integration tests for `hya-e2e`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod p01_session_prompt;
mod p02_tool_loop_fs;
mod p03_permissions;
mod p04_questions;
mod p05_skills;
mod p06_mcp;
mod p07_session_lifecycle;
mod p08_subagent_task;
mod p09_nested_subagent;
mod p10_agent_roster;
mod p11_hyabundle;
mod p12_context_api;
mod p13_project_agents_context;
mod p14_compact_summarize;
mod p15_todo_and_edit;
mod p16_swarm_mailbox;
mod p17_workflow_composition;
mod p18_custom_slash_resources;
mod p19_workflow_model_routing;
mod p20_model_catalog_discovery;
mod p21_agent_model_preference;
mod p22_dispatch_model_resolution;
mod p23_mcp_background;
mod p24_bundle_schemas;
mod p25_goal_loop_bundle;
mod p26_plugin_bundle;
mod p27_bundle_process;
mod p28_subagent_bundle;
mod p29_claude_bundle;
mod p30_channel_bundle;
mod p31_extra_bundles;
mod p32_jev_model_router;
mod p33_model_fallback;
mod p34_bundle_apis;
mod p35_token_summary;
mod p36_wait_archive;
mod p37_project_permission_scope;
mod p38_relay;
