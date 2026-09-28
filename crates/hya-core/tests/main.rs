//! Integration tests for `hya-core`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod support;

mod agent_catalog;
mod agent_model_configuration;
mod agent_model_preferences;
mod agent_resource_view;
mod builtin_agents;
mod bundle_api_routing;
mod bundle_hook_generation;
mod bundle_usage_reads;
mod catalog_scope;
mod category_routing;
mod channel_policy;
mod command_hooks;
mod compact_context;
mod compaction_hooks;
mod coordination_tools;
mod fixed_system_agents;
mod goal_loop;
mod handle_naming;
mod historical_agent_identity;
mod hooks_seam;
mod loop_gate;
mod loop_mode;
mod loop_predicate;
mod mcp_background;
mod model_fallback;
mod model_probe;
mod model_selection;
mod permission_modes;
mod prompt_attachments;
mod report;
mod report_ends_turn;
mod resident;
mod resident_recovery;
mod revert;
mod role_selector_vs_can_spawn_roster;
mod round_rebind;
mod round_rebind_hooks;
mod runtime_catalog_refresh;
mod runtime_generation;
mod runtime_registry;
mod runtime_sources;
mod runtime_turn_binding;
mod session_catalog_scope;
mod session_cleanup;
mod session_grant_scope;
mod session_hook_scope;
mod session_project;
mod session_roots;
mod shell_direct;
mod single_active_turn;
mod stream_round;
mod subagent;
mod text_complete_hooks;
mod todo_projection;
mod token_accounting;
mod tool_error_payload;
mod tool_filtering;
mod turn_end;
mod turn_loop;
mod usage_attribution;
mod usage_ledger;
mod wait;
mod workflow;
mod worktree_lifecycle;
