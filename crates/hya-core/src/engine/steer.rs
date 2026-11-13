//! In-turn mail steering (ADR-0016 follow-up): unread mail is surfaced to the
//! agent inside tool results, mid-turn, so a long interactive turn is never
//! blind to team mail and the report gate cannot loop on mail the agent has
//! already seen.
//!
//! Mechanics: one projection snapshot at turn start captures the durable
//! backlog (`inbox[resident_cursor..]`); a bus subscription notices live
//! `MailSent` deliveries on the team log. After each tool call, when anything
//! may be pending, the turn loop rebuilds the pending mail from the durable
//! inbox past the durable cursor — so mail another delivery path already
//! consumed (a `wait` that returned it) is never shown twice, and mail on a
//! channel minted after the turn began (a new member's DM) is not missed —
//! prepares a notice appended to the tool result. The turn commits the result
//! and `MailConsumed` together before notifying the mail observer.

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
    /// `None` for harness notices.
    pub channel: Option<String>,
}

/// Per-turn steer state: the backlog plus the live bus tail.
pub struct SteerMailbox {
    /// Cursor acknowledgement and whether it consumes incoming mail.
    pending_ack: Option<(Event, bool)>,
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
    policy: Option<ChannelPolicySnapshot>,
    channel_roles: HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
    /// Set once the live bus lagged (a busy team overflowed the broadcast
    /// buffer and dropped `MailSent` envelopes): from then on every drain
    /// re-reads the durable inbox instead of trusting the live tail.
    durable: bool,
    /// Team mail was sent since the last drain: the next drain re-reads the
    /// durable inbox.
    dirty: bool,
}

impl SessionEngine {
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
                pending_ack: None,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                policy,
                channel_roles: HashMap::new(),
                durable: false,
                dirty: false,
            };
        };
        let Ok(handle) = self.resolve_handle(root, session).await else {
            return SteerMailbox {
                root,
                pending_ack: None,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                policy,
                channel_roles: HashMap::new(),
                durable: false,
                dirty: false,
            };
        };
        let Ok(projection) = self.read_projection_shared(root).await else {
            return SteerMailbox {
                root,
                pending_ack: None,
                handle: handle.clone(),
                queue: Vec::new(),
                through: 0,
                committed_through: 0,
                bus,
                channels: Vec::new(),
                policy,
                channel_roles: HashMap::new(),
                durable: false,
                dirty: false,
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
        let (channels, channel_roles) = topology(&projection, &handle);
        let mut mailbox = SteerMailbox {
            root,
            pending_ack: None,
            handle,
            queue: Vec::new(),
            through: cursor,
            committed_through: cursor,
            bus,
            channels,
            policy,
            channel_roles,
            durable: false,
            dirty: false,
        };
        mailbox.queue = projection
            .team
            .unread_incoming_mail(&mailbox.handle, cursor)
            .filter(|message| mailbox.allows_steer(&message.from, &message.to))
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        mailbox.through = projection
            .team
            .inboxes
            .get(&mailbox.handle)
            .map_or(cursor, |inbox| inbox.len() as u64);
        mailbox
    }
}

/// The acting handle's channel memberships and role per channel.
#[allow(clippy::type_complexity)]
fn topology(
    projection: &hya_proto::Projection,
    handle: &str,
) -> (
    Vec<String>,
    HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
) {
    let channels: Vec<String> = projection
        .team
        .channels
        .iter()
        .filter(|(_, channel)| channel.members.contains(handle))
        .map(|(key, _)| key.clone())
        .collect();
    let mut channel_roles = HashMap::new();
    for (id, channel) in &projection.team.channels {
        if channel.members.contains(handle) {
            let topology = if channel.kind == hya_proto::ChannelKind::Group {
                (
                    ChannelTemplateKind::Unit,
                    if channel.unit.as_deref() == Some(handle) {
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
                            if hya_proto::scope::parent_path(peer) == Some(handle) {
                                ChannelParticipantRole::Parent
                            } else {
                                ChannelParticipantRole::Child
                            }
                        }),
                )
            };
            channel_roles.insert(id.clone(), topology);
        }
    }
    (channels, channel_roles)
}

impl SteerMailbox {
    /// Replace only the admission policy at a successful Root round rebind.
    /// Mail cursor, queued messages, channel topology, and bus subscription stay intact.
    pub(crate) fn set_policy(&mut self, policy: ChannelPolicySnapshot) {
        self.policy = Some(policy);
    }

    pub(crate) fn take_acknowledgement(&mut self) -> Option<(Event, bool)> {
        self.pending_ack.take()
    }

    /// Notice live `MailSent` deliveries on the team log (the next drain
    /// re-reads the durable inbox), or a bus lag.
    pub(crate) fn poll_live(&mut self) {
        if self.handle.is_empty() {
            // Drain the bus anyway so it cannot lag behind a full channel.
            while self.bus.try_recv().is_ok() {}
            return;
        }
        loop {
            match self.bus.try_recv() {
                Ok(envelope) => {
                    if let Event::MailSent {
                        session, from, to, ..
                    } = &envelope.event
                        && from != &self.handle
                        && (*session == self.root || self.reaches(to))
                    {
                        self.dirty = true;
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    // Deliveries were dropped: switch to the durable inbox.
                    self.durable = true;
                }
                Err(_) => break,
            }
        }
    }

    /// Rebuild the pending queue from the durable inbox: everything past the
    /// durable cursor (which another delivery — a `wait` returning the mail —
    /// may have advanced) and past what this mailbox committed, with the
    /// channel view refreshed so channels minted mid-turn resolve.
    async fn resync_from_log(&mut self, engine: &SessionEngine) -> Result<bool, CoreError> {
        // Cached fold: this must cost the root's new events, not its whole log.
        let projection = engine.read_projection_shared(self.root).await?;
        (self.channels, self.channel_roles) = topology(&projection, &self.handle);
        let durable = projection.team.roster.get(&self.handle).map_or(0, |entry| {
            entry.resident_work.map_or(entry.resident_cursor, |work| {
                entry.resident_cursor.max(work.inbox_through)
            })
        });
        self.committed_through = self.committed_through.max(durable);
        let Some(inbox) = projection.team.inboxes.get(&self.handle) else {
            self.queue.clear();
            self.through = self.committed_through;
            return Ok(false);
        };
        let consumed_incoming = projection
            .team
            .unread_incoming_mail(&self.handle, self.committed_through)
            .next()
            .is_some();
        let pending = projection
            .team
            .unread_incoming_mail(&self.handle, self.committed_through)
            .filter(|message| self.allows_steer(&message.from, &message.to))
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        self.queue = pending;
        self.through = self.committed_through.max(inbox.len() as u64);
        Ok(consumed_incoming)
    }

    fn reaches(&self, to: &MailEndpoint) -> bool {
        match to {
            MailEndpoint::Handle(handle) => handle == &self.handle,
            MailEndpoint::Channel(channel) => self.channels.contains(channel),
        }
    }

    fn allows_steer(&self, from: &str, to: &MailEndpoint) -> bool {
        // Harness mail (the leader-failed wrap-up notice) is a control
        // message, never filtered by the channel policy.
        if from == hya_proto::scope::HARNESS_HANDLE {
            return true;
        }
        let Some(policy) = self.policy else {
            return false;
        };
        let MailEndpoint::Channel(channel) = to else {
            return false;
        };
        let topology = self.channel_roles[channel];
        match topology.0 {
            ChannelTemplateKind::Unit => snapshot_allows(policy, topology.1, true),
            ChannelTemplateKind::ParentDm => snapshot_allows(policy, topology.1, false),
        }
    }

    /// Prepare a notice without acknowledging delivery. The turn must commit
    /// `take_acknowledgement()` atomically with the result carrying this notice.
    pub async fn drain(&mut self, engine: &SessionEngine) -> Result<Option<String>, CoreError> {
        self.poll_live();
        if self.handle.is_empty() {
            return Ok(None);
        }
        let pending = self.durable
            || self.dirty
            || !self.queue.is_empty()
            || self.through != self.committed_through;
        if !pending {
            return Ok(None);
        }
        self.dirty = false;
        let consumed_incoming = self.resync_from_log(engine).await?;
        let shown: Vec<SteeredMail> = std::mem::take(&mut self.queue);
        if shown.is_empty() && self.through == self.committed_through {
            return Ok(None);
        }
        let root = self.root;
        let handle = self.handle.clone();
        let through = self.through;
        self.pending_ack = Some((
            Event::MailConsumed {
                session: root,
                handle,
                through,
            },
            consumed_incoming,
        ));
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
