use async_trait::async_trait;
use hya_proto::MemberId;
use hya_tool::ResolvedTool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::CoreError;
use crate::hooks::HookDispatcher;

/// Scheduling lifecycle for a harness sidecar activation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidecarLifecycle {
    Transient,
    Resident,
}

/// Opaque sidecar activation metadata bound to one member turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarStart {
    pub activation_id: String,
    pub lifecycle: SidecarLifecycle,
}

impl SidecarStart {
    pub(crate) fn transient() -> Self {
        Self {
            activation_id: format!("activation-{}", MemberId::new()),
            lifecycle: SidecarLifecycle::Transient,
        }
    }

    pub(crate) fn resident() -> Self {
        Self {
            activation_id: format!("activation-{}", MemberId::new()),
            lifecycle: SidecarLifecycle::Resident,
        }
    }
}

/// A started sidecar's readiness acknowledgment.
#[async_trait]
pub trait SidecarHandle: Send {
    async fn ready(&mut self) -> Result<(), CoreError>;

    async fn shutdown(&mut self) -> Result<(), CoreError>;

    fn is_healthy(&self) -> bool {
        true
    }

    fn loss_token(&self) -> Option<CancellationToken> {
        None
    }

    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.shutdown().await
    }

    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        Arc::from([])
    }

    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        None
    }
}

/// Request-scoped factory for a sidecar already bound by the application.
#[async_trait]
pub trait BoundSidecarFactory: Send + Sync {
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError>;
}

/// Application-owned resolver for a sidecar already bound to one turn snapshot.
///
/// Core receives only the captured binding and stable agent name. Materialization
/// and process ownership remain behind the opaque [`BoundSidecarFactory`].
pub trait SidecarEnvironment: Send + Sync {
    fn factory_for(
        &self,
        binding: &crate::TurnBinding,
        stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError>;
}
