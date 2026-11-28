//! Native HTTP/gRPC `BundleApi` read endpoints for `hya/agent-channels`.
//!
//! Exposed under `/v2/sessions/{session}/bundles/hya/agent-channels/...` so
//! frontends and TUI components (CHR-33) can inspect channels, message history,
//! unread inbox state, and direct-child liveness through the standard v2
//! `BundleApi` surface.

use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_proto::ChannelKind;
use hya_runtime_abi::{
    HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest,
};
use serde_json::json;

use crate::service::{AGENT_CHANNELS_BUNDLE_ID, AgentChannelsService};

/// Native [`NativeBundleApiHandler`] serving read-only channel/mailbox endpoints.
pub struct AgentChannelsBundleApiHandler {
    service: Arc<AgentChannelsService>,
    endpoints: Vec<NativeApiEndpoint>,
}

impl AgentChannelsBundleApiHandler {
    /// Construct the bundle API handler bound to `service`.
    #[must_use]
    pub fn new(service: Arc<AgentChannelsService>) -> Self {
        let endpoints = vec![
            NativeApiEndpoint {
                id: "channels.list".to_string(),
                method: ApiMethod::Get,
                scope: ApiScope::Session,
                path: "/channels".to_string(),
                description: "List channels visible to the session with unread counts and direct-child team status".to_string(),
            },
            NativeApiEndpoint {
                id: "channels.messages".to_string(),
                method: ApiMethod::Get,
                scope: ApiScope::Session,
                path: "/channels/{channel}/messages".to_string(),
                description: "Read recent messages on a channel without consuming the session's unread cursor".to_string(),
            },
            NativeApiEndpoint {
                id: "channels.inbox".to_string(),
                method: ApiMethod::Get,
                scope: ApiScope::Session,
                path: "/inbox".to_string(),
                description: "Inspect the session's unread inbox summary without advancing the cursor".to_string(),
            },
            NativeApiEndpoint {
                id: "channels.team_status".to_string(),
                method: ApiMethod::Get,
                scope: ApiScope::Session,
                path: "/team-status".to_string(),
                description: "List direct-child member liveness and heartbeat freshness".to_string(),
            },
        ];
        Self { service, endpoints }
    }
}

#[async_trait]
impl NativeBundleApiHandler for AgentChannelsBundleApiHandler {
    fn id(&self) -> &str {
        AGENT_CHANNELS_BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let Some(session) = request.session else {
            return Ok(NativeBundleApiReply {
                status: 400,
                body: json!({ "error": "session id is required for hya/agent-channels endpoints" }),
            });
        };
        let runtime = self.service.runtime()?;

        match request.api.as_str() {
            "channels.list" => {
                let channels =
                    match AgentChannelsService::list_channels(runtime.as_ref(), session).await {
                        Ok(rows) => rows,
                        Err(HostError::InvalidInput(msg)) => {
                            return Ok(NativeBundleApiReply {
                                status: 404,
                                body: json!({ "error": msg }),
                            });
                        }
                        Err(err) => return Err(err),
                    };
                let team = AgentChannelsService::team_status(runtime.as_ref(), session).await?;
                Ok(NativeBundleApiReply {
                    status: 200,
                    body: json!({
                        "channels": channels.iter().map(|row| json!({
                            "id": row.id,
                            "kind": if row.kind == ChannelKind::Group { "group" } else { "dm" },
                            "can_speak": row.can_speak,
                            "peer": row.peer,
                            "unread": row.unread,
                        })).collect::<Vec<_>>(),
                        "team": team.iter().map(|row| json!({
                            "handle": row.handle,
                            "status": row.status,
                            "last_active_seconds": row.last_active_seconds,
                        })).collect::<Vec<_>>(),
                    }),
                })
            }
            "channels.messages" => {
                let Some(channel) = request.path_params.get("channel") else {
                    return Ok(NativeBundleApiReply {
                        status: 400,
                        body: json!({ "error": "missing path parameter `channel`" }),
                    });
                };
                let last = request
                    .query
                    .get("last")
                    .and_then(|value| value.parse::<usize>().ok());
                let read = match AgentChannelsService::peek_channel_history(
                    runtime.as_ref(),
                    session,
                    channel,
                    last,
                )
                .await
                {
                    Ok(read) => read,
                    Err(HostError::InvalidInput(msg)) => {
                        return Ok(NativeBundleApiReply {
                            status: 404,
                            body: json!({ "error": msg }),
                        });
                    }
                    Err(err) => return Err(err),
                };
                Ok(NativeBundleApiReply {
                    status: 200,
                    body: json!({
                        "channel": read.channel,
                        "count": read.messages.len(),
                        "unread_remaining": read.unread_remaining,
                        "warning": read.warning,
                        "messages": read.messages.iter().map(|(from, body)| json!({
                            "from": from,
                            "body": body,
                        })).collect::<Vec<_>>(),
                    }),
                })
            }
            "channels.inbox" => {
                let summary =
                    match AgentChannelsService::inbox_summary(runtime.as_ref(), session).await {
                        Ok(summary) => summary,
                        Err(HostError::InvalidInput(msg)) => {
                            return Ok(NativeBundleApiReply {
                                status: 404,
                                body: json!({ "error": msg }),
                            });
                        }
                        Err(err) => return Err(err),
                    };
                Ok(NativeBundleApiReply {
                    status: 200,
                    body: json!({
                        "handle": summary.handle,
                        "cursor": summary.cursor,
                        "total": summary.total,
                        "unread": summary.unread.iter().map(|mail| json!({
                            "from": mail.from,
                            "channel": mail.channel,
                            "body": mail.body,
                        })).collect::<Vec<_>>(),
                    }),
                })
            }
            "channels.team_status" => {
                let members =
                    match AgentChannelsService::team_status(runtime.as_ref(), session).await {
                        Ok(rows) => rows,
                        Err(HostError::InvalidInput(msg)) => {
                            return Ok(NativeBundleApiReply {
                                status: 404,
                                body: json!({ "error": msg }),
                            });
                        }
                        Err(err) => return Err(err),
                    };
                Ok(NativeBundleApiReply {
                    status: 200,
                    body: json!({
                        "members": members.iter().map(|row| json!({
                            "handle": row.handle,
                            "status": row.status,
                            "last_active_seconds": row.last_active_seconds,
                        })).collect::<Vec<_>>(),
                    }),
                })
            }
            other => Ok(NativeBundleApiReply {
                status: 404,
                body: json!({ "error": format!("unknown hya/agent-channels endpoint `{other}`") }),
            }),
        }
    }
}
