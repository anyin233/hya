//! Managed ABI v2 [`NativeService`] backing the `hya/goal` native bundle.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use hya_runtime_abi::{HostError, HostRuntime, NativeService, ServiceHealth};

/// Stable bundle identity for the built-in goal-mode bundle.
pub const BUNDLE_ID: &str = "hya/goal";

/// Stable service identifier within `hya/goal`.
pub const GOAL_SERVICE_ID: &str = "goal";

/// Lifecycle service tracking readiness of the `hya/goal` native driver.
#[derive(Debug, Default)]
pub struct GoalDriverService {
    started: AtomicBool,
}

impl GoalDriverService {
    /// Construct a new stopped [`GoalDriverService`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return `true` when the service has been started and not yet stopped.
    #[must_use]
    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }
}

#[async_trait]
impl NativeService for GoalDriverService {
    fn id(&self) -> &str {
        GOAL_SERVICE_ID
    }

    async fn start(&self, _runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        self.started.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.started.store(false, Ordering::Release);
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        if self.is_started() {
            ServiceHealth::healthy("ready")
        } else {
            ServiceHealth::unhealthy("stopped")
        }
    }
}
