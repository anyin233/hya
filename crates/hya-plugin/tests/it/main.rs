//! Integration tests for `hya-plugin`, built as one binary; each module covers one area.

mod bundle_apis;
mod chain_load_order;
mod codec_lines;
mod command_dispatch;
mod configured_id_mismatch;
mod connect_all_observed_in;
mod crash_restart;
mod enrichment_timeout;
mod goal_evaluator_hooks;
mod guard_timeout;
mod hook_wire_golden;
mod host_dispatch;
mod injection_hooks;
mod kind_wire;
mod loop_should_stop_hooks;
mod manifest_config;
mod model_fallback_hooks;
mod permission_approve_hooks;
mod permission_identity;
mod plugin_tools;
mod protocol_roundtrip;
mod respawn_declaration_drift;
mod text_complete_dispatch;
mod transport;
