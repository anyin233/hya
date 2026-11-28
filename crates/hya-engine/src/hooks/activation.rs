use std::future::Future;
use std::sync::{Arc, RwLock};

use hya_proto::SessionId;

use super::HookChain;

#[derive(Clone)]
struct ActivationHookContext {
    session: SessionId,
    hooks: Arc<RwLock<Option<Arc<HookChain>>>>,
}

tokio::task_local! {
    static ACTIVATION_HOOK_CONTEXT: ActivationHookContext;
}

pub(crate) async fn scope_activation_hooks<F, T>(
    session: SessionId,
    hooks: Arc<HookChain>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    ACTIVATION_HOOK_CONTEXT
        .scope(
            ActivationHookContext {
                session,
                hooks: Arc::new(RwLock::new(Some(hooks))),
            },
            Box::pin(future),
        )
        .await
}

pub(crate) async fn scope_optional_activation_hooks<F, T>(
    session: SessionId,
    hooks: Option<Arc<HookChain>>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    ACTIVATION_HOOK_CONTEXT
        .scope(
            ActivationHookContext {
                session,
                hooks: Arc::new(RwLock::new(hooks)),
            },
            Box::pin(future),
        )
        .await
}

pub(crate) fn activation_hook_for(session: SessionId) -> Option<Arc<HookChain>> {
    ACTIVATION_HOOK_CONTEXT
        .try_with(|context| {
            (context.session == session).then(|| {
                context
                    .hooks
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone()
            })
        })
        .ok()
        .flatten()
        .flatten()
}

pub(crate) fn replace_activation_hooks(session: SessionId, hooks: Option<Arc<HookChain>>) {
    let _ = ACTIVATION_HOOK_CONTEXT.try_with(|context| {
        if context.session == session {
            *context
                .hooks
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = hooks;
        }
    });
}
