//! Channel send and listing tool implementations.
use async_trait::async_trait;
use hya_proto::{ChannelKind, MailEndpoint, MailKind, ToolSchema};
use hya_tool::tool::obj_schema;
use hya_tool::{MailboxError, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

fn map_err(err: MailboxError) -> ToolError {
    match err {
        MailboxError::Unavailable => {
            ToolError::Other("mailbox is only available inside a running team".to_string())
        }
        MailboxError::Rejected(message) => ToolError::Other(message),
    }
}

pub struct SendTool;

#[derive(Deserialize)]
struct SendInput {
    /// Channel address: `#channel`, a bare channel id (`DM-…`/`announce-…`),
    /// a vertical peer handle, or `^parent`. Omit to use the default channel.
    #[serde(default, alias = "to")]
    channel: String,
    body: String,
}

/// Resolve the input's channel spelling to an endpoint; `None` means the
/// sender's role-default channel.
fn send_endpoint(raw: &str) -> Option<MailEndpoint> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw == "^parent" || raw == "^" {
        return Some(MailEndpoint::Handle("^parent".to_string()));
    }
    if let Some(channel) = raw.strip_prefix('#') {
        return Some(MailEndpoint::Channel(channel.to_string()));
    }
    if raw.starts_with("DM-") || raw.starts_with("announce-") {
        return Some(MailEndpoint::Channel(raw.to_string()));
    }
    Some(MailEndpoint::Handle(raw.to_string()))
}

fn send_address(endpoint: &MailEndpoint) -> String {
    match endpoint {
        MailEndpoint::Handle(handle) => handle.clone(),
        MailEndpoint::Channel(channel) => format!("#{channel}"),
    }
}

#[async_trait]
impl Tool for SendTool {
    fn name(&self) -> &str {
        "send"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "send",
            "Send a message on one channel; the channel's own nature decides the \
             delivery. `#channel` (or a bare `DM-…`/`announce-…` id from \
             `list_channel`) posts on that channel: group channels broadcast to \
             the unit (leader-only), DM channels stay private. A bare handle \
             sends private mail to that vertical peer (`^parent` for your \
             parent; mail to an archived child revives it). Omit `channel` to \
             use your default: the unit you lead, or your parent.",
            json!({
                "channel": {"type": "string", "description": "`#channel`, a channel id, a peer handle, or `^parent`; omit for the default"},
                "body": {"type": "string", "description": "The message body"}
            }),
            &["body"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let input: SendInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        if input.body.trim().is_empty() {
            return Err(ToolError::Input("message body is empty".to_string()));
        }
        let receipt = match send_endpoint(&input.channel) {
            Some(endpoint) => {
                ctx.mailbox
                    .send(endpoint, MailKind::Message, input.body)
                    .await
            }
            None => ctx.mailbox.send_default(input.body).await,
        }
        .map_err(map_err)?;
        let address = send_address(&receipt.to);
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

pub struct ListChannelTool;

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
             peers and their unread counts. Group channels never list members. \
             When you lead agents, a team section follows with each direct \
             child's live status and how long ago the harness last saw it make \
             progress (heartbeat) — a busy child with a stale heartbeat may be \
             stalled, a fresh one is progressing.",
            json!({}),
            &[],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        let rows = ctx.mailbox.list_channels().await.map_err(map_err)?;
        let team = ctx.mailbox.team_status().await.map_err(map_err)?;
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
        // Team section: the caller's direct children and their harness
        // heartbeat freshness (ADR-0002 liveness). Omitted entirely when the
        // caller leads nobody.
        let team_lines: Vec<String> = team
            .iter()
            .map(|row| {
                let freshness = match row.last_active_seconds {
                    Some(seconds) => format!("last active {seconds}s ago (heartbeat)"),
                    None => "no heartbeat yet".to_string(),
                };
                format!(
                    "  {} · {} · {freshness}",
                    hya_proto::scope::leaf(&row.handle),
                    row.status
                )
            })
            .collect();
        let mut lines = rendered;
        if team_lines.is_empty() {
            if lines.is_empty() {
                lines.push("You have no channels yet.".to_string());
            }
        } else {
            if !lines.is_empty() {
                lines.push(String::new());
            }
            lines.extend(team_lines);
        }
        let output = lines.join("\n");
        let mut result = json!({
            "title": format!("{} channel(s)", rows.len()),
            "output": output,
            "channels": rows.iter().map(|row| json!({
                "id": row.id,
                "kind": if row.kind == ChannelKind::Group { "group" } else { "dm" },
                "can_speak": row.can_speak,
                "peer": row.peer,
                "unread": row.unread,
            })).collect::<Vec<_>>(),
        });
        if !team.is_empty() {
            result["team"] = Value::Array(
                team.iter()
                    .map(|row| {
                        json!({
                            "handle": row.handle,
                            "status": row.status,
                            "lastActiveSeconds": row.last_active_seconds,
                        })
                    })
                    .collect(),
            );
        }
        Ok(result)
    }
}
