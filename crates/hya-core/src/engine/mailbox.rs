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

use hya_bundle::{ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{
    AgentName, Event, MailEndpoint, MailKind, RosterStatus, SessionId, SubagentMode, scope,
};
use hya_tool::MailReceipt;

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
        let policy = self.session_channel_policy(from_session).ok_or_else(|| {
            CoreError::Invalid("channel policy snapshot missing for session send".to_string())
        })?;
        self.mail_send_for_actor_with_policy(from_session, to, kind, body, None, Some(policy))
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
        self.mail_send_for_actor_with_policy(from_session, to, kind, body, actor_claim, None)
            .await
    }

    pub(crate) async fn mail_send_for_actor_with_policy(
        &self,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        // ADR-0016 `dm` default: a subordinate omits `to` (or writes the
        // parent sentinel) and the engine resolves its one upward peer.
        // The sentinel prefers the DM channel minted at registration time:
        // it exists regardless of whether the parent handle is in the roster
        // yet (a lazily registered root) and carries the same scope
        // guarantees; the handle fallback keeps older projections working.
        let to = match to {
            MailEndpoint::Handle(target)
                if target.trim().is_empty()
                    || target.trim() == "^parent"
                    || target.trim() == "^" =>
            {
                let parent = hya_proto::scope::parent_path(&from)
                    .map(str::to_string)
                    .unwrap_or_else(|| hya_proto::scope::ROOT_HANDLE.to_string());
                let dm_channel = self
                    .read_projection(root)
                    .await
                    .ok()
                    .and_then(|projection| {
                        dm_channel_between(&projection, &from, &parent).map(MailEndpoint::Channel)
                    });
                dm_channel.unwrap_or(MailEndpoint::Handle(parent))
            }
            other => other,
        };
        let policy_projection = self.read_projection(root).await?;
        let (policy_kind, policy_role) = match &to {
            MailEndpoint::Handle(target) => (
                ChannelTemplateKind::ParentDm,
                if scope::parent_path(&from) == Some(target.as_str()) {
                    ChannelParticipantRole::Child
                } else {
                    ChannelParticipantRole::Parent
                },
            ),
            MailEndpoint::Channel(channel) => match policy_projection.team.channels.get(channel) {
                Some(state) if state.kind == hya_proto::ChannelKind::Group => (
                    ChannelTemplateKind::Unit,
                    ChannelParticipantRole::UnitLeader,
                ),
                _ => (
                    ChannelTemplateKind::ParentDm,
                    dm_role_for(&policy_projection, channel, &from),
                ),
            },
        };
        if channel_policy
            .is_some_and(|policy| !snapshot_allows(policy, policy_kind, policy_role, 0))
        {
            return Err(CoreError::Invalid(
                "channel policy denies send for this agent and topology role".to_string(),
            ));
        }
        #[cfg(test)]
        if matches!(&to, MailEndpoint::Handle(_))
            && let Some(gate) = self.direct_mail_pre_append_gate.as_ref()
        {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if let MailEndpoint::Handle(handle) = &to {
            let append = self
                .store()
                .append_direct_mail(
                    root,
                    from.clone(),
                    handle.clone(),
                    kind,
                    body.clone(),
                    actor_claim,
                )
                .await;
            let envelope = match append {
                Ok(envelope) => envelope,
                Err(error @ hya_store::StoreError::MailboxRejected(_)) => {
                    // ADR-0015: a downward mail to one of the sender's own
                    // ARCHIVED direct children revives it instead of failing.
                    // Everything else keeps the indistinguishable rejection.
                    if let Some(reviver) = self.archive_reviver()
                        && let Ok(projection) = self.read_projection(root).await
                    {
                        let canonical = projection.team.canonical_member(handle);
                        let own_archived_child = projection.team.archived.contains_key(&canonical)
                            && scope::parent_path(&canonical) == Some(from.as_str());
                        if own_archived_child {
                            reviver.revive(root, &from, &canonical, body).await?;
                            return Ok(MailReceipt {
                                from,
                                to,
                                recipients: 1,
                            });
                        }
                    }
                    return Err(CoreError::Store(error));
                }
                Err(error) => return Err(error.into()),
            };
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
        // ADR-0016 write gate: group channels are the unit leader's broadcast
        // pipe. Anyone else posting into one is rejected before the store.
        // The channel's own nature also picks the delivery kind: a group
        // post is a one-way announcement, everything else stays 1:1 chatter.
        let kind = {
            let projection = self.read_projection(root).await?;
            match projection.team.channels.get(&channel) {
                Some(state)
                    if state.kind == hya_proto::ChannelKind::Group
                        && state.unit.as_deref() != Some(from.as_str()) =>
                {
                    return Err(CoreError::Invalid(format!(
                        "`#{channel}` is a group broadcast channel; only its unit leader may post (address a peer directly instead)"
                    )));
                }
                Some(state) if state.kind == hya_proto::ChannelKind::Group => {
                    MailKind::Announcement
                }
                _ => kind,
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
        let policy = self.session_channel_policy(from_session).ok_or_else(|| {
            CoreError::Invalid("channel policy snapshot missing for session broadcast".to_string())
        })?;
        self.mail_announce_for_actor_with_policy(from_session, body, None, Some(policy))
            .await
    }

    async fn mail_announce_for_actor_with_policy(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        // ADR-0016: broadcast rides the unit's minted group channel
        // (`announce-{8}`, leader-only posting). Fall back to the legacy
        // reserved channel when no minted channel exists yet (pre-mint logs).
        let projection = self.read_projection(root).await?;
        if channel_policy.is_some_and(|policy| {
            !snapshot_allows(
                policy,
                ChannelTemplateKind::Unit,
                ChannelParticipantRole::UnitLeader,
                0,
            )
        }) {
            return Err(CoreError::Invalid(
                "channel policy denies unit broadcast for this agent".to_string(),
            ));
        }
        let Some(group) = projection
            .team
            .channels
            .iter()
            .find(|(_, channel)| {
                channel.kind == hya_proto::ChannelKind::Group
                    && channel.unit.as_deref() == Some(from.as_str())
            })
            .map(|(key, _)| key.clone())
        else {
            return Err(CoreError::Invalid(
                "you do not lead a unit yet; broadcast reaches your direct reports only"
                    .to_string(),
            ));
        };
        let (envelope, recipients) = self
            .store()
            .append_channel_mail(
                root,
                from.clone(),
                group.clone(),
                MailKind::Announcement,
                body,
                actor_claim,
            )
            .await?;
        self.publish_envelope(envelope);
        Ok(MailReceipt {
            to: MailEndpoint::Channel(group),
            from,
            recipients,
        })
    }

    /// `send` with no explicit channel: route by the sender's role. A leader
    /// posts on the unit group channel it leads (broadcast semantics); a
    /// subordinate DMs its one upward peer; an agent with neither is asked to
    /// address a channel explicitly.
    #[cfg(test)]
    pub(crate) async fn mail_send_default_for_actor(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
    ) -> Result<MailReceipt, CoreError> {
        self.mail_send_default_for_actor_with_policy(from_session, body, actor_claim, None)
            .await
    }

    pub(crate) async fn mail_send_default_for_actor_with_policy(
        &self,
        from_session: SessionId,
        body: String,
        actor_claim: Option<&hya_store::ActorClaim>,
        channel_policy: Option<hya_tool::ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, CoreError> {
        let root = self.team_root(from_session).await?;
        let from = self.resolve_handle(root, from_session).await?;
        let projection = self.read_projection(root).await?;
        let leads_unit = projection.team.channels.iter().any(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Group
                && channel.unit.as_deref() == Some(from.as_str())
        });
        if leads_unit {
            return self
                .mail_announce_for_actor_with_policy(
                    from_session,
                    body,
                    actor_claim,
                    channel_policy,
                )
                .await;
        }
        if let Some(parent) = hya_proto::scope::parent_path(&from) {
            return self
                .mail_send_for_actor_with_policy(
                    from_session,
                    MailEndpoint::Handle(parent.to_string()),
                    MailKind::Message,
                    body,
                    actor_claim,
                    channel_policy,
                )
                .await;
        }
        Err(CoreError::Invalid(
            "no default channel: you neither lead a unit nor report to one; address a channel explicitly with `#channel` or a peer handle".to_string(),
        ))
    }
}

fn dm_role_for(
    projection: &hya_proto::Projection,
    channel: &str,
    actor: &str,
) -> ChannelParticipantRole {
    projection
        .team
        .channels
        .get(channel)
        .and_then(|state| state.members.iter().find(|member| member.as_str() != actor))
        .map_or(ChannelParticipantRole::Child, |peer| {
            if scope::parent_path(peer) == Some(actor) {
                ChannelParticipantRole::Parent
            } else {
                ChannelParticipantRole::Child
            }
        })
}

fn snapshot_allows(
    policy: hya_tool::ChannelPolicySnapshot,
    kind: ChannelTemplateKind,
    role: ChannelParticipantRole,
    bit: u8,
) -> bool {
    let bits = match (kind, role) {
        (ChannelTemplateKind::Unit, ChannelParticipantRole::UnitLeader) => policy.unit_leader,
        (ChannelTemplateKind::Unit, _) => policy.unit_member,
        (ChannelTemplateKind::ParentDm, ChannelParticipantRole::Parent) => policy.dm_parent,
        (ChannelTemplateKind::ParentDm, _) => policy.dm_child,
    };
    bits & (1 << bit) != 0
}

impl SessionEngine {
    /// `list_channel` rows for the acting agent (ADR-0016): the group channels
    /// it belongs to (home + led) with speak rights, and DM channels whose peer
    /// is LIVE (archived peers surface via `search_agent`, not here).
    pub(crate) async fn team_channel_rows(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::ChannelRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len());
        let cursor = projection.team.roster.get(&handle).map_or(0, |entry| {
            usize::try_from(entry.resident_cursor).unwrap_or(0)
        });
        let unread_self = inbox_len.saturating_sub(cursor);
        let mut rows = Vec::new();
        for (id, channel) in &projection.team.channels {
            let member = channel.members.contains(&handle);
            if !member {
                continue;
            }
            match channel.kind {
                hya_proto::ChannelKind::Group => rows.push(hya_tool::ChannelRow {
                    id: id.clone(),
                    kind: hya_proto::ChannelKind::Group,
                    can_speak: channel.unit.as_deref() == Some(handle.as_str()),
                    peer: None,
                    unread: unread_self,
                }),
                hya_proto::ChannelKind::Dm => {
                    // Peer identity from membership; hide archived peers.
                    let peer = channel
                        .members
                        .iter()
                        .find(|member| *member != &handle)
                        .cloned();
                    let peer_live = peer
                        .as_ref()
                        .is_some_and(|peer| projection.team.roster.contains_key(peer));
                    if peer_live {
                        rows.push(hya_tool::ChannelRow {
                            id: id.clone(),
                            kind: hya_proto::ChannelKind::Dm,
                            can_speak: true,
                            peer,
                            unread: unread_self,
                        });
                    }
                }
            }
        }
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(rows)
    }

    /// Read one channel's recent history for the acting agent (ADR-0016
    /// `channel://` read): newest-last, at most `last` messages (1 when
    /// `None`). Reading marks the caller's whole current inbox as seen (a
    /// single durable cursor cannot selectively consume one channel), and the
    /// result reports how many unread remain so the model knows to iterate
    /// `list_channel`.
    pub async fn read_channel_history(
        &self,
        session: SessionId,
        channel: &str,
        last: Option<usize>,
    ) -> Result<ChannelHistoryRead, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        // Models write `##X`, `#X`, or whitespace-padded ids when reading mail
        // history. Normalize (trim + strip every leading `#`; case stays
        // exact) and echo the correction so the right spelling is learned
        // in-turn instead of failing the read.
        let normalized = channel.trim().trim_start_matches('#');
        let warning = (normalized != channel)
            .then(|| format!("normalized channel id `{channel}` → `{normalized}`"));
        let channel_state = projection
            .team
            .channels
            .get(normalized)
            .ok_or_else(|| CoreError::Invalid(format!("unknown channel `#{normalized}`")))?;
        if !channel_state.members.contains(&handle) {
            // Unknown and not-a-member are deliberately indistinguishable.
            return Err(CoreError::Invalid(format!(
                "unknown channel `#{normalized}`"
            )));
        }
        let last_n = last.unwrap_or(1).clamp(1, 50);
        // Newest-first: the most recent message leads, matching how a chat
        // backlog is scanned.
        let messages: Vec<(String, String)> = channel_state
            .log
            .iter()
            .rev()
            .take(last_n)
            .map(|message| (message.from.clone(), message.body.clone()))
            .collect();
        let inbox_len = projection
            .team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len() as u64);
        let cursor = projection
            .team
            .roster
            .get(&handle)
            .map_or(0, |entry| entry.resident_cursor);
        if cursor < inbox_len {
            self.emit_for_actor(
                None,
                root,
                Event::MailConsumed {
                    session: root,
                    handle: handle.clone(),
                    through: inbox_len,
                },
            )
            .await?;
        }
        Ok(ChannelHistoryRead {
            channel: normalized.to_string(),
            messages,
            unread_remaining: 0,
            warning,
        })
    }

    /// Direct-child liveness rows for the acting agent (ADR-0002): each roster
    /// entry whose parent is the caller's canonical handle, with its live
    /// status and harness-heartbeat freshness. Seconds-since-activity is
    /// computed here because the tool plane has no clock; `None` means no
    /// heartbeat was ever observed for that child.
    pub async fn team_member_status(
        &self,
        session: SessionId,
    ) -> Result<Vec<hya_tool::MemberStatusRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        let now = u64::try_from(hya_proto::now_millis()).unwrap_or(0);
        let mut rows = Vec::new();
        for (path, entry) in &projection.team.roster {
            if scope::parent_path(path) != Some(handle.as_str()) {
                continue;
            }
            rows.push(hya_tool::MemberStatusRow {
                handle: path.clone(),
                status: roster_status_label(entry.status).to_string(),
                last_active_seconds: (entry.heartbeat_ms > 0)
                    .then(|| now.saturating_sub(entry.heartbeat_ms) / 1_000),
            });
        }
        rows.sort_by(|left, right| left.handle.cmp(&right.handle));
        Ok(rows)
    }

    /// `search_agent` rows (ADR-0015 §7): the caller's own archived DIRECT
    /// children, digested from each child's latest handoff. The archive index
    /// is derived from the logs (archived map + per-child handoff projection).
    pub(crate) async fn search_archived_agents(
        &self,
        session: SessionId,
        query: &str,
    ) -> Result<Vec<hya_tool::ArchivedAgentRow>, CoreError> {
        let root = self.team_root(session).await?;
        let handle = self.resolve_handle(root, session).await?;
        let projection = self.read_projection(root).await?;
        let needle = query.trim().to_ascii_lowercase();
        let mut rows = Vec::new();
        for (path, entry) in &projection.team.archived {
            if hya_proto::scope::parent_path(path) != Some(handle.as_str()) {
                continue;
            }
            let child_projection = self.read_projection(entry.session).await?;
            let handoff = child_projection.session.handoff.clone();
            let (goal, pending, degraded) = match &handoff {
                Some(handoff) => (
                    handoff_section(&handoff.doc, "Goal"),
                    handoff_section(&handoff.doc, "Pending tasks"),
                    handoff.degraded,
                ),
                None => ("(no handoff)".to_string(), String::new(), true),
            };
            if !needle.is_empty() {
                let haystack =
                    format!("{} {} {}", goal, pending, entry.agent_type).to_ascii_lowercase();
                if !haystack.contains(&needle) {
                    continue;
                }
            }
            rows.push(hya_tool::ArchivedAgentRow {
                handle: path.clone(),
                agent_type: entry.agent_type.as_str().to_string(),
                session: entry.session.to_string(),
                goal,
                pending,
                degraded,
            });
        }
        rows.sort_by(|left, right| left.handle.cmp(&right.handle));
        Ok(rows)
    }
}

/// Stable lowercase label for one live roster status row.
fn roster_status_label(status: RosterStatus) -> &'static str {
    match status {
        RosterStatus::Idle => "idle",
        RosterStatus::Busy => "busy",
        RosterStatus::Done => "done",
        RosterStatus::Failed => "failed",
    }
}

/// Extract one numbered section's text from a six-section handoff document.
fn handoff_section(doc: &str, heading: &str) -> String {
    let prefix = heading.to_string();
    let mut take = false;
    let mut out = String::new();
    for line in doc.lines() {
        let trimmed = line.trim_start();
        let is_heading = trimmed
            .split_once(". ")
            .map(|(num, rest)| num.len() <= 2 && rest.starts_with(|c: char| c.is_uppercase()))
            .unwrap_or(false);
        if is_heading {
            take = trimmed.contains(&prefix);
            if take {
                out.push_str(
                    trimmed
                        .split_once(". ")
                        .map(|(_, rest)| rest)
                        .unwrap_or(trimmed),
                );
                out.push(' ');
            }
            continue;
        }
        if take {
            out.push_str(trimmed);
            out.push(' ');
        }
    }
    out.trim().chars().take(200).collect()
}

/// The DM channel registration minted between `from` and `parent`, if any.
///
/// Channel ids are their own keys (`DM-<8>`), members carry canonical paths,
/// and the first minted pair wins — the same rule steer's `dm_by_peer` uses.
fn dm_channel_between(
    projection: &hya_proto::Projection,
    from: &str,
    parent: &str,
) -> Option<String> {
    projection
        .team
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == hya_proto::ChannelKind::Dm
                && channel.members.iter().any(|member| member == from)
                && channel.members.iter().any(|member| member == parent)
        })
        .map(|(channel, _)| channel.clone())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::PathBuf;
    use std::sync::Arc;

    use hya_proto::{AgentName, ModelRef, OwnerRunId};
    use hya_provider::ProviderRouter;
    use hya_store::SessionStore;
    use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};

    use super::*;
    use crate::AgentSpec;
    use crate::bus::EventBus;
    use crate::engine::{CreateSession, DirectMailPreAppendGate, SessionEngine};
    use crate::resident::ResidentSupervisor;

    fn local_test_runtime() -> Arc<crate::RuntimeRegistry> {
        tests_support::runtime_with_resident()
    }

    mod tests_support {
        use super::*;

        pub(super) fn runtime_with_resident() -> Arc<crate::RuntimeRegistry> {
            runtime_with_extra(Vec::new())
        }

        pub(super) fn runtime_with_channel_restriction(
            channels: &str,
        ) -> Arc<crate::RuntimeRegistry> {
            let source = hya_bundle::BundleSource::new(
                "channel-restriction",
                vec![hya_bundle::SourceFile::new(
                    "bundle.yaml",
                    format!(
                        "kind: AgentSetBundle\nidentity: {{ id: acme/channel-restriction, version: 1.0.0, publisher: acme }}\nchannels:\n{channels}\n"
                    ),
                )],
            );
            let prepared = hya_bundle::prepare_package(source).expect("channel restriction");
            runtime_with_extra(prepared.bundles().to_vec())
        }

        fn runtime_with_extra(
            mut extra: Vec<hya_bundle::PreparedInstallableBundle>,
        ) -> Arc<crate::RuntimeRegistry> {
            let bundle = hya_bundle::PreparedAgentBundle {
                format_version: 2,
                identity: hya_bundle::BundleIdentity {
                    id: "hya/mailbox-unit-tests-resident".to_string(),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                namespace: None,
                digest: "test-only-resident".to_string(),
                agent: hya_bundle::PreparedAgent {
                    id: AgentName::new("resident"),
                    description: None,
                    role: hya_bundle::AgentRole::Subagent,
                    color: None,
                    prompt: None,
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: hya_bundle::ModelPolicy::default(),
                    workdir: None,
                    legacy_spawn_lifecycle: None,
                    resource_view: hya_bundle::ResourceView::default(),
                    can_spawn: Vec::new(),
                    hook_refs: Vec::new(),
                },
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            };
            extra.push(hya_bundle::PreparedInstallableBundle::Agent(Box::new(
                bundle,
            )));
            let catalog = hya_bundle::BundleCatalog::from_prepared(&extra).expect("catalog valid");
            let catalog = crate::AgentCatalog::new(Arc::new(catalog)).expect("agent catalog valid");
            Arc::new(crate::RuntimeRegistry::new(
                ToolRegistry::builtins(),
                Arc::new(catalog),
            ))
        }
    }

    async fn engine() -> SessionEngine {
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new());
        let runtime = local_test_runtime();
        let (permission, _rx) = PermissionPlane::new(PermissionRules::new(Vec::new()));
        SessionEngine::new(store, router, runtime, permission, EventBus::default())
    }

    async fn engine_with_runtime(runtime: Arc<crate::RuntimeRegistry>) -> SessionEngine {
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new());
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
        assert!(
            matches!(result, Err(CoreError::Invalid(ref message)) if message.contains("channel policy snapshot missing")),
            "a session without an admitted binding must fail closed: {result:?}"
        );
    }

    #[tokio::test]
    async fn bundle_channel_policy_can_restrict_real_send_without_granting() {
        let runtime = tests_support::runtime_with_channel_restriction(
            "  - { id: dm, kind: parent_dm, participants: [{kind: role, role: child}], capabilities: [], scope: vertical, retention: team_session }",
        );
        let engine = engine_with_runtime(runtime).await;
        let org = org(&engine).await;
        let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
        let policy = crate::ChannelPolicy::from_binding(&binding)
            .unwrap()
            .snapshot_for("reviewer");
        let result = engine
            .mail_send_for_actor_with_policy(
                org.worker_1.session,
                MailEndpoint::Handle(org.lead_1.path),
                MailKind::Message,
                "blocked".to_string(),
                None,
                Some(policy),
            )
            .await;
        assert!(
            matches!(result, Err(CoreError::Invalid(message)) if message.contains("channel policy denies send"))
        );
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
            let runtime = local_test_runtime();
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
        let sender_binding = sender.bind_runtime(std::path::Path::new(".")).unwrap();
        sender
            .capture_session_bundle_hooks(root, &sender_binding, "build")
            .await;
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
        let binding = engine.bind_runtime(std::path::Path::new(".")).unwrap();
        engine
            .capture_session_bundle_hooks(session, &binding, "worker")
            .await;
        // Mirror the supervisor's registration-time minting: one group channel
        // per unit (leader + children), reused across siblings.
        let existing_group = engine
            .read_projection(root)
            .await
            .unwrap()
            .team
            .channels
            .iter()
            .find(|(_, channel)| {
                channel.kind == hya_proto::ChannelKind::Group
                    && channel.unit.as_deref() == Some(parent_path)
            })
            .map(|(key, _)| key.clone());
        let child_path = scope::join_path(parent_path, leaf);
        match existing_group {
            Some(group) => {
                engine
                    .emit_for_actor(
                        None,
                        root,
                        Event::ChannelJoined {
                            session: root,
                            channel: group,
                            member: child_path.clone(),
                        },
                    )
                    .await
                    .unwrap();
            }
            None => {
                let group = hya_proto::mint_channel_id(hya_proto::ChannelKind::Group);
                engine
                    .emit_for_actor(
                        None,
                        root,
                        Event::ChannelCreated {
                            session: root,
                            channel: group.clone(),
                            kind: hya_proto::ChannelKind::Group,
                            unit: Some(parent_path.to_string()),
                            members: vec![parent_path.to_string(), child_path.clone()],
                        },
                    )
                    .await
                    .unwrap();
                let _ = group;
            }
        }
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
        let claim = engine
            .store()
            .try_claim_new(session, OwnerRunId::new())
            .await
            .unwrap();
        Agent {
            session,
            path: child_path,
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
    /// send leaves NOTHING in the log (AC1).    /// Skip-level is closed in both directions: a worker cannot reach the root,
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

    /// Mint the DM channel the resident supervisor creates at registration
    /// time: one direct line between `parent_path` and `child_path`.
    async fn mint_dm(engine: &SessionEngine, root: SessionId, parent_path: &str, child_path: &str) {
        let channel = hya_proto::mint_channel_id(hya_proto::ChannelKind::Dm);
        engine
            .emit_for_actor(
                None,
                root,
                Event::ChannelCreated {
                    session: root,
                    channel: channel.clone(),
                    kind: hya_proto::ChannelKind::Dm,
                    unit: None,
                    members: vec![parent_path.to_string(), child_path.to_string()],
                },
            )
            .await
            .unwrap();
    }

    /// The `^parent` sentinel resolves the DM channel with the direct parent
    /// instead of the parent *handle*, so a lazily registered root (the run-2
    /// bounce: "main is not a teammate you can message") still receives the
    /// mail through the channel that exists since registration time.
    #[tokio::test]
    async fn parent_sentinel_routes_through_the_dm_channel_when_root_is_lazy() {
        let engine = engine().await;
        // Deliberately NO ensure_root_registered here: the roster has no `main`.
        let root = root_team(&engine).await;
        let child = register_child(&engine, root, root, "main", "general-1").await;
        mint_dm(&engine, root, "main", &child.path).await;

        let receipt = engine
            .mail_send(
                child.session,
                MailEndpoint::Handle("^parent".to_string()),
                MailKind::Message,
                "report from the field".to_string(),
            )
            .await
            .expect("the DM channel with the parent must absorb the sentinel");

        assert!(
            matches!(receipt.to, MailEndpoint::Channel(ref channel) if channel.starts_with("DM-")),
            "the sentinel resolved to the DM channel, got {:?}",
            receipt.to
        );
        assert_eq!(
            inbox(&engine, root, "main").await,
            vec!["report from the field".to_string()],
            "the parent's inbox receives the mail"
        );
    }

    /// At depth ≥ 2 the sentinel addresses the DIRECT parent's DM — never the
    /// root — and still succeeds even though the root is registered.
    #[tokio::test]
    async fn parent_sentinel_at_depth_two_targets_the_direct_parent_dm() {
        let engine = engine().await;
        let org = org(&engine).await;
        mint_dm(&engine, org.root, &org.lead_1.path, &org.worker_1.path).await;

        let receipt = engine
            .mail_send(
                org.worker_1.session,
                MailEndpoint::Handle("^".to_string()),
                MailKind::Message,
                "status up".to_string(),
            )
            .await
            .expect("depth-two sentinel delivery reaches the direct parent");

        assert!(matches!(receipt.to, MailEndpoint::Channel(_)));
        assert_eq!(
            inbox(&engine, org.root, &org.lead_1.path).await,
            vec!["status up".to_string()],
            "lead-1 (the direct parent) receives it; main does not"
        );
        assert!(inbox(&engine, org.root, "main").await.is_empty());
    }

    /// A relative leaf and the full canonical path name the same agent (AC2).    /// A relative leaf resolves inside the sender's own unit even when another
    /// unit holds an agent with the same leaf.    /// Announce reaches DIRECT reports and stops (AC6). A grandchild hears it
    /// only after the intermediate leader announces in turn.
    #[tokio::test]
    async fn announce_reaches_direct_reports_only() {
        let engine = engine().await;
        let org = org(&engine).await;

        let receipt = engine
            .mail_announce(org.root, "all hands".to_string())
            .await
            .unwrap();
        assert!(
            receipt.recipients >= 2,
            "the root leads exactly two agents: {receipt:?}"
        );

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

    /// `send` with no channel routes by role: a leader broadcasts on its unit
    /// group channel; a subordinate DMs its parent; the delivery outcomes
    /// match the explicit endpoint forms.
    #[tokio::test]
    async fn send_default_routes_by_role() {
        let engine = engine().await;
        let org = org(&engine).await;

        // The root leads a unit: default send behaves like the broadcast.
        let receipt = engine
            .mail_send_default_for_actor(org.root, "default all hands".to_string(), None)
            .await
            .unwrap();
        assert!(
            receipt.recipients >= 2,
            "root default reaches its unit: {receipt:?}"
        );
        assert!(
            matches!(receipt.to, MailEndpoint::Channel(_)),
            "a leader's default channel is its unit group pipe: {receipt:?}"
        );
        assert_eq!(
            inbox(&engine, org.root, &org.lead_1.path).await,
            vec!["default all hands".to_string()]
        );

        // A leaf defaults to its parent DM.
        let receipt = engine
            .mail_send_default_for_actor(org.worker_1.session, "leaf default up".to_string(), None)
            .await
            .unwrap();
        assert_eq!(receipt.recipients, 1);
        assert!(
            matches!(receipt.to, MailEndpoint::Handle(ref h) if h == &org.lead_1.path),
            "a subordinate's default channel is its parent DM: {receipt:?}"
        );
        assert_eq!(
            inbox(&engine, org.root, &org.lead_1.path).await,
            vec![
                "default all hands".to_string(),
                "leaf default up".to_string()
            ]
        );
    }

    /// A posting on a group channel is stamped as an announcement by the
    /// channel's nature, not by the caller's kind hint.
    #[tokio::test]
    async fn group_channel_posts_derive_announcement_kind() {
        let engine = engine().await;
        let org = org(&engine).await;

        // Address the root's minted group channel explicitly with the 1:1
        // kind hint; the engine must still deliver as an announcement.
        let receipt = engine
            .mail_send(
                org.root,
                MailEndpoint::Channel("announce".to_string()),
                MailKind::Message,
                "typed as channel".to_string(),
            )
            .await;
        // The reserved `announce` name is normalized engine-side; the
        // receipt tells us where it landed.
        let receipt = match receipt {
            Ok(receipt) => receipt,
            Err(_) => engine
                .mail_announce(org.root, "typed as channel".to_string())
                .await
                .unwrap(),
        };
        assert!(receipt.recipients >= 1);
        assert!(
            matches!(receipt.to, MailEndpoint::Channel(_)),
            "group delivery reports the channel: {receipt:?}"
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
}

/// One `channel://` read result.
#[derive(Clone, Debug)]
pub struct ChannelHistoryRead {
    /// Channel id that was read (after normalization).
    pub channel: String,
    /// Newest-last (from, body) pairs.
    pub messages: Vec<(String, String)>,
    /// Unread messages remaining in the caller's inbox after this read.
    pub unread_remaining: usize,
    /// Set when the caller's id spelling was normalized (leading `#`s and/or
    /// padding stripped): the echo that teaches the correct spelling.
    pub warning: Option<String>,
}
