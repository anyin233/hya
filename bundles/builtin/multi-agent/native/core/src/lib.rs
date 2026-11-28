//! Native implementation of the `hya/multi-agent` built-in bundle exported via
//! Lockstep Native Runtime ABI v2 (ADR-0002, ADR-0015, ADR-0039).
//!
//! Exports:
//! - Managed [`MultiAgentService`] (`NativeService`)
//! - Model tools: `list_agents`, `task`, `search_agent`, `archive`, and `wait`
//! - [`MultiAgentLifecycleHook`] (`NativeHook`)
//! - [`MultiAgentBundleApiHandler`] (`NativeBundleApiHandler`: `/members`, `/archive`, `/steer`)
//! - [`MultiAgentProjectionReducer`] (`ProjectionReducer`)
//! - Bounded [`SubagentGovernor`], [`HandleNamer`], [`claims`], and [`handoff`]
//!
//! All host interactions flow exclusively through `Arc<dyn HostRuntime>` with
//! zero dependencies on `hya-store`, `sqlx`, `hya-engine`, `hya-server`, `axum`,
//! or `tonic`.

use std::sync::Arc;

/// Native HTTP/gRPC `BundleApi` handler (`/members`, `/archive`, `/steer`).
pub mod api;
/// Actor claim leases (`bundle_claim`) and team checkpoints (`bundle_state`).
pub mod claims;
/// Bounded subagent stream semaphore governor and fixed depth ceiling.
pub mod governor;
/// Subagent handle naming (`<prefix>-<operator>`) and reservation tracking.
pub mod handle_naming;
/// Model-backed and deterministic degraded terminal handoff generation.
pub mod handoff;
/// Native session-lifecycle hook for multi-agent transient state cleanup.
pub mod hook;
/// Durable `Event::Extension` projection reducer for `hya/multi-agent`.
pub mod reducer;
/// Managed [`hya_runtime_abi::NativeService`] owning multi-agent supervision.
pub mod service;
/// Model tools: `list_agents`, `task`, `search_agent`, and `archive`.
pub mod tools;

pub use api::MultiAgentBundleApiHandler;
pub use claims::{
    FencedActorLease, MULTI_AGENT_BUNDLE_ID, TeamCheckpoint, acquire_actor_claim,
    lease_root_session, lease_to_actor_claim, list_active_actor_claims, live_member_claim_count,
    load_team_checkpoint, read_actor_claim, recover_stale_actor_claims, release_actor_claim,
    renew_actor_claim, save_team_checkpoint,
};
pub use governor::{MAX_SUBAGENT_DEPTH, SubagentGovernor, SubagentLimits};
pub use handle_naming::{
    HANDLE_SEED_ENV, HandleNamer, HandleReservation, HandleRng, PAIR_NAME_ATTEMPTS,
    SINGLE_NAME_ATTEMPTS, SplitMix64, mint_leaf, operator_names, team_leaves,
};
pub use handoff::{
    DEFAULT_HANDOFF_MAX_TOKENS, STATE_HANDOFF_TEMPLATE, TerminalHandoff, TerminalHandoffOptions,
    build_handoff_messages, degraded_handoff_doc, degraded_terminal_handoff,
    generate_terminal_handoff,
};
pub use hook::{MULTI_AGENT_LIFECYCLE_HOOK_ID, MultiAgentLifecycleHook};
pub use reducer::{MultiAgentProjectionReducer, decode_multi_agent_projection};
pub use service::{
    ArchivedAgentRow, MULTI_AGENT_SERVICE_ID, MemberTarget, MembersSnapshot, MultiAgentService,
    USER_STEER_PREFIX, resolve_member_target,
};
pub use tools::{ArchiveTool, ListAgentsTool, SearchAgentTool, TaskTool};

/// Register all `hya/multi-agent` services, tools, hooks, API handlers, and reducers into `out`.
pub fn register_bundle(out: &mut hya_runtime_abi::NativeRegistrations) {
    let service = Arc::new(MultiAgentService::new());
    out.services
        .push(Arc::clone(&service) as Arc<dyn hya_runtime_abi::NativeService>);
    out.tools
        .push(Arc::new(ListAgentsTool) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(TaskTool) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(SearchAgentTool::new(Arc::clone(&service))) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(ArchiveTool) as Arc<dyn hya_tool::Tool>);
    out.tools
        .push(Arc::new(hya_tool::lifecycle::WaitTool::new(false)) as Arc<dyn hya_tool::Tool>);
    out.hooks
        .push(Arc::new(MultiAgentLifecycleHook::new(Arc::clone(&service)))
            as Arc<dyn hya_runtime_abi::NativeHook>);
    out.api_handlers
        .push(Arc::new(MultiAgentBundleApiHandler::new(service))
            as Arc<dyn hya_runtime_abi::NativeBundleApiHandler>);
    out.projection_reducers
        .push(Arc::new(MultiAgentProjectionReducer) as Arc<dyn hya_runtime_abi::ProjectionReducer>);
}

#[cfg(test)]
mod tests;
