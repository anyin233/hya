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
use hya_proto::{ActorClaim, ChannelKind, MailEndpoint, MailKind, SessionId, ToolSchema};
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
    /// Deliver private mail to a vertical peer (parent or direct child).
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
    /// Post a one-way announcement on the unit's group channel.
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
    /// List the acting agent's channels (group pipes + live-peer DMs).
    ListChannels {
        /// Acting session.
        session: SessionId,
        /// Host reply with channel rows.
        reply: oneshot::Sender<Result<Vec<ChannelRow>, String>>,
    },
    /// Search the caller's own archived direct children by handoff digest.
    SearchAgents {
        /// Acting session.
        session: SessionId,
        /// Free-text query over goal/state/pending digests.
        query: String,
        /// Host reply with archive rows.
        reply: oneshot::Sender<Result<Vec<ArchivedAgentRow>, String>>,
    },
}

/// One row of `list_channel`: what the acting agent can see. Group channels
/// never expose a member list (ADR-0016); DM channels always name their peer.
#[derive(Clone, Debug)]
pub struct ChannelRow {
    /// Channel id (`announce-{8}` / `DM-{8}`).
    pub id: String,
    /// Group broadcast pipe or DM pair.
    pub kind: ChannelKind,
    /// Whether the acting agent may post (group: unit leader only; dm: yes).
    pub can_speak: bool,
    /// DM peer identity; `None` for group channels.
    pub peer: Option<String>,
    /// Unread message count for the acting agent.
    pub unread: usize,
}

/// One row of `search_agent`: an archived direct child's handoff digest.
#[derive(Clone, Debug)]
pub struct ArchivedAgentRow {
    /// Canonical handle the agent had while live.
    pub handle: String,
    /// Agent type / stable id.
    pub agent_type: String,
    /// Session id string.
    pub session: String,
    /// `goal` section digest of the latest handoff.
    pub goal: String,
    /// `pending tasks` section digest.
    pub pending: String,
    /// Whether the handoff was degraded.
    pub degraded: bool,
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

    /// Send private mail to a vertical peer (ADR-0016 dm).
    pub async fn dm(&self, to: MailEndpoint, body: String) -> Result<MailReceipt, MailboxError> {
        self.send(to, MailKind::Message, body).await
    }

    /// Post a one-way announcement on the unit's group channel.
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

    /// The acting agent's channels: group pipes plus live-peer DMs.
    pub async fn list_channels(&self) -> Result<Vec<ChannelRow>, MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::ListChannels { session, reply })
            .await?
            .map_err(MailboxError::Rejected)
    }

    /// Search the caller's own archived direct children.
    pub async fn search_agents(
        &self,
        query: String,
    ) -> Result<Vec<ArchivedAgentRow>, MailboxError> {
        let session = self.session.ok_or(MailboxError::Unavailable)?;
        self.request(|reply| MailboxRequest::SearchAgents {
            session,
            query,
            reply,
        })
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

pub(crate) struct DmTool;

#[derive(Deserialize)]
struct DmInput {
    #[serde(default)]
    to: String,
    body: String,
}

#[async_trait]
impl Tool for DmTool {
    fn name(&self) -> &str {
        "dm"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "dm",
            "Send a private message over your DM channel with one vertical peer. \
             As a subordinate this reaches your parent (the only peer you have); \
             as a leader name one of your direct children. Mail to an archived \
             child revives it with its saved state. Siblings are not addressable.",
            json!({
                "to": {"type": "string", "description": "A direct child's handle (leaders); omit to reach your parent"},
                "body": {"type": "string", "description": "The message body"}
            }),
            &["body"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: DmInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.body.trim().is_empty() {
            return Err(ToolError::Input("message body is empty".to_string()));
        }
        let to = if input.to.trim().is_empty() {
            // Subordinate default: the parent — the only vertical peer upward.
            MailEndpoint::Handle("^parent".to_string())
        } else {
            MailEndpoint::Handle(input.to.trim().to_string())
        };
        let receipt = ctx.mailbox.dm(to, input.body).await.map_err(map_err)?;
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

pub(crate) struct BroadcastTool;

#[derive(Deserialize)]
struct BroadcastInput {
    body: String,
}

#[async_trait]
impl Tool for BroadcastTool {
    fn name(&self) -> &str {
        "broadcast"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "broadcast",
            "Post a one-way announcement on your unit's group channel: every \
             agent you directly lead hears it, and nobody further down. Your \
             reports answer with ordinary `dm` mail to you.",
            json!({
                "body": {"type": "string", "description": "The announcement body"}
            }),
            &["body"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: BroadcastInput =
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

pub(crate) struct ListChannelTool;

#[async_trait]
impl Tool for ListChannelTool {
    fn name(&self) -> &str {
        "list_channel"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "list_channel",
            "List your channels: the group broadcast pipes you can hear (and \
             post to, when you lead the unit) and your DM channels with live \
             peers and their unread counts. Group channels never list members.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let rows = ctx.mailbox.list_channels().await.map_err(map_err)?;
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| {
                let kind = if row.kind == ChannelKind::Group {
                    "group"
                } else {
                    "dm"
                };
                match &row.peer {
                    Some(peer) => format!(
                        "  #{} · {kind} · peer {peer} · {} unread",
                        row.id, row.unread
                    ),
                    None => format!(
                        "  #{} · {kind} · {} · {} unread",
                        row.id,
                        if row.can_speak {
                            "you can post"
                        } else {
                            "listen only"
                        },
                        row.unread
                    ),
                }
            })
            .collect();
        let output = if rendered.is_empty() {
            "You have no channels yet.".to_string()
        } else {
            rendered.join("\n")
        };
        Ok(json!({
            "title": format!("{} channel(s)", rows.len()),
            "output": output,
            "channels": rows.iter().map(|row| json!({
                "id": row.id,
                "kind": if row.kind == ChannelKind::Group { "group" } else { "dm" },
                "can_speak": row.can_speak,
                "peer": row.peer,
                "unread": row.unread,
            })).collect::<Vec<_>>(),
        }))
    }
}

pub(crate) struct SearchAgentTool;

#[derive(Deserialize)]
struct SearchAgentInput {
    #[serde(default)]
    query: String,
}

#[async_trait]
impl Tool for SearchAgentTool {
    fn name(&self) -> &str {
        "search_agent"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "search_agent",
            "Search YOUR OWN archived direct subagents by their final state \
             handoff (goal / current state / pending). Returns each agent's \
             handle and digest; `dm` that handle to revive the agent with its \
             saved state.",
            json!({
                "query": {"type": "string", "description": "Free-text query over goal/state/pending digests; empty lists all"}
            }),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SearchAgentInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let rows = ctx
            .mailbox
            .search_agents(input.query)
            .await
            .map_err(map_err)?;
        let rendered: Vec<String> = rows
            .iter()
            .map(|row| {
                format!(
                    "  {} ({}) · goal: {} · pending: {}{}",
                    row.handle,
                    row.agent_type,
                    row.goal,
                    row.pending,
                    if row.degraded {
                        " · degraded handoff"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        let output = if rendered.is_empty() {
            "No archived agents match.".to_string()
        } else {
            rendered.join("\n")
        };
        Ok(json!({
            "title": format!("{} archived agent(s)", rows.len()),
            "output": output,
            "agents": rows.iter().map(|row| json!({
                "handle": row.handle,
                "agent_type": row.agent_type,
                "session": row.session,
                "goal": row.goal,
                "pending": row.pending,
                "degraded": row.degraded,
            })).collect::<Vec<_>>(),
        }))
    }
}
