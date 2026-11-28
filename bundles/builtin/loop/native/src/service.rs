//! Managed [`hya_runtime_abi::NativeService`] tracking `hya/loop` driver
//! lifecycle and run statistics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_trait::async_trait;
use hya_runtime_abi::{HostError, HostRuntime, NativeService, ServiceHealth};
use serde_json::json;

use crate::types::LOOP_SERVICE_ID;

/// Lifecycle and health service for `hya/loop`.
#[derive(Default)]
pub struct LoopService {
    running: AtomicBool,
    active_runs: AtomicU64,
    completed_runs: AtomicU64,
}

/// RAII guard decrementing `active_runs` and incrementing `completed_runs` on drop.
pub(crate) struct LoopRunGuard {
    service: Arc<LoopService>,
}

impl Drop for LoopRunGuard {
    fn drop(&mut self) {
        self.service.active_runs.fetch_sub(1, Ordering::AcqRel);
        self.service.completed_runs.fetch_add(1, Ordering::AcqRel);
    }
}

impl LoopService {
    /// Construct a new stopped [`LoopService`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a loop driver run has entered execution.
    pub(crate) fn enter_run(self: &Arc<Self>) -> LoopRunGuard {
        self.active_runs.fetch_add(1, Ordering::AcqRel);
        LoopRunGuard {
            service: Arc::clone(self),
        }
    }

    /// Number of currently executing loop runs.
    #[must_use]
    pub fn active_runs(&self) -> u64 {
        self.active_runs.load(Ordering::Acquire)
    }

    /// Number of completed loop runs since startup.
    #[must_use]
    pub fn completed_runs(&self) -> u64 {
        self.completed_runs.load(Ordering::Acquire)
    }
}

#[async_trait]
impl NativeService for LoopService {
    fn id(&self) -> &str {
        LOOP_SERVICE_ID
    }

    async fn start(&self, _runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        self.running.store(true, Ordering::Release);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        self.running.store(false, Ordering::Release);
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        let running = self.running.load(Ordering::Acquire);
        let mut health = if running {
            ServiceHealth::healthy("ready")
        } else {
            ServiceHealth::unhealthy("stopped")
        };
        health.details = Some(json!({
            "active_runs": self.active_runs(),
            "completed_runs": self.completed_runs(),
        }));
        health
    }
}
