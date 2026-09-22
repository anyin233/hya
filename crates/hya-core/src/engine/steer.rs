//! In-turn mail steering (ADR-0016 follow-up): unread mail is surfaced to the
//! agent inside tool results, mid-turn, so a long interactive turn is never
//! blind to team mail and the report gate cannot loop on mail the agent has
//! already seen.
//!
//! Mechanics: one projection snapshot at turn start captures the durable
//! backlog (`inbox[resident_cursor..]`); a bus subscription follows live
//! `MailSent` deliveries that reach the acting handle. After each tool call
//! the turn loop drains the queue into a notice appended to the tool result
//! and commits a `MailConsumed` event advancing the durable cursor.

use std::collections::HashMap;

use hya_bundle::{ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{Envelope, Event, MailEndpoint, SessionId};

use super::SessionEngine;
use crate::error::CoreError;
use hya_tool::ChannelPolicySnapshot;

/// One steered message: sender, body, and the channel it arrived through.
#[derive(Clone, Debug)]
pub struct SteeredMail {
    /// Sender canonical handle.
    pub from: String,
    /// Message body.
    pub body: String,
    /// Channel id the message was delivered through (`DM-{8}` / `announce-{8}`);
    /// `None` when no channel matches (e.g. handle mail with no minted DM yet).
    pub channel: Option<String>,
}

/// Per-turn steer state: the backlog plus the live bus tail.
pub struct SteerMailbox {
    root: SessionId,
    handle: String,
    queue: Vec<SteeredMail>,
    /// Durable cursor covered so far; `MailConsumed.through` values.
    through: u64,
    /// Cursor already committed before this snapshot began scanning.
    committed_through: u64,
    /// Bus subscription for live deliveries.
    bus: tokio::sync::broadcast::Receiver<Envelope>,
    /// Channels the acting handle is a member of (for channel fan-out).
    channels: Vec<String>,
    /// Peer handle → the DM channel shared with the acting handle (first
    /// minted pair wins). Maps `to`-endpoints to the channel a reader can
    /// `read channel://<id>` for the full conversation.
    dm_by_peer: HashMap<String, String>,
    policy: Option<ChannelPolicySnapshot>,
    channel_roles: HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
}

impl SessionEngine {
    /// Snapshot the durable unread backlog and subscribe to live mail.
    ///
    /// Never fails the turn: a session outside a team gets an empty, inert
    /// mailbox (every drain returns nothing).
    pub async fn steer_mailbox_snapshot(&self, session: SessionId) -> SteerMailbox {
        let policy = self.session_channel_policy(session);
        self.steer_mailbox_snapshot_with_policy(session, policy)
            .await
    }

    /// Snapshot mail using the channel policy captured by this turn's admission binding.
    /// This is the turn-admission path; callers must pass the immutable snapshot
    /// derived from the turn's [`crate::TurnBinding`]. `None` denies delivery.
    pub async fn steer_mailbox_snapshot_with_policy(
        &self,
        session: SessionId,
        policy: Option<ChannelPolicySnapshot>,
    ) -> SteerMailbox {
        let bus = self.bus().subscribe();
        let Ok(root) = self.team_root(session).await else {
            return SteerMailbox {
                root: session,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                dm_by_peer: HashMap::new(),
                policy,
                channel_roles: HashMap::new(),
            };
        };
        let Ok(handle) = self.resolve_handle(root, session).await else {
            return SteerMailbox {
                root,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                dm_by_peer: HashMap::new(),
                policy,
                channel_roles: HashMap::new(),
            };
        };
        let Ok(projection) = self.read_projection(root).await else {
            return SteerMailbox {
                root,
                handle: handle.clone(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                dm_by_peer: HashMap::new(),
                policy,
                channel_roles: HashMap::new(),
            };
        };
        // Baseline: mail already claimed by THIS turn's wake. A resident wake
        // emits `ResidentWorkStarted.inbox_through` for the backlog it injects
        // as user prompts — steer must not duplicate those. An interactive
        // root turn claims nothing, so its durable backlog (which nothing else
        // will surface) becomes steer's to deliver.
        let cursor = projection.team.roster.get(&handle).map_or(0, |entry| {
            entry.resident_work.map_or(entry.resident_cursor, |work| {
                entry.resident_cursor.max(work.inbox_through)
            })
        });
        let channels: Vec<String> = projection
            .team
            .channels
            .iter()
            .filter(|(_, channel)| channel.members.contains(&handle))
            .map(|(key, _)| key.clone())
            .collect();
        // DM resolution table: every Dm channel the acting handle is in, keyed
        // by each member so a `to` handle maps to the pair's channel (the
        // first minted pair wins when several exist).
        let mut dm_by_peer: HashMap<String, String> = HashMap::new();
        let mut channel_roles = HashMap::new();
        for (id, channel) in &projection.team.channels {
            if channel.members.contains(&handle) {
                let topology = if channel.kind == hya_proto::ChannelKind::Group {
                    (
                        ChannelTemplateKind::Unit,
                        if channel.unit.as_deref() == Some(handle.as_str()) {
                            ChannelParticipantRole::UnitLeader
                        } else {
                            ChannelParticipantRole::DirectReports
                        },
                    )
                } else {
                    (
                        ChannelTemplateKind::ParentDm,
                        channel
                            .members
                            .iter()
                            .find(|member| member.as_str() != handle)
                            .map_or(ChannelParticipantRole::Child, |peer| {
                                if hya_proto::scope::parent_path(peer) == Some(handle.as_str()) {
                                    ChannelParticipantRole::Parent
                                } else {
                                    ChannelParticipantRole::Child
                                }
                            }),
                    )
                };
                channel_roles.insert(id.clone(), topology);
            }
            if channel.kind != hya_proto::ChannelKind::Dm || !channel.members.contains(&handle) {
                continue;
            }
            for member in &channel.members {
                dm_by_peer
                    .entry(member.clone())
                    .or_insert_with(|| id.clone());
            }
        }
        let mut mailbox = SteerMailbox {
            root,
            handle,
            queue: Vec::new(),
            through: cursor,
            committed_through: cursor,
            bus,
            channels,
            dm_by_peer,
            policy,
            channel_roles,
        };
        mailbox.queue = projection
            .team
            .inboxes
            .get(&mailbox.handle)
            .map(|inbox| {
                inbox
                    .iter()
                    .skip(usize::try_from(cursor).unwrap_or(usize::MAX))
                    .filter(|message| mailbox.allows_steer(&message.from, &message.to))
                    .map(|message| SteeredMail {
                        from: message.from.clone(),
                        body: message.body.clone(),
                        channel: delivered_channel(&message.to, &mailbox.dm_by_peer),
                    })
                    .collect()
            })
            .unwrap_or_default();
        mailbox.through = projection
            .team
            .inboxes
            .get(&mailbox.handle)
            .map_or(cursor, |inbox| inbox.len() as u64);
        mailbox
    }
}

/// The channel a delivered message arrived through, from its original address.
///
/// Channel-addressed mail names the channel directly. Handle-addressed mail
/// resolves to the DM pair shared by the acting handle and the endpoint (the
/// degenerate self-address picks the first DM the acting handle belongs to);
/// `None` when no such channel exists.
fn delivered_channel(to: &MailEndpoint, dm_by_peer: &HashMap<String, String>) -> Option<String> {
    match to {
        MailEndpoint::Channel(channel) => Some(channel.clone()),
        MailEndpoint::Handle(peer) => dm_by_peer.get(peer).cloned(),
    }
}

impl SteerMailbox {
    /// Replace only the admission policy at a successful Root round rebind.
    /// Mail cursor, queued messages, channel topology, and bus subscription stay intact.
    pub(crate) fn set_policy(&mut self, policy: ChannelPolicySnapshot) {
        self.policy = Some(policy);
    }

    /// Pull live `MailSent` deliveries that reach the acting handle.
    pub(crate) fn poll_live(&mut self) {
        if self.handle.is_empty() {
            // Drain the bus anyway so it cannot lag behind a full channel.
            while self.bus.try_recv().is_ok() {}
            return;
        }
        loop {
            match self.bus.try_recv() {
                Ok(envelope) => {
                    if let Event::MailSent { from, to, body, .. } = &envelope.event
                        && from != &self.handle
                        && self.reaches(to)
                    {
                        self.through = self.through.saturating_add(1);
                        if self.allows_steer(from, to) {
                            self.queue.push(SteeredMail {
                                from: from.clone(),
                                body: body.clone(),
                                channel: delivered_channel(to, &self.dm_by_peer),
                            });
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }

    fn reaches(&self, to: &MailEndpoint) -> bool {
        match to {
            MailEndpoint::Handle(handle) => handle == &self.handle,
            MailEndpoint::Channel(channel) => self.channels.contains(channel),
        }
    }

    fn allows_steer(&self, from: &str, to: &MailEndpoint) -> bool {
        let Some(policy) = self.policy else {
            return false;
        };
        let topology = delivered_channel(to, &self.dm_by_peer)
            .and_then(|channel| self.channel_roles.get(&channel).copied())
            .unwrap_or_else(|| {
                (
                    ChannelTemplateKind::ParentDm,
                    if hya_proto::scope::parent_path(from) == Some(self.handle.as_str()) {
                        ChannelParticipantRole::Parent
                    } else {
                        ChannelParticipantRole::Child
                    },
                )
            });
        match topology.0 {
            ChannelTemplateKind::Unit => snapshot_allows(policy, topology.1, true),
            ChannelTemplateKind::ParentDm => snapshot_allows(policy, topology.1, false),
        }
    }

    /// Drain pending mail into a notice appended to a tool result, advancing
    /// the durable cursor via `MailConsumed`. `None` when nothing is pending.
    pub async fn drain(&mut self, engine: &SessionEngine) -> Result<Option<String>, CoreError> {
        self.poll_live();
        if self.handle.is_empty() {
            return Ok(None);
        }
        let shown: Vec<SteeredMail> = std::mem::take(&mut self.queue);
        if shown.is_empty() && self.through == self.committed_through {
            return Ok(None);
        }
        let root = self.root;
        let handle = self.handle.clone();
        let through = self.through;
        engine
            .emit_for_actor(
                None,
                root,
                Event::MailConsumed {
                    session: root,
                    handle,
                    through,
                },
            )
            .await?;
        self.committed_through = through;
        if shown.is_empty() {
            return Ok(None);
        }
        let mut notice = String::from("\n\n--- [NEW MAIL · answer or acknowledge via dm] ---");
        for mail in shown {
            // Truncate on a char boundary — bodies are UTF-8 and a raw byte
            // slice panics on multi-byte characters.
            let body = if mail.body.len() > 600 {
                let mut end = 600;
                while end > 0 && !mail.body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &mail.body[..end])
            } else {
                mail.body.clone()
            };
            // Name the channel inline so the reader can `read channel://<id>`
            // directly instead of guessing id spellings (`##DM-x`, `#dm-x`).
            let channel = mail
                .channel
                .as_ref()
                .map_or(String::new(), |id| format!(" @{id}"));
            notice.push_str(&format!("\n[mail from {}{channel}] {}", mail.from, body));
        }
        notice.push_str("\n(history: read channel://<id>?last=N · unread overview: list_channel)");
        Ok(Some(notice))
    }
}

fn snapshot_allows(
    policy: ChannelPolicySnapshot,
    role: ChannelParticipantRole,
    unit: bool,
) -> bool {
    let bits = match (unit, role) {
        (true, ChannelParticipantRole::UnitLeader) => policy.unit_leader,
        (true, ChannelParticipantRole::DirectReports) => policy.unit_member,
        (false, ChannelParticipantRole::Parent) => policy.dm_parent,
        (false, ChannelParticipantRole::Child) => policy.dm_child,
        _ => 0,
    };
    bits & (1 << 2) != 0
}
