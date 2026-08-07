//! Event-sourced mailbox/channel plane + the model-facing comms tools (ADR-0001).
//!
//! Mirrors the [`SpawnerPlane`](crate::spawn::SpawnerPlane) idiom: the plane is a
//! channel handle held on `ToolCtx`; the backing service (owned by `hya-core`,
//! which has the store + projection) receives requests, appends the relevant
//! `Event` to the team-root log, and replies. `hya-tool` never depends on
//! `hya-core`, so all engine access flows over this channel.
//!
//! Team scoping: every request carries the acting agent's `SessionId`. The
//! service resolves it to the team root (session lineage) and the acting handle,
//! so an agent can only see/address its own team (decision 6).

use async_trait::async_trait;
use hya_proto::{
    ActorClaim, MailEndpoint, MailKind, RosterEntry, RosterStatus, ScopedRoster, SessionId,
    ToolSchema,
};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

/// Outcome of a delivered send: the resolved sender handle, the address, and how
/// many inboxes it reached (1 for a handle, the subscriber count for a channel).
#[derive(Clone, Debug)]
pub struct MailReceipt {
    /// Resolved sender handle.
    pub from: String,
    /// Normalized recipient endpoint.
    pub to: MailEndpoint,
    /// Number of inboxes that received the mail.
    pub recipients: usize,
}

/// A channel plus its current membership, for the `channels` tool.
#[derive(Clone, Debug)]
pub struct ChannelInfo {
    /// Channel name without the leading `#` and without its unit qualifier.
    pub name: String,
    /// Canonical path of the unit that owns this channel. Two units may each own
    /// a channel of the same `name`; this is what distinguishes them.
    pub unit: String,
    /// Current member canonical paths.
    pub members: Vec<String>,
    /// Message count on the channel.
    pub messages: usize,
}

/// A request from a comms tool to the mailbox service. `reply` carries either the
/// typed result or a human-readable rejection string (the service maps its typed
/// errors to strings so this enum stays free of `hya-core` types).
pub enum MailboxRequest {
    /// Deliver mail to a handle or `#channel`.
    Send {
        /// Sending session.
        from: SessionId,
        /// Optional actor claim for the send.
        actor_claim: Option<ActorClaim>,
        /// Recipient endpoint.
        to: MailEndpoint,
        /// Message vs announcement.
        kind: MailKind,
        /// Body text.
        body: String,
        /// Host reply with receipt or rejection.
        reply: oneshot::Sender<Result<MailReceipt, String>>,
    },
    /// Subscribe (creating the channel if needed).
    Join {
        /// Acting session.
        session: SessionId,
        /// Optional actor claim.
        actor_claim: Option<ActorClaim>,
        /// Channel name.
        channel: String,
        /// Host reply.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Unsubscribe from a channel.
    Leave {
        /// Acting session.
        session: SessionId,
        /// Optional actor claim.
        actor_claim: Option<ActorClaim>,
        /// Channel name.
        channel: String,
        /// Host reply.
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Post a one-way announcement to the agents the sender leads.
    Announce {
        /// Sending session.
        from: SessionId,
        /// Optional actor claim for the send.
        actor_claim: Option<ActorClaim>,
        /// Announcement body.
        body: String,
        /// Host reply with receipt or rejection.
        reply: oneshot::Sender<Result<MailReceipt, String>>,
    },
    /// List the teammates the sender may address, grouped by relation.
    Roster {
        /// Acting session (scope resolution).
        session: SessionId,
        /// Host reply with the scoped roster.
        reply: oneshot::Sender<Result<ScopedRoster, String>>,
    },
    /// List channels for the team.
    Channels {
        /// Acting session.
        session: SessionId,
        /// Host reply with channel info.
        reply: oneshot::Sender<Result<Vec<ChannelInfo>, String>>,
    },
}

/// Mailbox plane or service failure.
#[derive(Debug, Error)]
pub enum MailboxError {
    /// Plane disconnected or no session bound.
    #[error("mailbox service unavailable")]
    Unavailable,
    /// Service rejected the request with a message.
    #[error("{0}")]
    Rejected(String),
}

/// Channel handle to the mailbox service, scoped to the acting session.
///
/// A default/`disconnected` plane has no channel; its operations return
/// [`MailboxError::Unavailable`]. This is what unit tests and engines without a
/// wired mailbox service carry, exactly like a `SpawnerPlane` with no session.
#[derive(Clone, Default)]
pub struct MailboxPlane {
    tx: Option<mpsc::UnboundedSender<MailboxRequest>>,
    session: Option<SessionId>,
    actor_claim: Option<ActorClaim>,
}

impl MailboxPlane {
    /// Build a connected plane plus the receiver the service loop drains.
    #[must_use]
    pub fn new() -> (Self, mpsc::UnboundedReceiver<MailboxRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: Some(tx),
                session: None,
                actor_claim: None,
            },
            rx,
        )
    }

    /// A plane with no backing service — every call is `Unavailable`.
    #[must_use]
    pub fn disconnected() -> Self {
        Self::default()
    }

    /// Bind the plane to the acting agent's session (set when building `ToolCtx`).
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        self.for_session_with_actor(session, None)
    }

    /// Bind session and optional actor claim used for fenced sends.
    #[must_use]
    pub fn for_session_with_actor(
        &self,
        session: SessionId,
        actor_claim: Option<ActorClaim>,
    ) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane.actor_claim = actor_claim;
        plane
    }

    async fn request<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> MailboxRequest,
    ) -> Result<T, MailboxError> {
        let tx = self.tx.as_ref().ok_or(MailboxError::Unavailable)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(make(reply_tx))
            .map_err(|_| MailboxError::Unavailable)?;
        reply_rx.await.map_err(|_| MailboxError::Unavailable)
    }

    /// Append a `MailSent` addressed to a handle or `#channel`.
    pub async fn send(
        &self,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
    ) -> Result<MailReceipt, MailboxError> {
        let from = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Send {
            from,
            actor_claim: self.actor_claim,
            to,
            kind,
            body,
            reply,
        })
        .await?
        .map_err(MailboxError::Rejected)
    }

    /// Subscribe the acting agent's handle to `channel`.
    pub async fn join(&self, channel: String) -> Result<(), MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Join {
            session,
            actor_claim: self.actor_claim,
            channel,
            reply,
        })
        .await?
        .map_err(MailboxError::Rejected)
    }

    /// Unsubscribe the acting agent's handle from `channel`.
    pub async fn leave(&self, channel: String) -> Result<(), MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Leave {
            session,
            actor_claim: self.actor_claim,
            channel,
            reply,
        })
        .await?
        .map_err(MailboxError::Rejected)
    }

    /// Post a one-way announcement to the agents the acting agent leads.
    pub async fn announce(&self, body: String) -> Result<MailReceipt, MailboxError> {
        let from = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Announce {
            from,
            actor_claim: self.actor_claim,
            body,
            reply,
        })
        .await?
        .map_err(MailboxError::Rejected)
    }

    /// The roster as the acting agent sees it, grouped by relation.
    pub async fn roster(&self) -> Result<ScopedRoster, MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Roster { session, reply })
            .await?
            .map_err(MailboxError::Rejected)
    }

    /// List channels + membership for the acting agent's team.
    pub async fn channels(&self) -> Result<Vec<ChannelInfo>, MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::Channels { session, reply })
            .await?
            .map_err(MailboxError::Rejected)
    }
}

fn map_err(err: MailboxError) -> ToolError {
    match err {
        MailboxError::Unavailable => {
            ToolError::Other("mailbox is only available inside a running team".to_string())
        }
        MailboxError::Rejected(message) => ToolError::Other(message),
    }
}

pub(crate) struct SendTool;

#[derive(Deserialize)]
struct SendInput {
    to: String,
    body: String,
    #[serde(default)]
    kind: String,
}

#[async_trait]
impl Tool for SendTool {
    fn name(&self) -> &str {
        "send"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "send",
            "Send mail to a teammate, or post to a channel (prefix with `#`).\n\n\
             You can only reach your own unit: your parent, the teammates who \
             share your parent, and the agents you lead. Anyone else must be \
             reached by asking your parent to pass it on. Run `roster` to see \
             who you can reach.\n\n\
             Name a teammate by its short name (`worker-1`) or its full path \
             (`main/lead-1/worker-1`). `#build` is your unit's channel; if you \
             lead agents, `#^build` is your parent's unit's channel instead.",
            json!({
                "to": {"type": "string", "description": "A teammate's short name or full path, or a #channel"},
                "body": {"type": "string", "description": "The message body"},
                "kind": {"type": "string", "enum": ["message", "announcement"], "description": "Message intent; defaults to message"}
            }),
            &["to", "body"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SendInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.body.trim().is_empty() {
            return Err(ToolError::Input("message body is empty".to_string()));
        }
        let to = MailEndpoint::parse(&input.to);
        let kind = MailKind::parse(&input.kind);
        let receipt = ctx
            .mailbox
            .send(to, kind, input.body)
            .await
            .map_err(map_err)?;
        let address = match &receipt.to {
            MailEndpoint::Handle(handle) => handle.clone(),
            MailEndpoint::Channel(channel) => format!("#{channel}"),
        };
        Ok(json!({
            "title": format!("Sent to {address}"),
            "output": format!(
                "Delivered from {} to {} ({} recipient{}).",
                receipt.from,
                address,
                receipt.recipients,
                if receipt.recipients == 1 { "" } else { "s" }
            ),
            "metadata": {
                "from": receipt.from,
                "to": address,
                "recipients": receipt.recipients,
            },
        }))
    }
}

pub(crate) struct RosterTool;

#[async_trait]
impl Tool for RosterTool {
    fn name(&self) -> &str {
        "roster"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "roster",
            "List the agents you can message, grouped by how they relate to you: \
             your parent, your peers (same parent), and your reports (agents you \
             lead). Nobody outside your unit is listed, because you cannot \
             message them directly.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let roster = ctx.mailbox.roster().await.map_err(map_err)?;
        Ok(render_roster(&roster))
    }
}

pub(crate) struct AnnounceTool;

#[derive(Deserialize)]
struct AnnounceInput {
    body: String,
}

#[async_trait]
impl Tool for AnnounceTool {
    fn name(&self) -> &str {
        "announce"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "announce",
            "Announce something to every agent you directly lead. One-way: they \
             cannot reply on this path, and they will answer with ordinary mail \
             to you if they need to.\n\n\
             It reaches your DIRECT reports only, not the agents they lead. To \
             reach further down, your reports must announce in turn.",
            json!({
                "body": {"type": "string", "description": "The announcement body"}
            }),
            &["body"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: AnnounceInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.body.trim().is_empty() {
            return Err(ToolError::Input("announcement body is empty".to_string()));
        }
        let receipt = ctx.mailbox.announce(input.body).await.map_err(map_err)?;
        Ok(json!({
            "title": format!("Announced to {} report(s)", receipt.recipients),
            "output": format!(
                "Announced from {} to {} direct report{}.",
                receipt.from,
                receipt.recipients,
                if receipt.recipients == 1 { "" } else { "s" }
            ),
            "metadata": {
                "from": receipt.from,
                "recipients": receipt.recipients,
            },
        }))
    }
}

/// Human-readable label for a teammate's live activity, folded into the roster
/// projection from `AgentActivityChanged` by the resident supervisor.
fn status_label(status: &RosterStatus) -> &'static str {
    match status {
        RosterStatus::Idle => "idle",
        RosterStatus::Busy => "busy",
        RosterStatus::Done => "done",
        RosterStatus::Failed => "failed",
    }
}

/// One roster row as the model sees it.
fn roster_row(entry: &RosterEntry, relation: &str) -> Value {
    json!({
        "handle": entry.handle,
        "name": hya_proto::scope::leaf(&entry.handle),
        "relation": relation,
        "type": entry.agent_type.as_str(),
        "session": entry.session.to_string(),
        "mode": entry.mode,
        "status": status_label(&entry.status),
        "current_task": entry.current_task,
    })
}

/// One human-readable roster line: short name first (that is what you address),
/// with the full path kept for disambiguation.
fn roster_line(entry: &RosterEntry) -> String {
    let mut line = format!(
        "  {} ({}) · {} · {}",
        hya_proto::scope::leaf(&entry.handle),
        entry.agent_type.as_str(),
        status_label(&entry.status),
        entry.handle,
    );
    if let Some(task) = entry.current_task.as_deref().filter(|t| !t.is_empty()) {
        line.push_str(" — ");
        line.push_str(task);
    }
    line
}

/// Render the `roster` payload grouped by relation, so the model reads its own
/// position in the org straight off the result. Empty groups are omitted rather
/// than shown as empty headings.
fn render_roster(roster: &ScopedRoster) -> Value {
    let mut rows = Vec::new();
    let mut sections: Vec<String> = Vec::new();

    if let Some(parent) = &roster.parent {
        rows.push(roster_row(parent, "parent"));
        sections.push(format!("parent:\n{}", roster_line(parent)));
    }
    for (label, group) in [("peers", &roster.peers), ("reports", &roster.reports)] {
        if group.is_empty() {
            continue;
        }
        for entry in group.iter() {
            rows.push(roster_row(entry, label.trim_end_matches('s')));
        }
        let lines: Vec<String> = group.iter().map(roster_line).collect();
        sections.push(format!("{label}:\n{}", lines.join("\n")));
    }

    let output = if sections.is_empty() {
        "You have no teammates yet: no parent, no peers, and no reports.".to_string()
    } else {
        format!("self: {}\n\n{}", roster.self_path, sections.join("\n\n"))
    };

    json!({
        "title": format!("{} teammate(s) in scope", rows.len()),
        "output": output,
        "self": roster.self_path,
        "members": rows,
    })
}

pub(crate) struct ChannelsTool;

#[async_trait]
impl Tool for ChannelsTool {
    fn name(&self) -> &str {
        "channels"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "channels",
            "List the channels you can use and their current members. A channel \
             belongs to one unit, so the same name can exist in another unit and \
             be a different channel; `unit` says which one this is.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let channels = ctx.mailbox.channels().await.map_err(map_err)?;
        let rows: Vec<Value> = channels
            .iter()
            .map(|ch| {
                json!({
                    "name": format!("#{}", ch.name),
                    "unit": ch.unit,
                    "members": ch.members,
                    "messages": ch.messages,
                })
            })
            .collect();
        let output = if channels.is_empty() {
            "No channels yet. Post to a #channel to create it.".to_string()
        } else {
            channels
                .iter()
                .map(|ch| {
                    format!(
                        "#{} ({} member(s)) · unit {}",
                        ch.name,
                        ch.members.len(),
                        ch.unit
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(json!({
            "title": format!("{} channel(s)", channels.len()),
            "output": output,
            "channels": rows,
        }))
    }
}

pub(crate) struct JoinTool;

#[derive(Deserialize)]
struct ChannelInput {
    channel: String,
}

#[async_trait]
impl Tool for JoinTool {
    fn name(&self) -> &str {
        "join"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "join",
            "Subscribe to a channel in your own unit so you receive its mail. \
             The channel is created if it does not exist. If you lead agents, a \
             bare name is your unit's channel and `^name` is your parent's \
             unit's.",
            json!({
                "channel": {"type": "string", "description": "Channel name (the leading # is optional; prefix ^ for your parent's unit)"}
            }),
            &["channel"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: ChannelInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let channel = normalize_channel(&input.channel)?;
        ctx.mailbox.join(channel.clone()).await.map_err(map_err)?;
        Ok(json!({
            "title": format!("Joined #{channel}"),
            "output": format!("You now receive mail on #{channel}."),
        }))
    }
}

pub(crate) struct LeaveTool;

#[async_trait]
impl Tool for LeaveTool {
    fn name(&self) -> &str {
        "leave"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "leave",
            "Unsubscribe from a channel; you stop receiving its mail.",
            json!({
                "channel": {"type": "string", "description": "Channel name (the leading # is optional)"}
            }),
            &["channel"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: ChannelInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let channel = normalize_channel(&input.channel)?;
        ctx.mailbox.leave(channel.clone()).await.map_err(map_err)?;
        Ok(json!({
            "title": format!("Left #{channel}"),
            "output": format!("You no longer receive mail on #{channel}."),
        }))
    }
}

/// Strip an optional leading `#` and reject an empty channel name.
fn normalize_channel(raw: &str) -> Result<String, ToolError> {
    let channel = raw.trim().strip_prefix('#').unwrap_or(raw.trim()).trim();
    if channel.is_empty() {
        return Err(ToolError::Input("channel name is empty".to_string()));
    }
    Ok(channel.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::{AgentName, SubagentMode};

    fn entry(path: &str, status: RosterStatus, task: Option<&str>) -> RosterEntry {
        RosterEntry {
            handle: path.to_string(),
            session: SessionId::new(),
            agent_type: AgentName::new("reviewer"),
            mode: SubagentMode::Resident,
            status,
            current_task: task.map(str::to_string),
            resident_cursor: 0,
            resident_work: None,
        }
    }

    #[test]
    fn render_roster_surfaces_live_status_mode_and_task() {
        let roster = ScopedRoster {
            self_path: "main/lead-1/worker-2".to_string(),
            parent: None,
            peers: vec![entry(
                "main/lead-1/reviewer-1",
                RosterStatus::Busy,
                Some("reviewing auth.rs"),
            )],
            reports: Vec::new(),
        };
        let value = render_roster(&roster);
        let member = &value["members"][0];
        assert_eq!(member["status"], "busy");
        assert_eq!(member["mode"], "resident");
        assert_eq!(member["current_task"], "reviewing auth.rs");
        assert_eq!(member["relation"], "peer");
        assert_eq!(
            member["name"], "reviewer-1",
            "the short name is what the model addresses"
        );
        let output = value["output"].as_str().unwrap_or_default();
        assert!(
            output.contains("reviewer-1 (reviewer) · busy"),
            "output was: {output}"
        );
        assert!(output.contains("reviewing auth.rs"), "output was: {output}");
    }

    /// Each group is labeled, and a group with no members is omitted rather than
    /// rendered as an empty heading (AC7).
    #[test]
    fn render_roster_groups_by_relation_and_omits_empty_groups() {
        let roster = ScopedRoster {
            self_path: "main/lead-1".to_string(),
            parent: Some(entry("main", RosterStatus::Idle, None)),
            peers: Vec::new(),
            reports: vec![
                entry("main/lead-1/worker-1", RosterStatus::Idle, None),
                entry("main/lead-1/worker-2", RosterStatus::Idle, None),
            ],
        };
        let value = render_roster(&roster);
        let output = value["output"].as_str().unwrap_or_default();

        assert!(output.contains("self: main/lead-1"), "output was: {output}");
        assert!(output.contains("parent:"), "output was: {output}");
        assert!(output.contains("reports:"), "output was: {output}");
        assert!(
            !output.contains("peers:"),
            "an empty group must be omitted, not shown empty: {output}"
        );
        assert_eq!(value["title"], "3 teammate(s) in scope");
        assert_eq!(value["members"][0]["relation"], "parent");
        assert_eq!(value["members"][1]["relation"], "report");
    }

    #[test]
    fn render_roster_reports_an_agent_that_can_reach_nobody() {
        let value = render_roster(&ScopedRoster {
            self_path: "main".to_string(),
            parent: None,
            peers: Vec::new(),
            reports: Vec::new(),
        });
        assert_eq!(value["title"], "0 teammate(s) in scope");
        assert_eq!(
            value["output"],
            "You have no teammates yet: no parent, no peers, and no reports."
        );
    }
}
