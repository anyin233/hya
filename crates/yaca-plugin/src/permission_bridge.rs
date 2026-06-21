//! Bridges `permission.ask` onto `yaca_tool::PermissionInterceptor`: the first
//! plugin that returns a non-`defer` answer decides; all-defer falls through to
//! the host's normal user-ask flow (`None`).

use std::sync::Arc;

use async_trait::async_trait;
use yaca_proto::SessionId;
use yaca_tool::{Action, Decision, PermissionInterceptor, Resource};

use crate::host::PluginHost;
use crate::messages::{HookName, PermissionAskParams, PermissionOutcomeWire, WireResource};

pub struct PermissionBridge {
    host: Arc<PluginHost>,
}

impl PermissionBridge {
    #[must_use]
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl PermissionInterceptor for PermissionBridge {
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

fn resource_to_wire(resource: &Resource) -> WireResource {
    match resource {
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
