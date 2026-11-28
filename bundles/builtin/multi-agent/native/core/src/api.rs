//! Native HTTP/gRPC `BundleApi` handler for `hya/multi-agent`.
//!
//! Exposes `/members`, `/archive`, and `/steer` on both session-scoped
//! (`/v2/sessions/{id}/bundles/hya%2Fmulti-agent/...`) and global-scoped
//! (`/v2/bundles/hya%2Fmulti-agent/api/...`) mounts, replacing the temporary
//! in-tree `builtin_bundle_apis.rs` shim in `hya-server`.

use std::sync::Arc;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_proto::SessionId;
use hya_runtime_abi::{
    HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest,
};
use serde_json::{Value, json};

use crate::claims::MULTI_AGENT_BUNDLE_ID;
use crate::service::MultiAgentService;

/// Native [`NativeBundleApiHandler`] serving `/members`, `/archive`, and `/steer`.
pub struct MultiAgentBundleApiHandler {
    service: Arc<MultiAgentService>,
    endpoints: Vec<NativeApiEndpoint>,
}

impl MultiAgentBundleApiHandler {
    /// Construct the bundle API handler bound to `service`.
    #[must_use]
    pub fn new(service: Arc<MultiAgentService>) -> Self {
        let endpoints = vec![
            NativeApiEndpoint {
                id: "list_members".to_owned(),
                method: ApiMethod::Get,
                scope: ApiScope::Session,
                path: "/members".to_owned(),
                description: "List spawned subagent members, live team roster, and archived entries for a session.".to_owned(),
            },
            NativeApiEndpoint {
                id: "archive_subagent".to_owned(),
                method: ApiMethod::Post,
                scope: ApiScope::Session,
                path: "/archive".to_owned(),
                description: "Archive a subagent session on the user's behalf.".to_owned(),
            },
            NativeApiEndpoint {
                id: "steer_subagent".to_owned(),
                method: ApiMethod::Post,
                scope: ApiScope::Session,
                path: "/steer".to_owned(),
                description: "Steer a subagent session with a message from the user.".to_owned(),
            },
            NativeApiEndpoint {
                id: "list_members_global".to_owned(),
                method: ApiMethod::Get,
                scope: ApiScope::Global,
                path: "/members".to_owned(),
                description: "List subagent members for a session specified via `?session=<id>`.".to_owned(),
            },
            NativeApiEndpoint {
                id: "archive_subagent_global".to_owned(),
                method: ApiMethod::Post,
                scope: ApiScope::Global,
                path: "/archive".to_owned(),
                description: "Archive a subagent session specified via `{\"session\": \"<id>\"}`.".to_owned(),
            },
            NativeApiEndpoint {
                id: "steer_subagent_global".to_owned(),
                method: ApiMethod::Post,
                scope: ApiScope::Global,
                path: "/steer".to_owned(),
                description: "Steer a subagent session specified via `{\"session\": \"<id>\", \"text\": \"...\"}`.".to_owned(),
            },
        ];
        Self { service, endpoints }
    }
}

fn resolve_target_session(
    request: &NativeBundleApiRequest,
) -> Result<SessionId, NativeBundleApiReply> {
    if let Some(session) = request.session {
        return Ok(session);
    }
    let raw = request
        .query
        .get("session")
        .map(String::as_str)
        .or_else(|| request.body.get("session").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error_reply(400, "invalid_argument", "session is required"))?;
    raw.parse::<SessionId>().map_err(|_| {
        error_reply(
            400,
            "invalid_argument",
            &format!("invalid session id: {raw}"),
        )
    })
}

fn error_reply(status: u16, code: &str, message: &str) -> NativeBundleApiReply {
    NativeBundleApiReply {
        status,
        body: json!({
            "error": {
                "code": code,
                "message": message,
            }
        }),
    }
}

fn host_error_reply(error: HostError) -> NativeBundleApiReply {
    match error {
        HostError::InvalidInput(message) => error_reply(400, "invalid_argument", &message),
        HostError::PermissionDenied(message) => error_reply(403, "permission_denied", &message),
        HostError::NotFound(message) => error_reply(404, "not_found", &message),
        HostError::Conflict(message) | HostError::StaleClaim(message) => {
            error_reply(409, "failed_precondition", &message)
        }
        HostError::Unavailable(message) => error_reply(503, "unavailable", &message),
        HostError::Cancelled => error_reply(499, "cancelled", "cancelled"),
        HostError::Storage(message) | HostError::Internal(message) => {
            error_reply(500, "internal", &message)
        }
    }
}

#[async_trait]
impl NativeBundleApiHandler for MultiAgentBundleApiHandler {
    fn id(&self) -> &str {
        MULTI_AGENT_BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let session = match resolve_target_session(&request) {
            Ok(session) => session,
            Err(reply) => return Ok(reply),
        };
        let runtime = self.service.runtime()?;

        match request.api.as_str() {
            "list_members" | "list_members_global" => {
                match MultiAgentService::list_members(runtime.as_ref(), session).await {
                    Ok(snapshot) => Ok(NativeBundleApiReply {
                        status: 200,
                        body: json!({
                            "session": snapshot.session,
                            "root": snapshot.root,
                            "members": snapshot.members,
                            "roster": snapshot.roster,
                            "archived": snapshot.archived,
                        }),
                    }),
                    Err(error) => Ok(host_error_reply(error)),
                }
            }
            "archive_subagent" | "archive_subagent_global" => {
                match MultiAgentService::archive_subagent(runtime.as_ref(), session).await {
                    Ok(receipt) => Ok(NativeBundleApiReply {
                        status: 200,
                        body: json!({
                            "handle": receipt.handle,
                            "cancelledTurn": receipt.cancelled_turn,
                            "descendants": receipt.descendants,
                        }),
                    }),
                    Err(error) => Ok(host_error_reply(error)),
                }
            }
            "steer_subagent" | "steer_subagent_global" => {
                let text = request
                    .body
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match MultiAgentService::steer_subagent(runtime.as_ref(), session, text).await {
                    Ok(receipt) => Ok(NativeBundleApiReply {
                        status: 200,
                        body: json!({ "from": receipt.from }),
                    }),
                    Err(error) => Ok(host_error_reply(error)),
                }
            }
            other => Err(HostError::NotFound(format!(
                "unknown multi-agent endpoint `{other}`"
            ))),
        }
    }
}
