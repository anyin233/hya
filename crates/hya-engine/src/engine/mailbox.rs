//! Engine-side mailbox delivery + roster/channel queries (ADR-0001).
//!
//! These methods implement the host behind the [`hya_tool::MailboxPlane`] tools.
//! Every team-comms event is appended to the **team-root** session log (top of
//! the sender's parent lineage), so one replay reconstructs inboxes, channels,
//! and roster. Live observers receive the same envelopes through
//! [`crate::EventBus`].
//!
//! **Delivery** means the event is appended and folded by the shared projection
//! reducer. Resident supervisors (and other bus subscribers) wake idle actors on
//! mail; this module itself only persists and publishes.

mod query;
mod request;

#[cfg(test)]
mod tests;

pub(crate) use hya_agent_channels_core::{MAIN_HANDLE, dm_channel_between};

use hya_proto::{ActorClaim, AgentName, RosterStatus, SessionId, SubagentMode, scope};
use hya_tool::extension::{ExtensionEvent, team_projection};

use crate::engine::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// The team-root session for `session` (walks the `parent` chain to the top).
    pub(crate) async fn team_root(&self, session: SessionId) -> Result<SessionId, CoreError> {
        Ok(self.session_lineage(session).await?.0)
    }

    /// Append an `AgentActivityChanged` updating a member's live roster status
    /// (idle ⇄ busy / done / failed) and optional current-task label. Appended to
    /// the team-root log by the resident supervisor (ADR-0002).
    pub(crate) async fn record_agent_activity(
        &self,
        root: SessionId,
        handle: String,
        status: RosterStatus,
        current_task: Option<String>,
    ) -> Result<(), CoreError> {
        self.emit(
            root,
            ExtensionEvent::AgentActivityChanged {
                session: root,
                handle,
                status,
                current_task,
            }
            .into_event(),
        )
        .await
    }

    /// Ensure the team root itself has a roster entry, registering it as
    /// [`MAIN_HANDLE`] the first time. Returns the main agent's handle.
    pub(crate) async fn ensure_root_registered(
        &self,
        root: SessionId,
    ) -> Result<String, CoreError> {
        self.ensure_root_registered_for_actor(root, None).await
    }

    pub(crate) async fn ensure_root_registered_for_actor(
        &self,
        root: SessionId,
        actor_claim: Option<&ActorClaim>,
    ) -> Result<String, CoreError> {
        let projection = self.read_projection_shared(root).await?;
        let team = team_projection(&projection);
        if let Some(entry) = team.roster.values().find(|entry| entry.session == root) {
            return Ok(entry.handle.clone());
        }
        let agent_type = projection
            .session
            .agent
            .clone()
            .unwrap_or_else(|| AgentName::new(MAIN_HANDLE));
        // The main/root agent is registered as transient: it is the team root, not
        // a resident subagent. Its actor behaviour (woken by child mail /
        // quiescence) is driven by the resident supervisor, not this flag.
        self.emit_for_actor(
            actor_claim,
            root,
            ExtensionEvent::AgentRegistered {
                session: root,
                agent_session: root,
                handle: MAIN_HANDLE.to_string(),
                parent: None,
                agent_type,
                mode: SubagentMode::Transient,
            }
            .into_event(),
        )
        .await?;
        Ok(MAIN_HANDLE.to_string())
    }

    /// Resolve the acting `session` to its canonical path. The root falls back to
    /// lazily-registered [`MAIN_HANDLE`]; any other unregistered session is an
    /// error (only spawned/registered members can act on the mailbox).
    pub(crate) async fn resolve_handle(
        &self,
        root: SessionId,
        session: SessionId,
    ) -> Result<String, CoreError> {
        let projection = self.read_projection_shared(root).await?;
        if let Some(entry) = team_projection(&projection)
            .roster
            .values()
            .find(|entry| entry.session == session)
        {
            return Ok(entry.handle.clone());
        }
        if session == root {
            return self.ensure_root_registered(root).await;
        }
        Err(CoreError::Invalid(
            "this agent has no team handle yet; it must be spawned as a team member to use the mailbox"
                .to_string(),
        ))
    }

    /// The canonical path of the agent that spawned `session` — the unit
    /// `session` belongs to.
    ///
    /// Derived from `session`'s own lineage so callers that only hold the child
    /// (the `register_existing_resident*` entry points) need not thread a parent
    /// path through their signatures.
    ///
    /// Falls back to the team root when the parent is unknown or itself
    /// unregistered. That reproduces the pre-scoping flat arrangement for such a
    /// session rather than failing its spawn: an agent placed at the root is
    /// still reachable, whereas a rejected registration would strand it.
    pub(crate) async fn parent_agent_path(&self, root: SessionId, session: SessionId) -> String {
        let fallback = scope::ROOT_HANDLE.to_string();
        let Ok(projection) = self.read_projection_shared(session).await else {
            return fallback;
        };
        let Some(parent) = projection.session.parent else {
            return fallback;
        };
        if parent == root {
            return fallback;
        }
        self.resolve_handle(root, parent).await.unwrap_or(fallback)
    }
}
