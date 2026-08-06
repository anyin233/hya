//! Bridges `permission.ask` onto `hya_tool::PermissionInterceptor`: the first
//! plugin that returns a non-`defer` answer decides; all-defer falls through to
//! the host's normal user-ask flow (`None`).

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{Action, Decision, PermissionInterceptor, Resource};
use sha2::{Digest, Sha256};

use crate::host::PluginHost;
use crate::messages::{
    HookName, HookPosture, PermissionAskParams, PermissionOutcomeWire, WireResource,
};

const PERMISSION_BRIDGE_SEMANTIC_IDENTITY_DOMAIN_V1: &[u8] =
    b"hya.plugin.permission-bridge.semantic-identity/v1";

/// `PermissionInterceptor` that asks each loaded plugin's `permission.ask` hook.
///
/// The first non-`defer` outcome decides; if every plugin defers (or none handle
/// the hook), the interceptor returns `None` so the host can fall through to
/// the normal user-ask path.
pub struct PermissionBridge {
    host: Arc<PluginHost>,
}

impl PermissionBridge {
    /// Create a bridge over a shared [`PluginHost`].
    #[must_use]
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl PermissionInterceptor for PermissionBridge {
    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.host.permission_semantic_identity_v1()
    }

    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.host.permission_ask(session, action, resource).await
    }
}

impl PluginHost {
    fn permission_semantic_identity_v1(&self) -> Option<[u8; 32]> {
        let prepared = self.prepared_plugins();
        let participants = self
            .plugins()
            .iter()
            .zip(prepared.iter())
            .filter_map(|(conn, plugin)| {
                conn.posture(HookName::PermissionAsk)
                    .map(|posture| (plugin, posture))
            })
            .collect::<Vec<_>>();

        let mut bytes = Vec::new();
        bytes.extend_from_slice(PERMISSION_BRIDGE_SEMANTIC_IDENTITY_DOMAIN_V1);
        bytes.push(1); // participant count field
        append_identity_count(&mut bytes, participants.len())?;

        for (plugin, posture) in participants {
            bytes.push(1); // participant record
            bytes.push(1); // stable plugin ID field
            append_identity_bytes(&mut bytes, plugin.id().as_bytes())?;
            bytes.push(2); // canonical declaration field
            append_identity_bytes(&mut bytes, plugin.canonical_declaration())?;
            bytes.push(3); // effective permission.ask posture field
            bytes.push(match posture {
                HookPosture::Safe => 0,
                HookPosture::Open => 1,
            });
        }

        Some(Sha256::digest(bytes).into())
    }

    /// Walk plugins in load order and return the first non-defer permission decision.
    ///
    /// Plugins without a registered `permission.ask` hook are skipped. Serialize
    /// or RPC failures are logged and treated as continue-to-next-plugin.
    pub async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        for conn in self.plugins() {
            if conn.posture(HookName::PermissionAsk).is_none() {
                continue;
            }
            let params = PermissionAskParams {
                session,
                action,
                resource: resource_to_wire(resource),
            };
            let value = match serde_json::to_value(&params) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(%error, plugin = %conn.id, "permission.ask serialize failed");
                    continue;
                }
            };
            let Ok(reply) = conn.call_hook(HookName::PermissionAsk, value).await else {
                continue;
            };
            match serde_json::from_value::<PermissionOutcomeWire>(reply) {
                Ok(PermissionOutcomeWire::AllowOnce) => return Some(Decision::AllowOnce),
                Ok(PermissionOutcomeWire::AllowAlways) => return Some(Decision::AllowAlways),
                Ok(PermissionOutcomeWire::Reject { feedback }) => {
                    return Some(Decision::Reject { feedback });
                }
                Ok(PermissionOutcomeWire::Defer) | Err(_) => continue,
            }
        }
        None
    }
}

fn append_identity_count(bytes: &mut Vec<u8>, count: usize) -> Option<()> {
    let count = u64::try_from(count).ok()?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Some(())
}

fn append_identity_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    append_identity_count(bytes, value.len())?;
    bytes.extend_from_slice(value);
    Some(())
}

fn resource_to_wire(resource: &Resource) -> WireResource {
    match resource {
        Resource::Tool(value) => WireResource::Tool {
            value: value.clone(),
        },
        Resource::Path(value) => WireResource::Path {
            value: value.clone(),
        },
        Resource::Glob(value) => WireResource::Glob {
            value: value.clone(),
        },
        Resource::Command(value) => WireResource::Command {
            value: value.clone(),
        },
        Resource::Subagent(value) => WireResource::Subagent {
            value: value.clone(),
        },
        Resource::Url(value) => WireResource::Url {
            value: value.clone(),
        },
        Resource::WebSearch(value) => WireResource::WebSearch {
            value: value.clone(),
        },
        Resource::Skill(value) => WireResource::Skill {
            value: value.clone(),
        },
        Resource::Any => WireResource::Any,
    }
}
