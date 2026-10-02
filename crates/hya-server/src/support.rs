//! Shared server support modules: catalogs, guidance, PTY, worktrees, git,
//! and the config bag used by the `/v1` binding and process state.
//! Re-homed from the retired Compat surface; route handlers there were
//! deleted, helpers survive here unchanged.

pub(crate) mod bound_agent_metadata;
pub(crate) mod catalog_place;
pub(crate) mod command_catalog;
pub(crate) mod command_sources;
pub(crate) mod git;
pub(crate) mod global_state;
pub(crate) mod pty_runtime;
pub(crate) mod pty_shell;
pub(crate) mod pty_state;
pub(crate) mod reference;
pub(crate) mod reference_cache;
pub(crate) mod reference_entries;
pub(crate) mod reference_repository;
pub(crate) mod skill_catalog;
pub(crate) mod worktree_git;
pub(crate) mod worktree_git_info;
