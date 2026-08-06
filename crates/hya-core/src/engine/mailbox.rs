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
                agent_type,
                mode: SubagentMode::Transient,
            },
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

    /// Subscribe the acting agent's handle to `channel`.
    pub(crate) async fn channel_join(
        &self,
        session: SessionId,
        channel: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
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

    /// Unsubscribe the acting agent's handle from `channel`.
    pub(crate) async fn channel_leave(
        &self,
        session: SessionId,
        channel: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<(), CoreError> {
        let root = self.team_root(session).await?;
        let member = self.resolve_handle(root, session).await?;
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
        assert_eq!(body_of("reviewer-1"), vec!["ship it".to_string()]);
        assert_eq!(body_of("reviewer-2"), vec!["ship it".to_string()]);
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
            .finalize_resident_stop(&stopped_claim, root, "stopped-1")
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
        assert_eq!(inbox_body("stopped-1"), 0);
        assert_eq!(inbox_body("active-1"), 1);

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
            .finalize_resident_stop(&send_first_claim, root, "send-first-1")
            .await
            .unwrap();
        let projection = engine.read_projection(root).await.unwrap();
        let send_first_inbox = projection.team.inboxes.get("send-first-1").unwrap();
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
                .get("send-first-1")
                .unwrap()
                .resident_cursor,
            send_first_inbox.len() as u64
        );
        assert_eq!(
            projection
                .team
                .inboxes
                .get("active-1")
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
                .get("stopped-1")
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
                    } if channel == "build" && event_body == &body
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
                    } if channel == "build" && event_body == &second_body
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
}
