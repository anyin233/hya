//! Shared server support modules: catalogs, guidance, PTY, worktrees, git,
//! and the config bag used by the `/v1` binding and process state.
//! Re-homed from the retired Compat surface; route handlers there were
//! deleted, helpers survive here unchanged.

#[allow(dead_code)]
pub(crate) mod bound_agent_metadata;
#[allow(dead_code)]
pub(crate) mod command_catalog;
#[allow(dead_code)]
pub(crate) mod command_sources;
#[allow(dead_code)]
pub(crate) mod git;
#[allow(dead_code)]
pub(crate) mod model_ref;
#[allow(dead_code)]
pub(crate) mod pty_runtime;
#[allow(dead_code)]
pub(crate) mod pty_shell;
#[allow(dead_code)]
pub(crate) mod pty_state;
#[allow(dead_code)]
pub(crate) mod reference;
#[allow(dead_code)]
pub(crate) mod reference_cache;
#[allow(dead_code)]
pub(crate) mod reference_entries;
#[allow(dead_code)]
pub(crate) mod reference_repository;
#[allow(dead_code)]
pub(crate) mod skill_catalog;
#[allow(dead_code)]
pub(crate) mod workspace_id;
#[allow(dead_code)]
pub(crate) mod worktree_git;
#[allow(dead_code)]
pub(crate) mod worktree_git_info;

pub(crate) mod global_state;
