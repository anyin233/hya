//! Integration tests for `hya-plugin`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod activation_hooks;
mod bundle_apis;
mod chain_load_order;
mod codec_lines;
mod command_dispatch;
mod configured_id_mismatch;
mod connect_all_observed_in;
mod crash_restart;
mod goal_evaluator_hooks;
mod host_dispatch;
mod injection_hooks;
mod kind_wire;
mod loop_should_stop_hooks;
mod manifest_config;
mod model_fallback_hooks;
mod permission_approve_hooks;
mod permission_identity;
mod plugin_tools;
mod posture_open_timeout;
mod posture_safe_timeout;
mod protocol_roundtrip;
mod respawn_declaration_drift;
mod text_complete_dispatch;
mod transport;
mod workspace_adapters;
