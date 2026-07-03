//! Frontend team/mailbox read-model (ADR-0001 bridge).
//!
//! The backend folds mail/channel/roster events into `hya_proto::TeamProjection`,
//! but the TUI renders from the SEPARATE frontend [`crate::MessageStore`], which
//! consumes the compat SSE stream. Team events reach the frontend wrapped in a
//! `hya.envelope` global event whose `properties` is the raw backend `Envelope`
//! (`{ "seq", "ts_millis", "event": { "type": ..., .. } }`). This module folds the
//! inner `event` into a faithful mirror of `hya_proto::TeamProjection` so the
//! roster/channel/inbox views can render.
//!
//! It is a READ-MODEL only: the fold logic mirrors the backend reducer arm-for-arm
//! (see `hya-proto/src/projection.rs`) so replay and the live stream converge. No
//! divergent logic lives here — recognized team events mutate the projection, all
//! else is ignored. Enum-valued fields (`mode`/`status`, `MailKind`) are kept as
//! their wire strings because the frontend only displays them.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// A mail address: a single agent's handle or a named channel (no leading `#`).
///
/// Mirrors `hya_proto::MailEndpoint`, which serializes adjacently tagged as
/// `{"kind":"handle","id":..}` / `{"kind":"channel","id":..}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailEndpoint {
    /// A single agent addressed by its team-scoped handle.
    Handle(String),
    /// A named channel; every current subscriber received the message.
    Channel(String),
}

impl MailEndpoint {
    /// Decode the wire form `{"kind":"handle"|"channel","id":".."}`.
    #[must_use]
    pub fn from_value(value: &Value) -> Option<Self> {
        let id = value.get("id").and_then(Value::as_str)?.to_owned();
        match value.get("kind").and_then(Value::as_str)? {
            "handle" => Some(MailEndpoint::Handle(id)),
            "channel" => Some(MailEndpoint::Channel(id)),
            _ => None,
        }
    }

    /// The channel id if this is a channel address, else `None`.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        match self {
            MailEndpoint::Channel(name) => Some(name),
            MailEndpoint::Handle(_) => None,
        }
    }

    /// The handle if this is a direct address, else `None`.
    #[must_use]
    pub fn handle(&self) -> Option<&str> {
        match self {
            MailEndpoint::Handle(name) => Some(name),
            MailEndpoint::Channel(_) => None,
        }
    }
}

/// A single delivered message folded into an inbox / channel log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailMessage {
    /// Sender handle.
    pub from: String,
    /// Recipient address (handle or channel).
    pub to: MailEndpoint,
    /// Wire kind: `message` (default) or `announcement`.
    pub kind: String,
    /// Message body.
    pub body: String,
}

/// One channel: current subscribers plus the ordered log of everything posted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelProjection {
    /// Current subscriber handles.
    pub members: BTreeSet<String>,
    /// Ordered log of every message posted to the channel.
    pub log: Vec<MailMessage>,
}

/// A live team member: handle, its own session id, declared type, and the
/// resident-lifecycle enrichment (mode/status/current-task) the roster shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    /// Stable team-scoped handle (also the TUI label).
    pub handle: String,
    /// The member's own session id, used to open a read-only pane.
    pub session: String,
    /// Declared agent type (e.g. `reviewer`); empty when unknown.
    pub agent_type: String,
    /// Wire mode string: `transient` (default) or `resident`.
    pub mode: String,
    /// Wire status string: `idle` (default), `busy`, `done`, or `failed`.
    pub status: String,
    /// Short human-facing description of the current task, if any.
    pub current_task: Option<String>,
}

/// Team-scoped mailbox/channel/roster read-model, folded from `hya.envelope`
/// team events. Empty until the first team event arrives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamProjection {
    /// Per-agent inbox keyed by handle, in delivery order.
    pub inboxes: BTreeMap<String, Vec<MailMessage>>,
    /// Channels keyed by name (no leading `#`).
    pub channels: BTreeMap<String, ChannelProjection>,
    /// Live roster keyed by handle.
    pub roster: BTreeMap<String, RosterEntry>,
}

impl TeamProjection {
    /// Whether no team state has been folded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inboxes.is_empty() && self.channels.is_empty() && self.roster.is_empty()
    }

    /// Fold one backend `event` object (the inner `event` of a `hya.envelope`
    /// envelope). Returns `true` when the event was a recognized team event.
    ///
    /// Mirrors the `hya_proto::Projection` reducer arms for `AgentRegistered`,
    /// `AgentActivityChanged`, `ChannelJoined`, `ChannelLeft`, and `MailSent`.
    pub fn apply_event(&mut self, event: &Value) -> bool {
        let Some(kind) = event.get("type").and_then(Value::as_str) else {
            return false;
        };
        match kind {
            "agent_registered" => self.apply_agent_registered(event),
            "agent_activity_changed" => self.apply_agent_activity_changed(event),
            "channel_joined" => self.apply_channel_joined(event),
            "channel_left" => self.apply_channel_left(event),
            "mail_sent" => self.apply_mail_sent(event),
            _ => false,
        }
    }

    fn apply_agent_registered(&mut self, event: &Value) -> bool {
        let Some(handle) = str_field(event, "handle") else {
            return false;
        };
        let session = str_field(event, "agent_session")
            .unwrap_or_default()
            .to_owned();
        let agent_type = str_field(event, "agent_type")
            .unwrap_or_default()
            .to_owned();
        let mode = str_field(event, "mode").unwrap_or("transient").to_owned();
        // Re-registering a handle preserves its live status/current_task (roster is
        // keyed by handle) while refreshing the binding + mode — matching the reducer.
        let entry = self
            .roster
            .entry(handle.to_owned())
            .or_insert_with(|| RosterEntry {
                handle: handle.to_owned(),
                session: session.clone(),
                agent_type: agent_type.clone(),
                mode: mode.clone(),
                status: "idle".to_owned(),
                current_task: None,
            });
        entry.session = session;
        entry.agent_type = agent_type;
        entry.mode = mode;
        true
    }

    fn apply_agent_activity_changed(&mut self, event: &Value) -> bool {
        let Some(handle) = str_field(event, "handle") else {
            return false;
        };
        let Some(entry) = self.roster.get_mut(handle) else {
            return false;
        };
        if let Some(status) = str_field(event, "status") {
            entry.status = status.to_owned();
        }
        // The reducer only overwrites the task when the event carries one.
        if let Some(task) = str_field(event, "current_task") {
            entry.current_task = Some(task.to_owned());
        }
        true
    }

    fn apply_channel_joined(&mut self, event: &Value) -> bool {
        let (Some(channel), Some(member)) =
            (str_field(event, "channel"), str_field(event, "member"))
        else {
            return false;
        };
        self.channels
            .entry(channel.to_owned())
            .or_default()
            .members
            .insert(member.to_owned());
        true
    }

    fn apply_channel_left(&mut self, event: &Value) -> bool {
        let (Some(channel), Some(member)) =
            (str_field(event, "channel"), str_field(event, "member"))
        else {
            return false;
        };
        if let Some(ch) = self.channels.get_mut(channel) {
            ch.members.remove(member);
        }
        true
    }

    fn apply_mail_sent(&mut self, event: &Value) -> bool {
        let Some(from) = str_field(event, "from") else {
            return false;
        };
        let Some(to) = event.get("to").and_then(MailEndpoint::from_value) else {
            return false;
        };
        let kind = str_field(event, "kind").unwrap_or("message").to_owned();
        let body = str_field(event, "body").unwrap_or_default().to_owned();
        let message = MailMessage {
            from: from.to_owned(),
            to: to.clone(),
            kind,
            body,
        };
        match &to {
            MailEndpoint::Handle(handle) => {
                self.inboxes
                    .entry(handle.clone())
                    .or_default()
                    .push(message);
            }
            MailEndpoint::Channel(channel) => {
                let channel_state = self.channels.entry(channel.clone()).or_default();
                channel_state.log.push(message.clone());
                // Fan out to every CURRENT subscriber; snapshot the member set first
                // so the inbox borrow does not alias the channel.
                let members: Vec<String> = channel_state.members.iter().cloned().collect();
                for member in members {
                    self.inboxes
                        .entry(member)
                        .or_default()
                        .push(message.clone());
                }
            }
        }
        true
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn team_event(event: Value) -> Value {
        // The inner `event` object as carried in a hya.envelope's `properties.event`.
        event
    }

    #[test]
    fn agent_registered_adds_roster_entry() {
        let mut team = TeamProjection::default();
        assert!(team.apply_event(&team_event(json!({
            "type": "agent_registered",
            "session": "ses_root",
            "agent_session": "ses_child",
            "handle": "reviewer-3",
            "agent_type": "reviewer",
            "mode": "resident",
        }))));
        let entry = team.roster.get("reviewer-3").expect("roster entry");
        assert_eq!(entry.session, "ses_child");
        assert_eq!(entry.agent_type, "reviewer");
        assert_eq!(entry.mode, "resident");
        assert_eq!(entry.status, "idle");
    }

    #[test]
    fn activity_change_updates_status_and_preserves_on_reregister() {
        let mut team = TeamProjection::default();
        team.apply_event(&json!({
            "type": "agent_registered", "session": "ses_root",
            "agent_session": "ses_child", "handle": "r1", "agent_type": "reviewer", "mode": "resident",
        }));
        team.apply_event(&json!({
            "type": "agent_activity_changed", "session": "ses_root",
            "handle": "r1", "status": "busy", "current_task": "reviewing",
        }));
        assert_eq!(team.roster["r1"].status, "busy");
        assert_eq!(team.roster["r1"].current_task.as_deref(), Some("reviewing"));
        // Re-registering keeps live status/current_task.
        team.apply_event(&json!({
            "type": "agent_registered", "session": "ses_root",
            "agent_session": "ses_child", "handle": "r1", "agent_type": "reviewer", "mode": "resident",
        }));
        assert_eq!(team.roster["r1"].status, "busy");
        assert_eq!(team.roster["r1"].current_task.as_deref(), Some("reviewing"));
    }

    #[test]
    fn channel_message_fans_out_to_current_members() {
        let mut team = TeamProjection::default();
        team.apply_event(&json!({ "type": "channel_joined", "session": "ses_root", "channel": "build", "member": "a" }));
        team.apply_event(&json!({ "type": "channel_joined", "session": "ses_root", "channel": "build", "member": "b" }));
        assert!(team.apply_event(&json!({
            "type": "mail_sent", "session": "ses_root", "from": "a",
            "to": { "kind": "channel", "id": "build" }, "kind": "announcement", "body": "ship it",
        })));
        assert_eq!(team.channels["build"].log.len(), 1);
        assert_eq!(team.inboxes["a"].len(), 1);
        assert_eq!(team.inboxes["b"].len(), 1);
        assert_eq!(team.inboxes["b"][0].body, "ship it");
        assert_eq!(team.inboxes["b"][0].kind, "announcement");
    }

    #[test]
    fn direct_mail_lands_in_handle_inbox_only() {
        let mut team = TeamProjection::default();
        assert!(team.apply_event(&json!({
            "type": "mail_sent", "session": "ses_root", "from": "a",
            "to": { "kind": "handle", "id": "b" }, "body": "hi",
        })));
        assert_eq!(team.inboxes["b"].len(), 1);
        assert_eq!(team.inboxes["b"][0].kind, "message");
        assert!(!team.inboxes.contains_key("a"));
    }

    #[test]
    fn channel_left_removes_member_from_fanout() {
        let mut team = TeamProjection::default();
        team.apply_event(&json!({ "type": "channel_joined", "session": "ses_root", "channel": "build", "member": "a" }));
        team.apply_event(&json!({ "type": "channel_left", "session": "ses_root", "channel": "build", "member": "a" }));
        team.apply_event(&json!({
            "type": "mail_sent", "session": "ses_root", "from": "x",
            "to": { "kind": "channel", "id": "build" }, "body": "hello",
        }));
        assert_eq!(
            team.channels["build"].log.len(),
            1,
            "channel log keeps the post"
        );
        assert!(
            !team.inboxes.contains_key("a"),
            "unsubscribed member gets no fanout"
        );
    }

    #[test]
    fn non_team_event_is_ignored() {
        let mut team = TeamProjection::default();
        assert!(!team.apply_event(&json!({ "type": "message_started", "session": "ses_1" })));
        assert!(team.is_empty());
    }
}
