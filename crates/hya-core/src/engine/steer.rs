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

use hya_proto::{Envelope, Event, MailEndpoint, SessionId};

use super::SessionEngine;
use crate::error::CoreError;

/// One steered message: sender and body.
#[derive(Clone, Debug)]
pub(super) struct SteeredMail {
    /// Sender canonical handle.
    pub from: String,
    /// Message body.
    pub body: String,
}

/// Per-turn steer state: the backlog plus the live bus tail.
pub(crate) struct SteerMailbox {
    root: SessionId,
    handle: String,
    queue: Vec<SteeredMail>,
    /// Durable cursor covered so far; `MailConsumed.through` values.
    through: u64,
    /// Bus subscription for live deliveries.
    bus: tokio::sync::broadcast::Receiver<Envelope>,
    /// Channels the acting handle is a member of (for channel fan-out).
    channels: Vec<String>,
}

impl SessionEngine {
    /// Snapshot the durable unread backlog and subscribe to live mail.
    ///
    /// Never fails the turn: a session outside a team gets an empty, inert
    /// mailbox (every drain returns nothing).
    pub(crate) async fn steer_mailbox_snapshot(&self, session: SessionId) -> SteerMailbox {
        let bus = self.bus().subscribe();
        let Ok(root) = self.team_root(session).await else {
            return SteerMailbox {
                root: session,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                bus,
                channels: Vec::new(),
            };
        };
        let Ok(handle) = self.resolve_handle(root, session).await else {
            return SteerMailbox {
                root,
                handle: String::new(),
                queue: Vec::new(),
                through: 0,
                bus,
                channels: Vec::new(),
            };
        };
        let Ok(projection) = self.read_projection(root).await else {
            return SteerMailbox {
                root,
                handle: handle.clone(),
                queue: Vec::new(),
                through: 0,
                bus,
                channels: Vec::new(),
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
        let queue: Vec<SteeredMail> = projection
            .team
            .inboxes
            .get(&handle)
            .map(|inbox| {
                inbox
                    .iter()
                    .skip(usize::try_from(cursor).unwrap_or(usize::MAX))
                    .map(|message| SteeredMail {
                        from: message.from.clone(),
                        body: message.body.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        SteerMailbox {
            root,
            handle,
            queue,
            through: cursor,
            bus,
            channels,
        }
    }
}

impl SteerMailbox {
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
                        self.queue.push(SteeredMail {
                            from: from.clone(),
                            body: body.clone(),
                        });
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

    /// Drain pending mail into a notice appended to a tool result, advancing
    /// the durable cursor via `MailConsumed`. `None` when nothing is pending.
    pub(crate) async fn drain(
        &mut self,
        engine: &SessionEngine,
    ) -> Result<Option<String>, CoreError> {
        self.poll_live();
        if self.handle.is_empty() || self.queue.is_empty() {
            return Ok(None);
        }
        let shown: Vec<SteeredMail> = std::mem::take(&mut self.queue);
        self.through = self.through.saturating_add(shown.len() as u64);
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
        let mut notice = String::from("\n\n--- [NEW MAIL · answer or acknowledge via dm] ---");
        for mail in shown {
            let body = if mail.body.len() > 600 {
                format!("{}…", &mail.body[..600])
            } else {
                mail.body.clone()
            };
            notice.push_str(&format!("\n[mail from {}] {}", mail.from, body));
        }
        notice.push_str("\n(history: read channel://<id>?last=N · unread overview: list_channel)");
        Ok(Some(notice))
    }
}
