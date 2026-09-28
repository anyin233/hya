//! Integration tests for `hya-tool`, compiled as one test binary.
//!
//! Every `tests/*.rs` file is a module here (not its own binary) so the
//! crate links once. Files that mutate process-global state (environment,
//! working directory) stay separate `[[test]]` targets in `Cargo.toml`.

mod allow_model_no_ask;
mod apply_patch;
mod apply_patch_formatter;
mod apply_patch_lsp;
mod ask_user;
mod base_tools_preset;
mod edit;
mod extended_bundle;
mod external_directory_scope;
mod formatter;
mod formatter_bom;
mod glob_grep;
mod grep_hashline;
mod handle_read;
mod handle_ref;
mod handle_router;
mod hashline_atomic_contract;
mod hashline_contract;
mod invalid;
mod lsp;
mod lsp_write_edit;
mod mailbox_team_status;
mod namespace;
mod output_spill;
mod permission_identity;
mod plan_exit;
mod project_roots;
mod read;
mod read_limits;
mod read_missing;
mod remaining_bundle;
mod scheme_dispatch;
mod send_tool;
mod shell;
mod task;
mod todo_tools;
mod tool;
mod wait_override;
mod webfetch;
mod websearch;
mod workflow;
mod write;
