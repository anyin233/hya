//! Compatibility steer snapshot facade delegating topology, policy, and notice
//! formatting to `hya/agent-channels`.
//!
//! Live turn steering runs through [`hya_agent_channels_core::UnreadMailSteerHook`]
//! (`HookPoint::ToolExecuteAfter`). This wrapper preserves the
//! [`SessionEngine::steer_mailbox_snapshot_with_policy`] API used by engine
//! integration tests.

use std::collections::HashMap;

use hya_agent_channels_core::{
    SteeredMail, allows_steer_message, channel_topology, format_steer_notice,
};
use hya_bundle::{ChannelParticipantRole, ChannelTemplateKind};
use hya_proto::{Envelope, Event, MailEndpoint, SessionId};
use hya_tool::ChannelPolicySnapshot;

use super::SessionEngine;
use crate::error::CoreError;

/// Per-turn steer state: the backlog plus the live bus tail.
pub struct SteerMailbox {
    pending_ack: Option<(Event, bool)>,
    root: SessionId,
    handle: String,
    queue: Vec<SteeredMail>,
    through: u64,
    committed_through: u64,
    bus: tokio::sync::broadcast::Receiver<Envelope>,
    channels: Vec<String>,
    policy: Option<ChannelPolicySnapshot>,
    channel_roles: HashMap<String, (ChannelTemplateKind, ChannelParticipantRole)>,
    durable: bool,
    dirty: bool,
}

impl SessionEngine {
    /// Snapshot unread mail using the given channel policy snapshot.
    pub async fn steer_mailbox_snapshot_with_policy(
        &self,
        session: SessionId,
        policy: Option<ChannelPolicySnapshot>,
    ) -> SteerMailbox {
        let bus = self.bus().subscribe();
        let Ok(root) = self.team_root(session).await else {
            return SteerMailbox::empty(session, String::new(), bus, policy);
        };
        let Ok(handle) = self.resolve_handle(root, session).await else {
            return SteerMailbox::empty(root, String::new(), bus, policy);
        };
        let Ok(projection) = self.read_projection_shared(root).await else {
            return SteerMailbox::empty(root, handle, bus, policy);
        };
        let team = hya_tool::extension::team_projection(&projection);
        let cursor = team.roster.get(&handle).map_or(0, |entry| {
            entry.resident_work.map_or(entry.resident_cursor, |work| {
                entry.resident_cursor.max(work.inbox_through)
            })
        });
        let (channels, channel_roles) = channel_topology(&team, &handle);
        let queue = team
            .unread_incoming_mail(&handle, cursor)
            .filter(|message| {
                allows_steer_message(policy, &channel_roles, &message.from, &message.to)
            })
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        let through = team
            .inboxes
            .get(&handle)
            .map_or(cursor, |inbox| inbox.len() as u64);
        SteerMailbox {
            root,
            pending_ack: None,
            handle,
            queue,
            through,
            committed_through: cursor,
            bus,
            channels,
            policy,
            channel_roles,
            durable: false,
            dirty: false,
        }
    }
}

impl SteerMailbox {
    #[cfg(test)]
    pub(crate) fn take_acknowledgement(&mut self) -> Option<(Event, bool)> {
        self.pending_ack.take()
    }

    fn empty(
        root: SessionId,
        handle: String,
        bus: tokio::sync::broadcast::Receiver<Envelope>,
        policy: Option<ChannelPolicySnapshot>,
    ) -> Self {
        Self {
            root,
            pending_ack: None,
            handle,
            queue: Vec::new(),
            through: 0,
            committed_through: 0,
            bus,
            channels: Vec::new(),
            policy,
            channel_roles: HashMap::new(),
            durable: false,
            dirty: false,
        }
    }

    fn poll_live(&mut self) {
        if self.handle.is_empty() {
            while self.bus.try_recv().is_ok() {}
            return;
        }
        loop {
            match self.bus.try_recv() {
                Ok(envelope) => {
                    if let Some(hya_tool::extension::ExtensionEvent::MailSent {
                        session,
                        from,
                        to,
                        ..
                    }) = hya_tool::extension::ExtensionEvent::from_event(&envelope.event)
                        && from != self.handle
                        && (session == self.root || self.reaches(&to))
                    {
                        self.dirty = true;
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    self.durable = true;
                }
                Err(_) => break,
            }
        }
    }

    async fn resync_from_log(&mut self, engine: &SessionEngine) -> Result<bool, CoreError> {
        let projection = engine.read_projection_shared(self.root).await?;
        let team = hya_tool::extension::team_projection(&projection);
        (self.channels, self.channel_roles) = channel_topology(&team, &self.handle);
        let durable = team.roster.get(&self.handle).map_or(0, |entry| {
            entry.resident_work.map_or(entry.resident_cursor, |work| {
                entry.resident_cursor.max(work.inbox_through)
            })
        });
        self.committed_through = self.committed_through.max(durable);
        let Some(inbox) = team.inboxes.get(&self.handle) else {
            self.queue.clear();
            self.through = self.committed_through;
            return Ok(false);
        };
        let consumed_incoming = team
            .unread_incoming_mail(&self.handle, self.committed_through)
            .next()
            .is_some();
        self.queue = team
            .unread_incoming_mail(&self.handle, self.committed_through)
            .filter(|message| {
                allows_steer_message(self.policy, &self.channel_roles, &message.from, &message.to)
            })
            .map(|message| SteeredMail {
                from: message.from.clone(),
                body: message.body.clone(),
                channel: message.to.channel().map(str::to_string),
            })
            .collect();
        self.through = self.committed_through.max(inbox.len() as u64);
        Ok(consumed_incoming)
    }

    fn reaches(&self, to: &MailEndpoint) -> bool {
        match to {
            MailEndpoint::Handle(handle) => handle == &self.handle,
            MailEndpoint::Channel(channel) => self.channels.contains(channel),
        }
    }

    /// Prepare a notice without acknowledging delivery.
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
            hya_tool::extension::ExtensionEvent::MailConsumed {
                session: root,
                handle,
                through,
            }
            .into_event(),
            consumed_incoming,
        ));
        self.committed_through = through;
        if shown.is_empty() {
            return Ok(None);
        }
        Ok(Some(format_steer_notice(&shown)))
    }
}
