//! Model-facing tools exported by `hya/agent-channels`:
//! `send`, `list_channel`, `report`, and mail-aware `wait`.

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ChannelKind, MailEndpoint, MailKind, SessionId, ToolSchema};
use hya_runtime_abi::{HostError, HostRuntime};
use hya_tool::tool::obj_schema;
use hya_tool::{LifecyclePlane, ReportOutcome, Tool, ToolCtx, ToolError};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::service::{AgentChannelsService, host_error_to_tool_error};

/// The started service's host runtime and the calling session.
fn team_session(
    service: &AgentChannelsService,
    ctx: &ToolCtx,
) -> Result<(Arc<dyn HostRuntime>, SessionId), ToolError> {
    let session = ctx
        .session
        .ok_or_else(|| HostError::Unavailable("no team session".to_string()));
    service
        .runtime()
        .and_then(|runtime| Ok((runtime, session?)))
        .map_err(host_error_to_tool_error)
}

/// Send mail on a channel, peer DM, or default role channel (`send`).
pub struct SendTool {
    service: Arc<AgentChannelsService>,
}

impl SendTool {
    /// Create a `send` tool backed by `service`.
    #[must_use]
    pub fn new(service: Arc<AgentChannelsService>) -> Self {
        Self { service }
    }
}

#[derive(Deserialize)]
struct SendInput {
    #[serde(default)]
    channel: String,
    body: String,
}

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
        let (runtime, session) = team_session(&self.service, ctx)?;
        let policy = runtime
            .turn_runner()
            .session_channel_policy(session)
            .await
            .map_err(host_error_to_tool_error)?
            .ok_or_else(|| {
                ToolError::Input("channel policy snapshot missing for admitted send".to_string())
            })?;
        let claim = ctx.operation.actor_claim();
        let receipt = match send_endpoint(&input.channel) {
            Some(endpoint) => {
                AgentChannelsService::send(
                    runtime.as_ref(),
                    session,
                    endpoint,
                    MailKind::Message,
                    input.body,
                    claim,
                    Some(policy),
                    false,
                )
                .await
            }
            None => {
                AgentChannelsService::send_default(
                    runtime.as_ref(),
                    session,
                    input.body,
                    claim,
                    Some(policy),
                )
                .await
            }
        }
        .map_err(host_error_to_tool_error)?;
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

/// List visible channels and direct-child liveness (`list_channel`).
pub struct ListChannelTool {
    service: Arc<AgentChannelsService>,
}

impl ListChannelTool {
    /// Create a `list_channel` tool backed by `service`.
    #[must_use]
    pub fn new(service: Arc<AgentChannelsService>) -> Self {
        Self { service }
    }
}

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
        let (runtime, session) = team_session(&self.service, ctx)?;
        let rows = AgentChannelsService::list_channels(runtime.as_ref(), session)
            .await
            .map_err(host_error_to_tool_error)?;
        let team = AgentChannelsService::team_status(runtime.as_ref(), session)
            .await
            .map_err(host_error_to_tool_error)?;
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

/// Terminal subagent report tool (`report`, ADR-0015).
pub struct ReportTool;

#[async_trait]
impl Tool for ReportTool {
    fn name(&self) -> &str {
        "report"
    }

    fn schema(&self) -> ToolSchema {
        obj_schema(
            "report",
            "Submit your terminal report; it takes effect when this turn ends. The engine checks you have no unread mail and no live children (answer or archive them first). Submission ends your turn right after this tool round (no further model call). The engine then rechecks the gate, writes your state handoff, delivers the report to your parent, and archives you. If new mail, a live subagent, or a changed episode prevents archive, you receive a harness notice and must report again after handling it. Call it once per turn. Ending your turn with a final answer and no `report` submits that answer the same way with outcome `done`. Mail from your parent can wake you after archive for a new episode with that handoff context.",
            json!({
                "result": {
                    "type": "string",
                    "description": "The result summary your parent receives"
                },
                "outcome": {
                    "type": "string",
                    "enum": ["done", "failed"],
                    "description": "Whether the task succeeded (default done)"
                }
            }),
            &["result"],
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let result = input
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| ToolError::Input("report requires a non-empty result".to_string()))?;
        let outcome = match input.get("outcome").and_then(Value::as_str) {
            Some("failed") => ReportOutcome::Failed,
            _ => ReportOutcome::Done,
        };
        let lifecycle = ctx
            .extension::<LifecyclePlane>()
            .cloned()
            .unwrap_or_else(LifecyclePlane::disconnected);
        lifecycle.report(outcome, result).await?;
        Ok(json!({
            "title": "Report submitted",
            "output": "Report submitted; it takes effect when this turn ends. If new mail or a live subagent prevents it, you will receive a harness notice and must report again. After archive, mail from your parent can wake you for a new episode with your state handoff.",
        }))
    }
}
