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
    /// One-shot sidecar for a single member turn.
    Transient,
    /// Long-lived sidecar co-lived with a resident actor.
    Resident,
}

/// Opaque sidecar activation metadata bound to one member turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidecarStart {
    /// Opaque activation id for this start.
    pub activation_id: String,
    /// Transient vs resident scheduling.
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
    /// Wait until the sidecar is ready to serve tools/hooks.
    async fn ready(&mut self) -> Result<(), CoreError>;

    /// Gracefully shut down the sidecar.
    async fn shutdown(&mut self) -> Result<(), CoreError>;

    /// Whether the sidecar is still healthy.
    fn is_healthy(&self) -> bool {
        true
    }

    /// Optional token cancelled when the sidecar is lost.
    fn loss_token(&self) -> Option<CancellationToken> {
        None
    }

    /// Force terminate (defaults to shutdown).
    async fn terminate(&mut self) -> Result<(), CoreError> {
        self.shutdown().await
    }

    /// Tools contributed by this sidecar activation.
    fn tool_bindings(&self) -> Arc<[ResolvedTool]> {
        Arc::from([])
    }

    /// Optional hook dispatcher bound to this sidecar.
    fn hook_dispatcher(&self) -> Option<Arc<dyn HookDispatcher>> {
        None
    }
}

/// Request-scoped factory for a sidecar already bound by the application.
#[async_trait]
pub trait BoundSidecarFactory: Send + Sync {
    /// Start a sidecar with the given lifecycle metadata.
    async fn start(&self, start: SidecarStart) -> Result<Box<dyn SidecarHandle>, CoreError>;
}

/// Application-owned resolver for a sidecar already bound to one turn snapshot.
///
/// Core receives only the captured binding and stable agent name. Materialization
/// and process ownership remain behind the opaque [`BoundSidecarFactory`].
pub trait SidecarEnvironment: Send + Sync {
    /// Resolve a bound factory for `stable_id` under `binding`, if any.
    fn factory_for(
        &self,
        binding: &crate::TurnBinding,
        stable_id: &str,
    ) -> Result<Option<Arc<dyn BoundSidecarFactory>>, CoreError>;
}
