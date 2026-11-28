//! Managed ABI v2 [`NativeService`] owning mailbox delivery, channel queries,
//! unread-mail steering preparation, and team status for `hya/agent-channels`.
//!
//! All host access flows exclusively through [`HostRuntime`].

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use hya_bundle::ChannelTemplateKind;
use hya_proto::{
    ActorClaim, AgentName, ChannelKind, ChannelPolicySnapshot, Event, MailEndpoint, MailKind,
    Projection, RosterStatus, SessionId, SubagentMode, scope,
};
use hya_runtime_abi::{
    HostError, HostRuntime, MemberMailRevivalRequest, NativeService, ServiceHealth,
};
use hya_tool::extension::{
    BUNDLE_CHANNELS, BUNDLE_MULTI_AGENT, ChannelProjection, ExtensionEvent, TeamProjection,
    team_projection,
};
use hya_tool::{ChannelRow, MailReceipt, MemberStatusRow};

use crate::mail::{ChannelHistoryRead, MAIN_HANDLE, SteeredMail, mail_preview};
use crate::policy::{BIT_SEND, allows_steer_message, channel_topology};

/// Canonical bundle id for `hya/agent-channels`.
pub const AGENT_CHANNELS_BUNDLE_ID: &str = BUNDLE_CHANNELS;
/// Service id registered with ABI v2.
pub const AGENT_CHANNELS_SERVICE_ID: &str = "agent-channels";

const MAX_LINEAGE_DEPTH: usize = 64;

/// Read-only inbox summary returned by the `GET /inbox` bundle API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InboxSummary {
    /// Acting agent's canonical handle.
    pub handle: String,
    /// Durable acknowledged cursor.
    pub cursor: u64,
    /// Total messages ever appended to this handle's inbox.
    pub total: usize,
    /// Unread incoming messages past `cursor`.
    pub unread: Vec<SteeredMail>,
}

/// Prepared unread-mail steer notice plus the inbox acknowledgement to stage
/// with the tool result.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedSteerNotice {
    /// Model-facing notice text to splice into the tool result, if any mail
    /// should be shown to the agent (`None` when only self-sent or
    /// policy-filtered messages need cursor advancement).
    pub notice: Option<String>,
    /// `mail_consumed` extension event for the team root's log.
    pub ack: Event,
    /// Host mail watermark read before the inbox snapshot.
    pub mark: u64,
    /// Whether incoming (non-self) unread mail was consumed.
    pub consumed_incoming: bool,
}

/// Managed mailbox/channels service backed exclusively by [`HostRuntime`].
#[derive(Default)]
pub struct AgentChannelsService {
    runtime: RwLock<Option<Arc<dyn HostRuntime>>>,
}

impl AgentChannelsService {
    /// Construct an unstarted service awaiting [`NativeService::start`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct a service pre-bound to `runtime` (useful for tests and
    /// direct in-process embedding).
    #[must_use]
    pub fn with_runtime(runtime: Arc<dyn HostRuntime>) -> Self {
        Self {
            runtime: RwLock::new(Some(runtime)),
        }
    }

    /// Whether the service has been started with a [`HostRuntime`].
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Return the currently bound [`HostRuntime`], or [`HostError::Unavailable`].
    pub fn runtime(&self) -> Result<Arc<dyn HostRuntime>, HostError> {
        self.runtime
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| {
                HostError::Unavailable("hya/agent-channels service is not started".to_string())
            })
    }

    /// Resolve the team-root session by walking `parent` links through [`HostSessionStore`].
    pub async fn team_root(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<SessionId, HostError> {
        let mut current = session;
        for _ in 0..MAX_LINEAGE_DEPTH {
            let Some(meta) = runtime.session_store().session_metadata(current).await? else {
                break;
            };
            match meta.parent {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }
        Ok(current)
    }

    /// Ensure the team root has a roster entry (`main`), emitting
    /// `hya/multi-agent::agent_registered` if absent.
    pub async fn ensure_root_registered(
        runtime: &dyn HostRuntime,
        root: SessionId,
        actor_claim: Option<ActorClaim>,
    ) -> Result<String, HostError> {
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        if let Some(entry) = team.roster.values().find(|entry| entry.session == root) {
            return Ok(entry.handle.clone());
        }
        let agent_type = projection
            .session
            .agent
            .clone()
            .unwrap_or_else(|| AgentName::new(MAIN_HANDLE));
        let event = ExtensionEvent::AgentRegistered {
            session: root,
            agent_session: root,
            handle: MAIN_HANDLE.to_string(),
            parent: None,
            agent_type,
            mode: SubagentMode::Transient,
        };
        let env = append_extension(runtime, root, event, actor_claim).await?;
        let _ = runtime.event_bus().publish(env);
        Ok(MAIN_HANDLE.to_string())
    }

    /// Resolve `session` to its canonical team handle.
    pub async fn resolve_handle(
        runtime: &dyn HostRuntime,
        root: SessionId,
        session: SessionId,
    ) -> Result<String, HostError> {
        let projection = runtime.session_store().read_projection(root).await?;
        if let Some(entry) = team_projection(&projection)
            .roster
            .values()
            .find(|entry| entry.session == session)
        {
            return Ok(entry.handle.clone());
        }
        if session == root {
            return Self::ensure_root_registered(runtime, root, None).await;
        }
        Err(HostError::InvalidInput(
            "this agent has no team handle yet; it must be spawned as a team member to use the mailbox"
                .to_string(),
        ))
    }

    /// List visible channels (group pipes + live-peer DMs) with unread counts.
    pub async fn list_channels(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<Vec<ChannelRow>, HostError> {
        let root = Self::team_root(runtime, session).await?;
        let handle = Self::resolve_handle(runtime, root, session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let cursor = effective_cursor(&team, &handle);
        let unread_indices: HashSet<_> = team
            .inboxes
            .get(&handle)
            .into_iter()
            .flatten()
            .skip(usize::try_from(cursor).unwrap_or(usize::MAX))
            .copied()
            .filter(|&index| team.mail[index].from != handle)
            .collect();
        let mut rows = Vec::new();
        for (id, channel) in &team.channels {
            if !channel.members.contains(&handle) {
                continue;
            }
            let unread = channel
                .log
                .iter()
                .filter(|index| unread_indices.contains(*index))
                .count();
            match channel.kind {
                ChannelKind::Group => rows.push(ChannelRow {
                    id: id.clone(),
                    kind: ChannelKind::Group,
                    can_speak: channel.unit.as_deref() == Some(handle.as_str()),
                    peer: None,
                    unread,
                }),
                ChannelKind::Dm => {
                    let peer = channel
                        .members
                        .iter()
                        .find(|member| *member != &handle)
                        .cloned();
                    let peer_live = peer
                        .as_ref()
                        .is_some_and(|peer| team.roster.contains_key(peer));
                    if peer_live {
                        rows.push(ChannelRow {
                            id: id.clone(),
                            kind: ChannelKind::Dm,
                            can_speak: true,
                            peer,
                            unread,
                        });
                    }
                }
            }
        }
        rows.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(rows)
    }

    /// Direct-child liveness rows for the acting agent (ADR-0002).
    pub async fn team_status(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<Vec<MemberStatusRow>, HostError> {
        let root = Self::team_root(runtime, session).await?;
        let handle = Self::resolve_handle(runtime, root, session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let now = u64::try_from(hya_proto::now_millis()).unwrap_or(0);
        let mut rows = Vec::new();
        for (path, entry) in &team_projection(&projection).roster {
            if scope::parent_path(path) != Some(handle.as_str()) {
                continue;
            }
            rows.push(MemberStatusRow {
                handle: path.clone(),
                status: roster_status_label(entry.status).to_string(),
                last_active_seconds: (entry.heartbeat_ms > 0)
                    .then(|| now.saturating_sub(entry.heartbeat_ms) / 1_000),
            });
        }
        rows.sort_by(|left, right| left.handle.cmp(&right.handle));
        Ok(rows)
    }

    /// Read-only inbox overview for bundle API consumers (never consumes unread mail).
    pub async fn inbox_summary(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<InboxSummary, HostError> {
        let root = Self::team_root(runtime, session).await?;
        let handle = Self::resolve_handle(runtime, root, session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let cursor = effective_cursor(&team, &handle);
        let total = team.inboxes.get(&handle).map_or(0, Vec::len);
        let unread = team
            .unread_incoming_mail(&handle, cursor)
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        Ok(InboxSummary {
            handle,
            cursor,
            total,
            unread,
        })
    }

    /// Read one channel's recent history WITHOUT advancing the caller's inbox cursor
    /// (for read-only UI/API inspection).
    pub async fn peek_channel_history(
        runtime: &dyn HostRuntime,
        session: SessionId,
        channel: &str,
        last: Option<usize>,
    ) -> Result<ChannelHistoryRead, HostError> {
        let root = Self::team_root(runtime, session).await?;
        let handle = Self::resolve_handle(runtime, root, session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let (normalized, channel_state, warning) = resolve_channel_read(&team, &handle, channel)?;
        let last_n = last.unwrap_or(20).clamp(1, 100);
        let messages = recent_channel_messages(&team, &channel_state, last_n);
        let cursor = effective_cursor(&team, &handle);
        let unread_remaining = team.unread_incoming_mail(&handle, cursor).count();
        Ok(ChannelHistoryRead {
            channel: normalized,
            messages,
            unread_remaining,
            warning,
        })
    }

    /// Read one channel's recent history for the acting agent (ADR-0016 `channel://`
    /// read), marking the caller's current inbox as seen.
    pub async fn read_channel_history(
        runtime: &dyn HostRuntime,
        actor_claim: Option<ActorClaim>,
        session: SessionId,
        channel: &str,
        last: Option<usize>,
    ) -> Result<ChannelHistoryRead, HostError> {
        let root = Self::team_root(runtime, session).await?;
        let handle = Self::resolve_handle(runtime, root, session).await?;
        // Taken before the projection snapshot: mail arriving later stays owed.
        let mark = runtime.turn_runner().mail_mark(session);
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let (normalized, channel_state, warning) = resolve_channel_read(&team, &handle, channel)?;
        let last_n = last.unwrap_or(1).clamp(1, 50);
        let messages = recent_channel_messages(&team, &channel_state, last_n);
        let inbox_len = team
            .inboxes
            .get(&handle)
            .map_or(0, |inbox| inbox.len() as u64);
        let cursor = effective_cursor(&team, &handle);
        let mut unread = team.unread_incoming_mail(&handle, cursor).peekable();
        let incoming = unread.peek().is_some();
        let showed_all_unread = unread.all(|unread| {
            messages
                .iter()
                .any(|(from, body)| *from == unread.from && *body == unread.body)
        });
        drop(unread);
        drop(projection);
        if cursor < inbox_len {
            let env = append_extension(
                runtime,
                root,
                ExtensionEvent::MailConsumed {
                    session: root,
                    handle: handle.clone(),
                    through: inbox_len,
                },
                actor_claim,
            )
            .await?;
            let _ = runtime.event_bus().publish(env);
            if showed_all_unread && let Some(mark) = mark {
                runtime
                    .turn_runner()
                    .notify_mail_consumed(session, mark, incoming);
            }
        }
        Ok(ChannelHistoryRead {
            channel: normalized,
            messages,
            unread_remaining: 0,
            warning,
        })
    }

    /// Send mail from `from_session` to a handle or `#channel`, enforcing
    /// channel policy, membership, resident claim liveness, user-stop guards,
    /// and archived-child revival.
    #[allow(clippy::too_many_arguments)]
    pub async fn send(
        runtime: &dyn HostRuntime,
        from_session: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        actor_claim: Option<ActorClaim>,
        channel_policy: Option<ChannelPolicySnapshot>,
        user_steer: bool,
    ) -> Result<MailReceipt, HostError> {
        let root = Self::team_root(runtime, from_session).await?;
        let from = Self::resolve_handle(runtime, root, from_session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let channel = match to {
            MailEndpoint::Channel(channel) => channel.trim().to_string(),
            MailEndpoint::Handle(handle) => team.resolve_dm(&from, &handle).ok_or_else(|| {
                mail_rejected(
                    &from,
                    &format!("direct:{handle}"),
                    "no DM channel with that target; run `list_channel` to see your channels",
                )
            })?,
        };
        let to = MailEndpoint::Channel(channel.clone());

        if !user_steer {
            let peer = team
                .channels
                .get(&channel)
                .filter(|state| state.kind == ChannelKind::Dm && state.members.contains(&from))
                .and_then(|state| state.members.iter().find(|member| **member != from))
                .cloned();
            if let Some(peer) = peer
                && peer
                    .strip_prefix(from.as_str())
                    .is_some_and(|rest| rest.starts_with(scope::PATH_SEPARATOR))
                && runtime.turn_runner().is_member_stopped_by_user(root, &peer)
            {
                return Err(HostError::InvalidInput(format!(
                    "the user stopped `{peer}`; it stays stopped until the user prompts or steers it. Do not try to restart it: tell the user what you need from it instead"
                )));
            }
        }

        let (kind, recipients) = match authorize_channel_mail(
            runtime,
            root,
            &projection,
            &from,
            &channel,
            kind,
            channel_policy,
        )
        .await?
        {
            MailAuthOutcome::Allowed { kind, recipients } => (kind, recipients),
            MailAuthOutcome::ReviveDirectChild { target } => {
                drop(projection);
                let recipients = runtime
                    .turn_runner()
                    .revive_archived_member_with_mail(MemberMailRevivalRequest {
                        root,
                        from_session,
                        from_handle: from.clone(),
                        target_handle: target,
                        channel: channel.clone(),
                        kind,
                        body,
                        actor_claim,
                    })
                    .await?;
                return Ok(MailReceipt {
                    from,
                    to,
                    recipients,
                });
            }
        };
        drop(projection);

        let envelope = append_extension(
            runtime,
            root,
            ExtensionEvent::MailSent {
                session: root,
                from: from.clone(),
                to: to.clone(),
                kind,
                body,
            },
            actor_claim,
        )
        .await?;
        let _ = runtime.event_bus().publish(envelope);
        Ok(MailReceipt {
            from,
            to,
            recipients,
        })
    }

    /// Post a one-way announcement to the unit the acting agent leads.
    pub async fn announce(
        runtime: &dyn HostRuntime,
        from_session: SessionId,
        body: String,
        actor_claim: Option<ActorClaim>,
        channel_policy: Option<ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, HostError> {
        let root = Self::team_root(runtime, from_session).await?;
        let from = Self::resolve_handle(runtime, root, from_session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let Some(group) = team_projection(&projection)
            .channels
            .iter()
            .find(|(_, channel)| {
                channel.kind == ChannelKind::Group && channel.unit.as_deref() == Some(from.as_str())
            })
            .map(|(key, _)| key.clone())
        else {
            return Err(HostError::InvalidInput(
                "you do not lead a unit yet; broadcast reaches your direct reports only"
                    .to_string(),
            ));
        };
        drop(projection);
        Self::send(
            runtime,
            from_session,
            MailEndpoint::Channel(group),
            MailKind::Announcement,
            body,
            actor_claim,
            channel_policy,
            false,
        )
        .await
    }

    /// Send on the acting agent's default channel: the unit group pipe it leads,
    /// else its parent DM pair.
    pub async fn send_default(
        runtime: &dyn HostRuntime,
        from_session: SessionId,
        body: String,
        actor_claim: Option<ActorClaim>,
        channel_policy: Option<ChannelPolicySnapshot>,
    ) -> Result<MailReceipt, HostError> {
        let root = Self::team_root(runtime, from_session).await?;
        let from = Self::resolve_handle(runtime, root, from_session).await?;
        let projection = runtime.session_store().read_projection(root).await?;
        let leads_unit = team_projection(&projection)
            .channels
            .iter()
            .any(|(_, channel)| {
                channel.kind == ChannelKind::Group && channel.unit.as_deref() == Some(from.as_str())
            });
        drop(projection);
        if leads_unit {
            return Self::announce(runtime, from_session, body, actor_claim, channel_policy).await;
        }
        if let Some(parent) = scope::parent_path(&from) {
            return Self::send(
                runtime,
                from_session,
                MailEndpoint::Handle(parent.to_string()),
                MailKind::Message,
                body,
                actor_claim,
                channel_policy,
                false,
            )
            .await;
        }
        Err(HostError::InvalidInput(
            "no default channel: you neither lead a unit nor report to one; address a channel explicitly with `#channel` or a peer handle"
                .to_string(),
        ))
    }

    /// Inspect `session`'s durable inbox past its acknowledged cursor and prepare
    /// an in-turn `[NEW MAIL]` steer notice plus its `mail_consumed` acknowledgement.
    ///
    /// Returns `Ok(None)` when `session` is not registered on a team roster or
    /// has no unread entries past its durable cursor.
    pub async fn prepare_unread_steer(
        runtime: &dyn HostRuntime,
        session: SessionId,
    ) -> Result<Option<PreparedSteerNotice>, HostError> {
        let Ok(root) = Self::team_root(runtime, session).await else {
            return Ok(None);
        };
        let mark = runtime.turn_runner().mail_mark(session).unwrap_or(0);
        let projection = runtime.session_store().read_projection(root).await?;
        let team = team_projection(&projection);
        let Some(entry) = team.roster.values().find(|entry| entry.session == session) else {
            return Ok(None);
        };
        let handle = entry.handle.clone();
        let baseline = entry.resident_work.map_or(entry.resident_cursor, |work| {
            entry.resident_cursor.max(work.inbox_through)
        });
        let Some(inbox) = team.inboxes.get(&handle) else {
            return Ok(None);
        };
        let through = baseline.max(inbox.len() as u64);
        if through <= baseline {
            return Ok(None);
        }
        let policy = runtime
            .turn_runner()
            .session_channel_policy(session)
            .await?;
        let (_, channel_roles) = channel_topology(&team, &handle);
        let consumed_incoming = team
            .unread_incoming_mail(&handle, baseline)
            .next()
            .is_some();
        let shown: Vec<SteeredMail> = team
            .unread_incoming_mail(&handle, baseline)
            .filter(|message| {
                allows_steer_message(policy, &channel_roles, &message.from, &message.to)
            })
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        let ack = ExtensionEvent::MailConsumed {
            session: root,
            handle,
            through,
        }
        .into_event();
        let notice = if shown.is_empty() {
            None
        } else {
            Some(format_steer_notice(&shown))
        };
        Ok(Some(PreparedSteerNotice {
            notice,
            ack,
            mark,
            consumed_incoming,
        }))
    }
}

#[async_trait]
impl NativeService for AgentChannelsService {
    fn id(&self) -> &str {
        AGENT_CHANNELS_SERVICE_ID
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(runtime);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        *self
            .runtime
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        if self.is_started() {
            ServiceHealth::healthy("ready")
        } else {
            ServiceHealth::unhealthy("stopped")
        }
    }
}

/// Format one or more steered messages into the model-facing `[NEW MAIL]` footer.
#[must_use]
pub fn format_steer_notice(shown: &[SteeredMail]) -> String {
    let mut notice = String::from("\n\n--- [NEW MAIL · answer or acknowledge via dm] ---");
    for mail in shown {
        let body = mail_preview(&mail.body);
        let channel = mail
            .channel
            .as_ref()
            .map_or(String::new(), |id| format!(" @{id}"));
        notice.push_str(&format!("\n[mail from {}{channel}] {}", mail.from, body));
    }
    notice.push_str("\n(history: read channel://<id>?last=N · unread overview: list_channel)");
    notice
}

enum MailAuthOutcome {
    Allowed { kind: MailKind, recipients: usize },
    ReviveDirectChild { target: String },
}

async fn authorize_channel_mail(
    runtime: &dyn HostRuntime,
    root: SessionId,
    projection: &Projection,
    from: &str,
    key: &str,
    kind: MailKind,
    policy: Option<ChannelPolicySnapshot>,
) -> Result<MailAuthOutcome, HostError> {
    let team = team_projection(projection);
    if from != scope::ROOT_HANDLE && !team.roster.contains_key(from) {
        return Err(mail_rejected(from, key, "sender is not on the live roster"));
    }
    let state = team
        .channels
        .get(key)
        .ok_or_else(|| mail_rejected(from, key, "channel does not exist"))?;
    if !state.members.contains(from) {
        return Err(mail_rejected(from, key, "sender is not a channel member"));
    }
    let group = state.kind == ChannelKind::Group;
    if group && state.unit.as_deref() != Some(from) {
        return Err(mail_rejected(
            from,
            key,
            "only the unit leader may announce",
        ));
    }
    let bits = policy.map(|policy| {
        if group {
            policy.unit_leader
        } else if state
            .members
            .iter()
            .any(|peer| scope::parent_path(peer) == Some(from))
        {
            policy.dm_parent
        } else {
            policy.dm_child
        }
    });
    if bits.is_some_and(|bits| bits & BIT_SEND == 0) {
        return Err(mail_rejected(from, key, "direction denied by policy"));
    }
    if !group {
        for peer in state.members.iter().filter(|peer| peer.as_str() != from) {
            if team.archived.contains_key(peer) && !team.roster.contains_key(peer) {
                if scope::parent_path(peer) == Some(from) {
                    return Ok(MailAuthOutcome::ReviveDirectChild {
                        target: peer.clone(),
                    });
                }
                return Err(mail_rejected(
                    from,
                    key,
                    "archived recipient is not a direct child",
                ));
            }
        }
        for member in &state.members {
            if !team.mail_recipient_is_live(member) {
                return Err(mail_rejected(from, key, "recipient is stopped or terminal"));
            }
            if let Some(entry) = team.roster.get(member)
                && entry.session != root
                && entry.mode.is_resident()
            {
                let claim = runtime
                    .bundle_storage()
                    .read_claim(BUNDLE_MULTI_AGENT, &entry.session.to_string())
                    .await?;
                if claim.is_none() {
                    return Err(mail_rejected(
                        from,
                        key,
                        "recipient has no active actor claim",
                    ));
                }
            }
        }
    }
    let recipients = state
        .members
        .iter()
        .filter(|member| team.mail_recipient_is_live(member))
        .count();
    let kind = if group { MailKind::Announcement } else { kind };
    Ok(MailAuthOutcome::Allowed { kind, recipients })
}

async fn append_extension(
    runtime: &dyn HostRuntime,
    session: SessionId,
    event: ExtensionEvent,
    actor_claim: Option<ActorClaim>,
) -> Result<hya_proto::Envelope, HostError> {
    let hya_proto::Event::Extension {
        bundle,
        kind,
        payload,
        ..
    } = event.into_event()
    else {
        return Err(HostError::Internal(
            "expected Event::Extension from ExtensionEvent".to_string(),
        ));
    };
    runtime
        .session_store()
        .append_extension_event(session, &bundle, &kind, payload, actor_claim)
        .await
}

fn effective_cursor(team: &TeamProjection, handle: &str) -> u64 {
    team.roster
        .get(handle)
        .map_or(0, |entry| entry.resident_cursor)
}

fn resolve_channel_read(
    team: &TeamProjection,
    handle: &str,
    channel: &str,
) -> Result<(String, ChannelProjection, Option<String>), HostError> {
    let normalized = channel.trim().trim_start_matches('#');
    let mut warning = (normalized != channel)
        .then(|| format!("normalized channel id `{channel}` → `{normalized}`"));
    let (normalized, channel_state) = match team.channels.get(normalized) {
        Some(state) if state.members.contains(handle) => (normalized.to_string(), state.clone()),
        Some(_) => return Err(unknown_channel(team, handle, normalized)),
        None => {
            let Some((id, state)) = dm_with_handle(team, handle, normalized) else {
                return Err(unknown_channel(team, handle, normalized));
            };
            let alias = format!(
                "`#{normalized}` is a member handle, not a channel: showing your DM with it, `{id}` (read it as `read channel://{id}`)"
            );
            warning = Some(match warning {
                Some(earlier) => format!("{earlier}; {alias}"),
                None => alias,
            });
            (id, state)
        }
    };
    Ok((normalized, channel_state, warning))
}

fn recent_channel_messages(
    team: &TeamProjection,
    channel_state: &ChannelProjection,
    last_n: usize,
) -> Vec<(String, String)> {
    channel_state
        .log
        .iter()
        .rev()
        .take(last_n)
        .map(|index| {
            let message = &team.mail[*index];
            (message.from.clone(), message.body.clone())
        })
        .collect()
}

fn dm_with_handle(
    team: &TeamProjection,
    caller: &str,
    raw: &str,
) -> Option<(String, ChannelProjection)> {
    let id = team.resolve_dm(caller, raw)?;
    let state = team.channels.get(&id)?.clone();
    Some((id, state))
}

fn unknown_channel(team: &TeamProjection, caller: &str, raw: &str) -> HostError {
    const LISTED: usize = 8;
    let mut mine: Vec<String> = team
        .channels
        .iter()
        .filter(|(_, channel)| channel.members.iter().any(|member| member == caller))
        .map(|(id, channel)| {
            let peer = (channel.kind == ChannelKind::Dm)
                .then(|| channel.members.iter().find(|member| *member != caller))
                .flatten();
            match peer {
                Some(peer) => format!("`{id}` (DM with {peer})"),
                None => format!("`{id}`"),
            }
        })
        .collect();
    mine.sort();
    let more = mine.len().saturating_sub(LISTED);
    mine.truncate(LISTED);
    let yours = if mine.is_empty() {
        "you have no channels".to_string()
    } else if more > 0 {
        format!("your channels: {} and {more} more", mine.join(", "))
    } else {
        format!("your channels: {}", mine.join(", "))
    };
    HostError::InvalidInput(format!(
        "unknown channel `#{raw}`; {yours}. Read one with `read channel://<id>`; `list_channel` lists them with unread counts."
    ))
}

fn mail_rejected(sender: &str, channel: &str, reason: &str) -> HostError {
    HostError::InvalidInput(format!(
        "mail authorization rejected for `{sender}` on `{channel}`: {reason}"
    ))
}

fn roster_status_label(status: RosterStatus) -> &'static str {
    match status {
        RosterStatus::Idle => "idle",
        RosterStatus::Busy => "busy",
        RosterStatus::Done => "done",
        RosterStatus::Failed => "failed",
    }
}

/// Convert a [`HostError`] into a [`hya_tool::ToolError`] preserving capacity
/// overload and input rejection semantics.
#[must_use]
pub fn host_error_to_tool_error(error: HostError) -> hya_tool::ToolError {
    match error {
        HostError::InvalidInput(message) | HostError::PermissionDenied(message) => {
            hya_tool::ToolError::Input(message)
        }
        HostError::Conflict(message) | HostError::Unavailable(message)
            if message.contains("CAPACITY_EXCEEDED")
                || message.contains("capacity")
                || message.contains("overloaded") =>
        {
            hya_tool::ToolError::Overloaded(message)
        }
        HostError::Unavailable(_) => hya_tool::ToolError::Other(
            "mailbox is only available inside a running team".to_string(),
        ),
        other => hya_tool::ToolError::Other(other.to_string()),
    }
}

/// Validate that `kind` is allowed for a channel send when using `ChannelTemplateKind`.
#[must_use]
pub const fn channel_kind_template(kind: ChannelKind) -> ChannelTemplateKind {
    match kind {
        ChannelKind::Group => ChannelTemplateKind::Unit,
        ChannelKind::Dm => ChannelTemplateKind::ParentDm,
    }
}
