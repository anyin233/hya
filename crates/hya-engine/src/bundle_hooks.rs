//! Bundle hook chains as the permission plane's `permission.ask` interceptor,
//! and the hook view a private sidecar process may use.

use crate::hooks::{HookChain, HookEntry};
use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{Action, Decision, PermissionInterceptor, Resource};
use std::sync::Arc;

/// A sidecar process may observe events and guard or rewrite tool calls only.
pub(crate) fn restricted_sidecar_hooks(entry: HookEntry) -> Arc<HookChain> {
    let references = [
        "sidecar/hook/event".to_string(),
        "sidecar/hook/tool.execute.before".to_string(),
        "sidecar/hook/tool.execute.after".to_string(),
    ];
    Arc::new(HookChain::new(vec![entry.restrict(&references)]))
}

pub(crate) struct BundlePermissionInterceptor {
    chain: Arc<HookChain>,
}

impl BundlePermissionInterceptor {
    pub(crate) fn new(chain: Arc<HookChain>) -> Self {
        Self { chain }
    }
}

#[async_trait]
impl PermissionInterceptor for BundlePermissionInterceptor {
    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        self.chain.permission_identity()
    }

    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.chain.permission_ask(session, action, resource).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_runtime_abi::{HookPoint, HostError, NativeHook};

    struct PermissionHook(Option<Decision>);

    #[async_trait]
    impl NativeHook for PermissionHook {
        fn id(&self) -> &str {
            "permission"
        }
        fn hook_points(&self) -> &[HookPoint] {
            &[HookPoint::PermissionAsk]
        }
        async fn permission_ask(
            &self,
            _session: Option<SessionId>,
            _action: Action,
            _resource: &Resource,
        ) -> Result<Option<Decision>, HostError> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn private_hook_refs_gate_permission_ask() {
        let hook: Arc<dyn NativeHook> = Arc::new(PermissionHook(Some(Decision::AllowOnce)));
        let resource = Resource::Command("ls".into());
        let hidden = HookChain::new(vec![HookEntry::scoped(
            Arc::clone(&hook),
            &["acme/hook/message.user.before".to_string()],
        )]);
        assert!(
            hidden
                .permission_ask(None, Action::Bash, &resource)
                .await
                .is_none()
        );
        let selected = HookChain::new(vec![HookEntry::scoped(
            hook,
            &["acme/hook/permission.ask".to_string()],
        )]);
        assert!(matches!(
            selected.permission_ask(None, Action::Bash, &resource).await,
            Some(Decision::AllowOnce)
        ));
    }

    #[tokio::test]
    async fn permission_chain_stops_on_first_decision_and_all_defer_returns_none() {
        let resource = Resource::Command("ls".into());
        let chain = HookChain::of_hooks([
            Arc::new(PermissionHook(None)) as Arc<dyn NativeHook>,
            Arc::new(PermissionHook(Some(Decision::Reject {
                feedback: Some("no".into()),
            }))),
            Arc::new(PermissionHook(Some(Decision::AllowOnce))),
        ]);
        assert!(matches!(
            chain.permission_ask(None, Action::Bash, &resource).await,
            Some(Decision::Reject { .. })
        ));
        let defer = HookChain::of_hooks([
            Arc::new(PermissionHook(None)) as Arc<dyn NativeHook>,
            Arc::new(PermissionHook(None)),
        ]);
        assert!(
            defer
                .permission_ask(None, Action::Bash, &resource)
                .await
                .is_none()
        );
    }
}
