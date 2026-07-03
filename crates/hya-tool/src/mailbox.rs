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
use hya_proto::{MailEndpoint, MailKind, RosterEntry, SessionId, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::tool::{Tool, ToolCtx, ToolError, obj_schema};

/// Outcome of a delivered send: the resolved sender handle, the address, and how
/// many inboxes it reached (1 for a handle, the subscriber count for a channel).
#[derive(Clone, Debug)]
pub struct MailReceipt {
    pub from: String,
    pub to: MailEndpoint,
    pub recipients: usize,
}

/// A channel plus its current membership, for the `channels` tool.
#[derive(Clone, Debug)]
pub struct ChannelInfo {
    pub name: String,
    pub members: Vec<String>,
    pub messages: usize,
}

/// A request from a comms tool to the mailbox service. `reply` carries either the
/// typed result or a human-readable rejection string (the service maps its typed
/// errors to strings so this enum stays free of `hya-core` types).
pub enum MailboxRequest {
    Send {
        from: SessionId,
        to: MailEndpoint,
        kind: MailKind,
        body: String,
        reply: oneshot::Sender<Result<MailReceipt, String>>,
    },
    Join {
        session: SessionId,
        channel: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Leave {
        session: SessionId,
        channel: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Roster {
        session: SessionId,
        reply: oneshot::Sender<Result<Vec<RosterEntry>, String>>,
    },
    Channels {
        session: SessionId,
        reply: oneshot::Sender<Result<Vec<ChannelInfo>, String>>,
    },
}

#[derive(Debug, Error)]
pub enum MailboxError {
    #[error("mailbox service unavailable")]
    Unavailable,
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
        let mut plane = self.clone();
        plane.session = Some(session);
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
            channel,
            reply,
        })
        .await?
        .map_err(MailboxError::Rejected)
    }

    /// List the live roster for the acting agent's team.
    pub async fn roster(&self) -> Result<Vec<RosterEntry>, MailboxError> {
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
            "Send mail to a teammate by handle (e.g. `reviewer-3`) or to a channel (prefix with `#`, e.g. `#build`). Channel mail reaches every current subscriber.",
            json!({
                "to": {"type": "string", "description": "Recipient: a teammate handle, or a #channel name"},
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
            "List the live members of your team: each teammate's handle, agent type, and session.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let roster = ctx.mailbox.roster().await.map_err(map_err)?;
        let members: Vec<Value> = roster
            .iter()
            .map(|entry| {
                json!({
                    "handle": entry.handle,
                    "type": entry.agent_type.as_str(),
                    "session": entry.session.to_string(),
                    // Live status / current task land with the resident lifecycle
                    // (Phase 4); everything in the roster today is an active member.
                    "status": "active",
                })
            })
            .collect();
        let output = if members.is_empty() {
            "No teammates registered yet.".to_string()
        } else {
            roster
                .iter()
                .map(|e| format!("{} ({})", e.handle, e.agent_type.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(json!({
            "title": format!("{} teammate(s)", members.len()),
            "output": output,
            "members": members,
        }))
    }
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
            "List your team's channels and their current members.",
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
                .map(|ch| format!("#{} ({} member(s))", ch.name, ch.members.len()))
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
            "Subscribe to a channel so you receive its mail. The channel is created if it does not exist.",
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
