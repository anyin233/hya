//! Engine-side mailbox delivery + roster/channel queries (ADR-0001).
//!
//! These methods are the substance behind the [`MailboxPlane`] tools. Every team
//! comms event is appended to the TEAM-ROOT session's log (the root of the
//! sender's lineage), so one replay of that log reconstructs the whole team's
//! inboxes/channels/roster and the live [`EventBus`](crate::bus::EventBus) carries
//! them to the TUI for free.
//!
//! Delivery here means "the event is appended and folded into recipient inboxes
//! by the shared reducer" — idle sessions are NOT woken (that is Phase 4, which
//! subscribes to `MailSent` on the bus). The bus publish already happens inside
//! [`SessionEngine::emit`], so the wake hook has its seam without further change.

use hya_proto::{
    AgentName, Event, MailEndpoint, MailKind, RosterEntry, RosterStatus, SessionId, SubagentMode,
};
use hya_tool::{ChannelInfo, MailReceipt};

use crate::engine::SessionEngine;
use crate::error::CoreError;

/// The handle assigned to a team's root / main agent. Fixed (not derived from an
/// ordinal) because there is exactly one main agent per team, and a stable,
/// well-known handle keeps replay deterministic and lets members address it.
pub(crate) const MAIN_HANDLE: &str = "main";

impl SessionEngine {
    /// The team-root session for `session` (walks the `parent` chain to the top).
    async fn team_root(&self, session: SessionId) -> Result<SessionId, CoreError> {
        Ok(self.session_lineage(session).await?.0)
    }

    /// Append an `AgentRegistered` binding `handle` (+ its type + scheduling
    /// `mode`) to `agent_session` in the team-root log. Idempotent-friendly: the
    /// reducer keys the roster by handle, so re-registering the same handle
    /// refreshes the binding without dropping its live status.
    pub(crate) async fn record_agent_registered(
        &self,
        root: SessionId,
        agent_session: SessionId,
        handle: String,
        agent_type: AgentName,
        mode: SubagentMode,
    ) -> Result<(), CoreError> {
        self.emit(
            root,
            Event::AgentRegistered {
                session: root,
                agent_session,
                handle,
                agent_type,
                mode,
            },
        )
        .await
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
            Event::AgentActivityChanged {
                session: root,
                handle,
                status,
                current_task,
            },
        )
        .await
    }

    /// Ensure the team root itself has a roster entry, registering it as
    /// [`MAIN_HANDLE`] the first time. Returns the main agent's handle.
    pub(crate) async fn ensure_root_registered(
        &self,
        root: SessionId,
    ) -> Result<String, CoreError> {
        let projection = self.read_projection(root).await?;
        if let Some(entry) = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == root)
        {
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
        self.record_agent_registered(
            root,
            root,
            MAIN_HANDLE.to_string(),
            agent_type,
            SubagentMode::Transient,
        )
        .await?;
        Ok(MAIN_HANDLE.to_string())
    }

    /// Resolve the acting `session` to its team-scoped handle. The root falls back
    /// to lazily-registered [`MAIN_HANDLE`]; any other unregistered session is an
    /// error (only spawned/registered members can act on the mailbox).
    async fn resolve_handle(
        &self,
        root: SessionId,
        session: SessionId,
    ) -> Result<String, CoreError> {
        let projection = self.read_projection(root).await?;
        if let Some(entry) = projection
            .team
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

    /// Send mail from `from_session` to a handle or `#channel`. Appends a single
    /// `MailSent` to the team-root log; the reducer fans a channel send out to
    /// every current subscriber. Returns a receipt with the resolved sender handle
    /// and the recipient count at send time.
    ///
    /// Public so callers outside the mailbox service (the resident supervisor's
    /// tests, integration drivers) can inject team mail directly; the normal path
    /// is still the `MailboxPlane` → [`run_mailbox_service`](crate::run_mailbox_service).
    pub async fn mail_send(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        // Count recipients from the membership snapshot BEFORE the append; a direct
        // send always has exactly one recipient.
        let recipients = match &to {
            MailEndpoint::Handle(_) => 1,
            MailEndpoint::Channel(channel) => self
                .read_projection(root)
                .await?
                .team
                .channels
                .get(channel)
                .map_or(0, |ch| ch.members.len()),
        };
        self.emit(
            root,
            Event::MailSent {
                session: root,
                from: from.clone(),
                to: to.clone(),
                kind,
                body,
            },
        )
        .await?;
        Ok(MailReceipt {
            from,
            to,
            recipients,
        })
    }

    /// Subscribe the acting agent's handle to `channel`.
    pub(crate) async fn channel_join(
        &self,
        session: SessionId,
        channel: String,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
        self.emit(
            root,
            Event::ChannelJoined {
                session: root,
                channel,
                member,
            },
        )
        .await
    }

    /// Unsubscribe the acting agent's handle from `channel`.
    pub(crate) async fn channel_leave(
        &self,
        session: SessionId,
        channel: String,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
        self.emit(
            root,
            Event::ChannelLeft {
                session: root,
                channel,
                member,
            },
        )
        .await
    }

    /// The live roster for the team `session` belongs to (sorted by handle).
    pub(crate) async fn team_roster(
        &self,
        session: SessionId,
    ) -> Result<Vec<RosterEntry>, CoreError> {
        let root = self.team_root(session).await?;
        let projection = self.read_projection(root).await?;
        Ok(projection.team.roster.into_values().collect())
    }

    /// The channels + membership for the team `session` belongs to (sorted by name).
    pub(crate) async fn team_channels(
        &self,
        session: SessionId,
    ) -> Result<Vec<ChannelInfo>, CoreError> {
        let root = self.team_root(session).await?;
        let projection = self.read_projection(root).await?;
        Ok(projection
            .team
            .channels
            .into_iter()
            .map(|(name, channel)| ChannelInfo {
                name,
                members: channel.members.into_iter().collect(),
                messages: channel.log.len(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use hya_proto::{AgentName, ModelRef, Projection};
    use hya_provider::ProviderRouter;
    use hya_store::SessionStore;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    use super::*;
    use crate::bus::EventBus;
    use crate::engine::{CreateSession, SessionEngine};

    async fn engine() -> SessionEngine {
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new());
        let tools = Arc::new(ToolRegistry::builtins());
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
        SessionEngine::new(store, router, tools, permission, EventBus::default())
    }

    async fn root_team(engine: &SessionEngine) -> SessionId {
        engine
            .create(CreateSession {
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap()
    }

    /// Full delivery path through the engine: two registered members join
    /// `#build`, the main agent posts, and the message lands in BOTH inboxes —
    /// then a fresh replay of the team-root log reconstructs identical state. This
    /// exercises the routing the pure reducer test cannot: lineage → root log,
    /// handle resolution, recipient counting, and store replay.
    #[tokio::test]
    async fn channel_send_routes_to_root_log_and_survives_store_replay() {
        let engine = engine().await;
        let root = root_team(&engine).await;
        // Two members registered under the same team root (as spawn would do).
        let reviewer_1 = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("reviewer"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        let reviewer_2 = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("reviewer"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        engine
            .record_agent_registered(
                root,
                reviewer_1,
                "reviewer-1".to_string(),
                AgentName::new("reviewer"),
                SubagentMode::Resident,
            )
            .await
            .unwrap();
        engine
            .record_agent_registered(
                root,
                reviewer_2,
                "reviewer-2".to_string(),
                AgentName::new("reviewer"),
                SubagentMode::Resident,
            )
            .await
            .unwrap();

        // Both members subscribe using their own child session (handle resolved
        // from the roster), and the MAIN agent posts to the channel.
        engine
            .channel_join(reviewer_1, "build".to_string())
            .await
            .unwrap();
        engine
            .channel_join(reviewer_2, "build".to_string())
            .await
            .unwrap();
        let receipt = engine
            .mail_send(
                root,
                MailEndpoint::Channel("build".to_string()),
                MailKind::Announcement,
                "ship it".to_string(),
            )
            .await
            .unwrap();
        assert_eq!(
            receipt.from, MAIN_HANDLE,
            "main auto-registers on first send"
        );
        assert_eq!(receipt.recipients, 2, "both subscribers counted");

        // The team-root projection folded the post into both inboxes.
        let projection = engine.read_projection(root).await.unwrap();
        let body_of = |handle: &str| {
            projection
                .team
                .inboxes
                .get(handle)
                .map(|m| m.iter().map(|x| x.body.clone()).collect::<Vec<_>>())
                .unwrap_or_default()
        };
        assert_eq!(body_of("reviewer-1"), vec!["ship it".to_string()]);
        assert_eq!(body_of("reviewer-2"), vec!["ship it".to_string()]);
        assert!(
            projection.team.roster.contains_key(MAIN_HANDLE),
            "main is on the roster"
        );

        // A fresh replay from the store reconstructs identical team state.
        let replayed = Projection::from_events(&engine.replay(root).await.unwrap());
        assert_eq!(replayed.team, projection.team);
    }

    /// A session that was never spawned/registered cannot use the mailbox — its
    /// send is rejected rather than silently delivered under a bogus handle.
    #[tokio::test]
    async fn unregistered_non_root_sender_is_rejected() {
        let engine = engine().await;
        let root = root_team(&engine).await;
        let stranger = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("reviewer"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        let result = engine
            .mail_send(
                stranger,
                MailEndpoint::Handle("main".to_string()),
                MailKind::Message,
                "hi".to_string(),
            )
            .await;
        assert!(matches!(result, Err(CoreError::Invalid(_))));
    }
}
