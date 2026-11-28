//! Native lifecycle hook for `hya/multi-agent`.
//!
//! Cleans up per-session in-memory stall/wait bookkeeping when a session ends.

use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{HookPoint, HostError, NativeHook, NativeSessionLifecycleInput};

use crate::service::MultiAgentService;

/// Canonical hook id registered by `hya/multi-agent`.
pub const MULTI_AGENT_LIFECYCLE_HOOK_ID: &str = "multi-agent-lifecycle";

/// Native [`NativeHook`] observing session teardown to release transient wait
/// stall bookkeeping held by [`MultiAgentService`].
pub struct MultiAgentLifecycleHook {
    service: Arc<MultiAgentService>,
}

impl MultiAgentLifecycleHook {
    /// Construct the lifecycle hook bound to `service`.
    #[must_use]
    pub fn new(service: Arc<MultiAgentService>) -> Self {
        Self { service }
    }
}

#[async_trait]
impl NativeHook for MultiAgentLifecycleHook {
    fn id(&self) -> &str {
        MULTI_AGENT_LIFECYCLE_HOOK_ID
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[HookPoint::SessionEnd]
    }

    async fn session_end(&self, input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        self.service.clear_session_state(input.session);
        Ok(())
    }
}
