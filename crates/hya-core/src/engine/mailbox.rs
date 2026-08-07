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

use hya_proto::{
    AgentName, Event, MailEndpoint, MailKind, RosterStatus, ScopedRoster, SessionId, SubagentMode,
    scope,
};
use hya_tool::{ChannelInfo, MailReceipt};

use crate::engine::SessionEngine;
use crate::error::CoreError;

/// The handle assigned to a team's root / main agent. Fixed (not derived from an
/// ordinal) because there is exactly one main agent per team, and a stable,
/// well-known handle keeps replay deterministic and lets members address it.
///
/// Aliases [`scope::ROOT_HANDLE`] rather than repeating the literal: the reducer
/// derives every canonical path from that constant, so a second definition that
/// drifted would silently split the root into two roster entries.
pub(crate) const MAIN_HANDLE: &str = scope::ROOT_HANDLE;

impl SessionEngine {
    /// The team-root session for `session` (walks the `parent` chain to the top).
    async fn team_root(&self, session: SessionId) -> Result<SessionId, CoreError> {
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
        self.ensure_root_registered_for_actor(root, None).await
    }

    pub(crate) async fn ensure_root_registered_for_actor(
        &self,
        root: SessionId,
        actor_claim: Option<&hya_store::ActorClaim>,
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
        self.emit_for_actor(
            actor_claim,
            root,
            Event::AgentRegistered {
                session: root,
                agent_session: root,
                handle: MAIN_HANDLE.to_string(),
                parent: None,
                agent_type,
                mode: SubagentMode::Transient,
            },
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
        let Ok(projection) = self.read_projection(session).await else {
            return fallback;
        };
        let Some(parent) = projection.session.parent else {
            return fallback;
        };
        self.resolve_handle(root, parent).await.unwrap_or(fallback)
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
        self.mail_send_for_actor(from_session, to, kind, body, None)
            .await
    }

    pub(crate) async fn mail_send_for_actor(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        #[cfg(test)]
        if matches!(&to, MailEndpoint::Handle(_))
            && let Some(gate) = self.direct_mail_pre_append_gate.as_ref()
        {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if let MailEndpoint::Handle(handle) = &to {
            let envelope = self
                .store()
                .append_direct_mail(root, from.clone(), handle.clone(), kind, body, actor_claim)
                .await?;
            self.publish_envelope(envelope);
            return Ok(MailReceipt {
                from,
                to,
                recipients: 1,
            });
        }
        let channel = match &to {
            MailEndpoint::Channel(channel) => channel.clone(),
            MailEndpoint::Handle(_) => {
                return Err(CoreError::Invalid(
                    "mail endpoint was not a channel after direct delivery".to_string(),
                ));
            }
        };
        let (envelope, recipients) = self
            .store()
            .append_channel_mail(root, from.clone(), channel, kind, body, actor_claim)
            .await?;
        self.publish_envelope(envelope);
        Ok(MailReceipt {
            from,
            to,
            recipients,
        })
    }

    /// Post a one-way announcement to the unit the acting agent leads (R6).
    ///
    /// Reaches direct reports only. A whole-swarm announcement costs one call per
    /// level, each made deliberately by that level's leader.
    pub async fn mail_announce(
        &self,
        from_session: SessionId,
        body: String,
    ) -> Result<MailReceipt, CoreError> {
        self.mail_announce_for_actor(from_session, body, None).await
    }

    pub(crate) async fn mail_announce_for_actor(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        let (envelope, recipients) = self
            .store()
            .append_announce_mail(root, from.clone(), body, actor_claim)
            .await?;
        self.publish_envelope(envelope);
        Ok(MailReceipt {
            to: MailEndpoint::Channel(scope::announce_channel_of(&from)),
            from,
            recipients,
        })
    }

    /// Subscribe the acting agent to `channel` within its own unit.
    ///
    /// `channel` is the bare name as written (`build`, or `^build` for a leader's
    /// home unit). Resolution happens against the acting agent's position, so a
    /// join can never reach another unit's channel of the same name.
    pub(crate) async fn channel_join(
        &self,
        session: SessionId,
        channel: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
        let channel = self.resolve_channel_key(root, &member, &channel).await?;
        self.emit_for_actor(
            actor_claim,
            root,
            Event::ChannelJoined {
                session: root,
                channel,
                member,
            },
        )
        .await
    }

    /// Unsubscribe the acting agent from `channel` within its own unit.
    pub(crate) async fn channel_leave(
        &self,
        session: SessionId,
        channel: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
        let channel = self.resolve_channel_key(root, &member, &channel).await?;
        self.emit_for_actor(
            actor_claim,
            root,
            Event::ChannelLeft {
                session: root,
                channel,
                member,
            },
        )
        .await
    }

    /// Resolve a bare channel name against the acting agent's unit membership.
    async fn resolve_channel_key(
        &self,
        root: SessionId,
        member: &str,
        channel: &str,
    ) -> Result<String, CoreError> {
        let projection = self.read_projection(root).await?;
        projection
            .team
            .resolve_channel(member, channel)
            .map_err(|error| CoreError::Invalid(error.to_string()))
    }

    /// The roster as the acting agent sees it: its parent, its same-parent peers,
    /// and its direct reports. Agents in other units are not returned at all.
    pub(crate) async fn team_roster(&self, session: SessionId) -> Result<ScopedRoster, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        Ok(projection.team.scoped_roster(&handle))
    }

    /// The channels the acting agent may see: its home unit's and, when it leads
    /// one, its own unit's. Reserved announce channels are excluded.
    pub(crate) async fn team_channels(
        &self,
        session: SessionId,
    ) -> Result<Vec<ChannelInfo>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        Ok(projection
            .team
            .scoped_channels(&handle)
            .into_iter()
            .map(|(key, channel)| ChannelInfo {
                name: scope::channel_name(key).to_string(),
                unit: scope::channel_unit(key).unwrap_or(key).to_string(),
                members: channel.members.iter().cloned().collect(),
                messages: channel.log.len(),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;
    use std::sync::Arc;

    use hya_proto::{AgentName, ModelRef, OwnerRunId, Projection};
    use hya_provider::ProviderRouter;
    use hya_store::SessionStore;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    use super::*;
    use crate::AgentSpec;
    use crate::bus::EventBus;
    use crate::engine::{CreateSession, DirectMailPreAppendGate, SessionEngine};
    use crate::resident::ResidentSupervisor;

    async fn engine() -> SessionEngine {
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new());
        let runtime = crate::test_support::runtime(ToolRegistry::builtins());
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
        SessionEngine::new(store, router, runtime, permission, EventBus::default())
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
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: reviewer_1,
                    handle: "reviewer-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: reviewer_2,
                    handle: "reviewer-2".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();

        // Both members subscribe using their own child session (handle resolved
        // from the roster), and the MAIN agent posts to the channel.
        engine
            .channel_join(reviewer_1, "build".to_string(), None)
            .await
            .unwrap();
        engine
            .channel_join(reviewer_2, "build".to_string(), None)
            .await
            .unwrap();
        let reviewer_1_claim = engine
            .store()
            .try_claim_new(reviewer_1, OwnerRunId::new())
            .await
            .unwrap();
        let reviewer_2_claim = engine
            .store()
            .try_claim_new(reviewer_2, OwnerRunId::new())
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
        assert_eq!(body_of("main/reviewer-1"), vec!["ship it".to_string()]);
        assert_eq!(body_of("main/reviewer-2"), vec!["ship it".to_string()]);
        assert!(
            projection.team.roster.contains_key(MAIN_HANDLE),
            "main is on the roster"
        );

        // A fresh replay from the store reconstructs identical team state.
        let replayed = Projection::from_events(&engine.replay(root).await.unwrap());
        assert_eq!(replayed.team, projection.team);
        engine
            .store()
            .release_claim(&reviewer_1_claim)
            .await
            .unwrap();
        engine
            .store()
            .release_claim(&reviewer_2_claim)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn channel_mail_after_stop_excludes_resident_and_replays_for_active_subscriber() {
        let engine = engine().await;
        let root = root_team(&engine).await;
        let stopped = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("resident"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        let active = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("resident"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: stopped,
                    handle: "stopped-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("resident"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: active,
                    handle: "active-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("resident"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        engine
            .channel_join(stopped, "build".to_string(), None)
            .await
            .unwrap();
        engine
            .channel_join(active, "build".to_string(), None)
            .await
            .unwrap();

        let stopped_claim = engine
            .store()
            .try_claim_new(stopped, OwnerRunId::new())
            .await
            .unwrap();
        let active_claim = engine
            .store()
            .try_claim_new(active, OwnerRunId::new())
            .await
            .unwrap();
        engine
            .store()
            .finalize_resident_stop(&stopped_claim, root, "main/stopped-1")
            .await
            .unwrap();

        let body = "after stop".to_string();
        let receipt = engine
            .mail_send(
                root,
                MailEndpoint::Channel("build".to_string()),
                MailKind::Announcement,
                body.clone(),
            )
            .await
            .unwrap();
        assert_eq!(receipt.recipients, 1);

        let projection = engine.read_projection(root).await.unwrap();
        let replayed = Projection::from_events(&engine.replay(root).await.unwrap());
        assert_eq!(replayed, projection);
        let inbox_body = |handle: &str| {
            projection
                .team
                .inboxes
                .get(handle)
                .map(|inbox| inbox.iter().filter(|message| message.body == body).count())
                .unwrap_or_default()
        };
        assert_eq!(inbox_body("main/stopped-1"), 0);
        assert_eq!(inbox_body("main/active-1"), 1);

        let send_first = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("resident"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: send_first,
                    handle: "send-first-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("resident"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        engine
            .channel_join(send_first, "build".to_string(), None)
            .await
            .unwrap();
        let send_first_claim = engine
            .store()
            .try_claim_new(send_first, OwnerRunId::new())
            .await
            .unwrap();

        let second_body = "before send-first stop".to_string();
        let second_receipt = engine
            .mail_send(
                root,
                MailEndpoint::Channel("build".to_string()),
                MailKind::Announcement,
                second_body.clone(),
            )
            .await
            .unwrap();
        assert_eq!(second_receipt.recipients, 2);

        engine
            .store()
            .finalize_resident_stop(&send_first_claim, root, "main/send-first-1")
            .await
            .unwrap();
        let projection = engine.read_projection(root).await.unwrap();
        let send_first_inbox = projection.team.inboxes.get("main/send-first-1").unwrap();
        assert_eq!(
            send_first_inbox
                .iter()
                .map(|message| message.body.clone())
                .collect::<Vec<_>>(),
            vec![second_body.clone()]
        );
        assert_eq!(
            projection
                .team
                .roster
                .get("main/send-first-1")
                .unwrap()
                .resident_cursor,
            send_first_inbox.len() as u64
        );
        assert_eq!(
            projection
                .team
                .inboxes
                .get("main/active-1")
                .unwrap()
                .iter()
                .filter(|message| message.body == second_body)
                .count(),
            1
        );
        assert_eq!(
            projection
                .team
                .inboxes
                .get("main/stopped-1")
                .map(|inbox| {
                    inbox
                        .iter()
                        .filter(|message| message.body == second_body)
                        .count()
                })
                .unwrap_or_default(),
            0
        );

        let replay = engine.replay(root).await.unwrap();
        let replayed = Projection::from_events(&replay);
        assert_eq!(replayed, projection);
        let channel_events = replay
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MailSent {
                        to: MailEndpoint::Channel(channel),
                        body: event_body,
                        ..
                    } if channel == "main#build" && event_body == &body
                )
            })
            .count();
        assert_eq!(channel_events, 1);
        let second_channel_events = replay
            .iter()
            .filter(|envelope| {
                matches!(
                    &envelope.event,
                    Event::MailSent {
                        to: MailEndpoint::Channel(channel),
                        body: event_body,
                        ..
                    } if channel == "main#build" && event_body == &second_body
                )
            })
            .count();
        assert_eq!(second_channel_events, 1);
        engine.store().release_claim(&active_claim).await.unwrap();
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

    #[tokio::test]
    async fn direct_mail_to_transient_member_is_rejected_before_append() {
        let engine = engine().await;
        let root = root_team(&engine).await;
        engine.ensure_root_registered(root).await.unwrap();
        let child = engine
            .create(CreateSession {
                parent: Some(root),
                agent: AgentName::new("reviewer"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: child,
                    handle: "transient-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Transient,
                },
            )
            .await
            .unwrap();

        let before_len = engine.replay(root).await.unwrap().len();
        let result = engine
            .mail_send(
                root,
                MailEndpoint::Handle("transient-1".to_string()),
                MailKind::Message,
                "hi".to_string(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CoreError::Store(hya_store::StoreError::MailboxRejected(_)))
        ));

        let after_len = engine.replay(root).await.unwrap().len();
        assert_eq!(after_len, before_len);
        let projection = engine.read_projection(root).await.unwrap();
        assert!(!projection.team.inboxes.contains_key("transient-1"));
    }

    #[tokio::test]
    async fn direct_mail_to_unknown_handle_is_rejected_before_append() {
        let engine = engine().await;
        let root = root_team(&engine).await;
        engine.ensure_root_registered(root).await.unwrap();

        let before_len = engine.replay(root).await.unwrap().len();
        let result = engine
            .mail_send(
                root,
                MailEndpoint::Handle("missing-1".to_string()),
                MailKind::Message,
                "hi".to_string(),
            )
            .await;
        assert!(matches!(
            result,
            Err(CoreError::Store(hya_store::StoreError::MailboxRejected(_)))
        ));

        let after_len = engine.replay(root).await.unwrap().len();
        assert_eq!(after_len, before_len);
        let projection = engine.read_projection(root).await.unwrap();
        assert!(!projection.team.inboxes.contains_key("missing-1"));
    }

    #[tokio::test]
    async fn resident_stop_commits_before_stale_direct_send_rechecks_and_rejects() {
        let db_path =
            std::env::temp_dir().join(format!("hya-core-mailbox-stop-{}.db", SessionId::new()));
        let db_path = db_path.to_string_lossy().into_owned();
        let make_engine = |store: SessionStore, bus: EventBus| {
            let router = Arc::new(ProviderRouter::new());
            let runtime = crate::test_support::runtime(ToolRegistry::builtins());
            let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
            SessionEngine::new(store, router, runtime, permission, bus)
        };

        let primary = Arc::new(make_engine(
            SessionStore::connect(&db_path).await.unwrap(),
            EventBus::default(),
        ));
        let root = root_team(&primary).await;
        let agent = AgentSpec {
            name: AgentName::new("resident"),
            model: ModelRef::new("fake"),
            system_prompt: String::new(),
            workdir: PathBuf::from("."),
            reasoning: None,
        };
        let binding = primary.bind_runtime(&agent.workdir).unwrap();
        let resources = binding.agent_resource_policy(agent.name.as_str()).unwrap();
        let supervisor = ResidentSupervisor::start(primary.clone());
        let (child, handle) = supervisor
            .spawn_resident(
                root,
                agent,
                (binding, Arc::from([]), resources, None),
                String::new(),
                None,
                None,
            )
            .await
            .unwrap();

        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let sender = Arc::new(
            make_engine(
                SessionStore::connect(&db_path).await.unwrap(),
                EventBus::default(),
            )
            .with_direct_mail_pre_append_gate(DirectMailPreAppendGate::new(
                entered.clone(),
                release.clone(),
            )),
        );
        let sender_handle = handle.clone();
        let send = tokio::spawn(async move {
            sender
                .mail_send(
                    root,
                    MailEndpoint::Handle(sender_handle),
                    MailKind::Message,
                    "stale direct mail".to_string(),
                )
                .await
        });
        entered.notified().await;

        let stop_result = supervisor.stop_resident(root, &handle).await;
        assert!(
            stop_result.is_ok(),
            "resident stop must complete before stale send resumes: {stop_result:?}"
        );
        assert!(
            !primary
                .store()
                .active_actor_ids()
                .await
                .unwrap()
                .contains(&child)
        );

        release.notify_one();
        let send_result = send.await.unwrap();
        assert!(matches!(
            send_result,
            Err(CoreError::Store(hya_store::StoreError::MailboxRejected(_)))
        ));

        let replay = primary.replay(root).await.unwrap();
        assert!(
            !replay
                .iter()
                .any(|envelope| matches!(&envelope.event, Event::MailSent { .. }))
        );
    }

    // ---------------- hierarchy-scoped mailbox (task 08-07) ----------------

    /// One agent in a scoped test org: its session, its canonical path, and the
    /// claim that keeps it eligible to receive direct mail.
    struct Agent {
        session: SessionId,
        path: String,
        _claim: hya_store::ActorClaim,
    }

    /// Register a resident child of `parent`, mirroring what the spawn path does:
    /// an `AgentRegistered` carrying the real parent, plus the auto-join of that
    /// unit's reserved announce channel.
    async fn register_child(
        engine: &SessionEngine,
        root: SessionId,
        parent_session: SessionId,
        parent_path: &str,
        leaf: &str,
    ) -> Agent {
        let session = engine
            .create(CreateSession {
                parent: Some(parent_session),
                agent: AgentName::new("worker"),
                model: ModelRef::new("fake"),
                workdir: ".".to_string(),
            })
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::AgentRegistered {
                    session: root,
                    agent_session: session,
                    handle: leaf.to_string(),
                    parent: Some(parent_path.to_string()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        engine
            .emit_for_actor(
                None,
                root,
                Event::ChannelJoined {
                    session: root,
                    channel: scope::announce_channel_of(parent_path),
                    member: scope::join_path(parent_path, leaf),
                },
            )
            .await
            .unwrap();
        let claim = engine
            .store()
            .try_claim_new(session, OwnerRunId::new())
            .await
            .unwrap();
        Agent {
            session,
            path: scope::join_path(parent_path, leaf),
            _claim: claim,
        }
    }

    /// The standard two-unit org for the scope tests:
    ///
    /// ```text
    /// main
    /// ├── lead-1 ── worker-1, worker-2
    /// └── lead-2 ── worker-1      <- same leaf as lead-1's, a different agent
    /// ```
    struct Org {
        root: SessionId,
        lead_1: Agent,
        lead_2: Agent,
        worker_1: Agent,
        worker_2: Agent,
        other_worker: Agent,
    }

    async fn org(engine: &SessionEngine) -> Org {
        let root = root_team(engine).await;
        engine.ensure_root_registered(root).await.unwrap();
        let lead_1 = register_child(engine, root, root, "main", "lead-1").await;
        let lead_2 = register_child(engine, root, root, "main", "lead-2").await;
        let worker_1 = register_child(
            engine,
            root,
            lead_1.session,
            &lead_1.path.clone(),
            "worker-1",
        )
        .await;
        let worker_2 = register_child(
            engine,
            root,
            lead_1.session,
            &lead_1.path.clone(),
            "worker-2",
        )
        .await;
        let other_worker = register_child(
            engine,
            root,
            lead_2.session,
            &lead_2.path.clone(),
            "worker-1",
        )
        .await;
        Org {
            root,
            lead_1,
            lead_2,
            worker_1,
            worker_2,
            other_worker,
        }
    }

    /// Bodies delivered to `path`'s inbox, in order.
    async fn inbox(engine: &SessionEngine, root: SessionId, path: &str) -> Vec<String> {
        engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .inboxes
            .get(path)
            .map(|inbox| inbox.iter().map(|m| m.body.clone()).collect())
            .unwrap_or_default()
    }

    /// A sibling is reachable; an agent in another unit is not — and the refused
    /// send leaves NOTHING in the log (AC1).
    #[tokio::test]
    async fn sibling_is_reachable_and_cousin_is_refused_without_appending() {
        let engine = engine().await;
        let org = org(&engine).await;

        engine
            .mail_send(
                org.worker_1.session,
                MailEndpoint::Handle("worker-2".to_string()),
                MailKind::Message,
                "hello sibling".to_string(),
            )
            .await
            .expect("a same-parent sibling is in scope");
        assert_eq!(
            inbox(&engine, org.root, &org.worker_2.path).await,
            vec!["hello sibling".to_string()]
        );

        let before = engine.replay(org.root).await.unwrap().len();
        let refused = engine
            .mail_send(
                org.worker_1.session,
                MailEndpoint::Handle(org.other_worker.path.clone()),
                MailKind::Message,
                "hello cousin".to_string(),
            )
            .await;
        assert!(
            matches!(
                refused,
                Err(CoreError::Store(hya_store::StoreError::MailboxRejected(_)))
            ),
            "a cousin is out of scope: {refused:?}"
        );
        assert_eq!(
            engine.replay(org.root).await.unwrap().len(),
            before,
            "a refused send must append nothing"
        );
        assert!(
            inbox(&engine, org.root, &org.other_worker.path)
                .await
                .is_empty()
        );
    }

    /// Skip-level is closed in both directions: a worker cannot reach the root,
    /// and the root cannot reach a grandchild.
    #[tokio::test]
    async fn grandparent_and_grandchild_are_both_refused() {
        let engine = engine().await;
        let org = org(&engine).await;

        let up = engine
            .mail_send(
                org.worker_1.session,
                MailEndpoint::Handle("main".to_string()),
                MailKind::Message,
                "skipping levels".to_string(),
            )
            .await;
        assert!(
            up.is_err(),
            "a worker must not reach the team root directly"
        );

        let down = engine
            .mail_send(
                org.root,
                MailEndpoint::Handle(org.worker_1.path.clone()),
                MailKind::Message,
                "reaching past lead-1".to_string(),
            )
            .await;
        assert!(down.is_err(), "the root must not reach a grandchild");
    }

    /// A relative leaf and the full canonical path name the same agent (AC2).
    #[tokio::test]
    async fn relative_leaf_and_full_path_deliver_identically() {
        let engine = engine().await;
        let org = org(&engine).await;

        for address in ["worker-2", "main/lead-1/worker-2"] {
            engine
                .mail_send(
                    org.worker_1.session,
                    MailEndpoint::Handle(address.to_string()),
                    MailKind::Message,
                    format!("via {address}"),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            inbox(&engine, org.root, &org.worker_2.path).await,
            vec![
                "via worker-2".to_string(),
                "via main/lead-1/worker-2".to_string()
            ],
            "both spellings reach the same inbox"
        );
    }

    /// A relative leaf resolves inside the sender's own unit even when another
    /// unit holds an agent with the same leaf.
    #[tokio::test]
    async fn duplicate_leaf_across_units_resolves_to_the_senders_own() {
        let engine = engine().await;
        let org = org(&engine).await;

        engine
            .mail_send(
                org.worker_2.session,
                MailEndpoint::Handle("worker-1".to_string()),
                MailKind::Message,
                "mine".to_string(),
            )
            .await
            .unwrap();

        assert_eq!(
            inbox(&engine, org.root, &org.worker_1.path).await,
            vec!["mine".to_string()]
        );
        assert!(
            inbox(&engine, org.root, &org.other_worker.path)
                .await
                .is_empty(),
            "the other unit's worker-1 must not receive it"
        );
    }

    /// Announce reaches DIRECT reports and stops (AC6). A grandchild hears it
    /// only after the intermediate leader announces in turn.
    #[tokio::test]
    async fn announce_reaches_direct_reports_only() {
        let engine = engine().await;
        let org = org(&engine).await;

        let receipt = engine
            .mail_announce(org.root, "all hands".to_string())
            .await
            .unwrap();
        assert_eq!(receipt.recipients, 2, "the root leads exactly two agents");

        assert_eq!(
            inbox(&engine, org.root, &org.lead_1.path).await,
            vec!["all hands".to_string()]
        );
        assert_eq!(
            inbox(&engine, org.root, &org.lead_2.path).await,
            vec!["all hands".to_string()]
        );
        for grandchild in [&org.worker_1, &org.worker_2, &org.other_worker] {
            assert!(
                inbox(&engine, org.root, &grandchild.path).await.is_empty(),
                "{} must NOT hear the root's announcement",
                grandchild.path
            );
        }

        // The relay: lead-1 passes it down, and only ITS unit hears it.
        engine
            .mail_announce(org.lead_1.session, "all hands".to_string())
            .await
            .unwrap();
        assert_eq!(
            inbox(&engine, org.root, &org.worker_1.path).await,
            vec!["all hands".to_string()]
        );
        assert!(
            inbox(&engine, org.root, &org.other_worker.path)
                .await
                .is_empty(),
            "lead-2's unit is not reached by lead-1's announcement"
        );
    }

    /// An agent that leads nobody has nothing to announce to.
    #[tokio::test]
    async fn announce_from_a_leaf_agent_is_refused() {
        let engine = engine().await;
        let org = org(&engine).await;
        let result = engine
            .mail_announce(org.worker_1.session, "listen up".to_string())
            .await;
        assert!(result.is_err(), "a leaf agent leads no one: {result:?}");
    }

    /// R5 leaves a working path: relaying through the common ancestor delivers
    /// a message the direct send refuses (AC9).
    #[tokio::test]
    async fn cross_unit_relay_through_the_common_ancestor_arrives() {
        let engine = engine().await;
        let org = org(&engine).await;

        // worker-1 -> lead-1 -> main -> lead-2 -> lead-2's worker-1.
        let hops = [
            (org.worker_1.session, "lead-1"),
            (org.lead_1.session, "main"),
            (org.root, "lead-2"),
            (org.lead_2.session, "worker-1"),
        ];
        for (from, to) in hops {
            engine
                .mail_send(
                    from,
                    MailEndpoint::Handle(to.to_string()),
                    MailKind::Message,
                    "relayed payload".to_string(),
                )
                .await
                .unwrap_or_else(|e| panic!("hop to {to} must be in scope: {e:?}"));
        }

        assert_eq!(
            inbox(&engine, org.root, &org.other_worker.path).await,
            vec!["relayed payload".to_string()],
            "the message crossed units, one in-scope hop at a time"
        );
    }

    /// `#name` is the unit you lead; `#^name` is your parent's unit, and only a
    /// leader may use it (AC5).
    #[tokio::test]
    async fn caret_channel_is_leader_only() {
        let engine = engine().await;
        let org = org(&engine).await;

        engine
            .channel_join(org.lead_1.session, "build".to_string(), None)
            .await
            .unwrap();
        engine
            .channel_join(org.lead_1.session, "^build".to_string(), None)
            .await
            .unwrap();

        let channels = engine
            .read_projection(org.root)
            .await
            .unwrap()
            .team
            .channels;
        assert!(
            channels
                .get("main/lead-1#build")
                .is_some_and(|c| c.members.contains("main/lead-1")),
            "bare name joined the unit lead-1 LEADS"
        );
        assert!(
            channels
                .get("main#build")
                .is_some_and(|c| c.members.contains("main/lead-1")),
            "caret joined lead-1's HOME unit"
        );

        let refused = engine
            .channel_join(org.worker_1.session, "^build".to_string(), None)
            .await;
        assert!(
            matches!(refused, Err(CoreError::Invalid(_))),
            "a leaf agent has no second unit to disambiguate: {refused:?}"
        );
    }

    /// Two units each own a `#build`; a post in one never reaches the other (AC4).
    #[tokio::test]
    async fn same_channel_name_in_two_units_stays_separate() {
        let engine = engine().await;
        let org = org(&engine).await;

        engine
            .channel_join(org.worker_1.session, "build".to_string(), None)
            .await
            .unwrap();
        engine
            .channel_join(org.other_worker.session, "build".to_string(), None)
            .await
            .unwrap();

        engine
            .mail_send(
                org.lead_1.session,
                MailEndpoint::Channel("build".to_string()),
                MailKind::Message,
                "unit one".to_string(),
            )
            .await
            .unwrap();

        assert_eq!(
            inbox(&engine, org.root, &org.worker_1.path).await,
            vec!["unit one".to_string()]
        );
        assert!(
            inbox(&engine, org.root, &org.other_worker.path)
                .await
                .is_empty(),
            "the other unit's #build is a different channel"
        );
    }

    /// The roster is bucketed by relation and stops at the unit boundary (AC7),
    /// and channel listings hide the reserved announce channel.
    #[tokio::test]
    async fn scoped_roster_and_channels_stop_at_the_unit_boundary() {
        let engine = engine().await;
        let org = org(&engine).await;

        let roster = engine.team_roster(org.worker_1.session).await.unwrap();
        assert_eq!(roster.self_path, "main/lead-1/worker-1");
        assert_eq!(
            roster.parent.as_ref().map(|e| e.handle.as_str()),
            Some("main/lead-1")
        );
        assert_eq!(
            roster
                .peers
                .iter()
                .map(|e| e.handle.as_str())
                .collect::<Vec<_>>(),
            vec!["main/lead-1/worker-2"]
        );
        assert!(roster.reports.is_empty());
        assert_eq!(
            roster.entries().len(),
            2,
            "six agents exist; a worker sees two"
        );

        // Every agent auto-joined an announce channel, yet none is listed.
        let channels = engine.team_channels(org.worker_1.session).await.unwrap();
        assert!(
            channels.is_empty(),
            "the reserved announce channel must not appear: {channels:?}"
        );

        engine
            .channel_join(org.worker_1.session, "build".to_string(), None)
            .await
            .unwrap();
        let channels = engine.team_channels(org.worker_1.session).await.unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "build");
        assert_eq!(
            channels[0].unit, "main/lead-1",
            "the listing says which unit owns it"
        );
    }
}
