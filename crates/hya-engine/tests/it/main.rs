//! Integration tests for `hya-engine`, built as one binary; each module covers one domain.

mod support;

mod catalog_and_agents;
mod coding_extras_hooks;
mod goal_and_loop;
mod hooks_and_scope;
mod permissions_and_tools;
mod report_and_wait;
mod retry_fallback;
mod revert_and_usage;
mod shell_and_attachments;
mod subagent_lifecycle;
mod turn_end_and_recovery;
mod turn_loop;
mod workflow;
