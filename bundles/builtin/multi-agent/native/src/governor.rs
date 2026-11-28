//! Bounded subagent orchestration primitives (ADR-0002, ADR-0015).
//!
//! [`SubagentGovernor`] bounds concurrent provider streams across general and
//! reserved stream classes while enforcing the fixed two-layer subagent depth
//! ceiling ([`MAX_SUBAGENT_DEPTH`]).

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Hardcoded subagent recursion depth limit (ADR-0015): root -> child -> grandchild.
pub const MAX_SUBAGENT_DEPTH: u32 = 2;

const DEFAULT_GENERAL_STREAM_PERMITS: usize = 100;
const MAX_GENERAL_STREAM_PERMITS: usize = DEFAULT_GENERAL_STREAM_PERMITS;
const RESERVED_STREAM_PERMITS: usize = 28;

/// Configurable caps for nested/parallel subagent execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentLimits {
    /// Concurrent general streaming slots (clamped to `1..=100`).
    pub max_concurrency: usize,
    /// Maximum live members per team root (including parked workflow actors).
    pub per_run_budget: u64,
    /// Maximum resident turns per team root.
    pub per_team_turn_budget: u64,
    /// Maximum `MailSent` events per team root.
    pub per_team_message_budget: u64,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_GENERAL_STREAM_PERMITS,
            per_run_budget: 1024,
            per_team_turn_budget: 1024,
            per_team_message_budget: 1024,
        }
    }
}

/// Enforces [`SubagentLimits`] at runtime using independent general and
/// reserved provider-stream semaphores.
#[derive(Clone, Debug)]
pub struct SubagentGovernor {
    limits: SubagentLimits,
    general_stream_sem: Arc<Semaphore>,
    reserved_stream_sem: Arc<Semaphore>,
}

impl SubagentGovernor {
    /// Create a governor enforcing `limits` (`max_concurrency` clamped to `1..=100`).
    #[must_use]
    pub fn new(limits: SubagentLimits) -> Self {
        let mut limits = limits;
        limits.max_concurrency = limits.max_concurrency.clamp(1, MAX_GENERAL_STREAM_PERMITS);
        Self {
            limits,
            general_stream_sem: Arc::new(Semaphore::new(limits.max_concurrency)),
            reserved_stream_sem: Arc::new(Semaphore::new(RESERVED_STREAM_PERMITS)),
        }
    }

    /// Return the configured limits.
    #[must_use]
    pub fn limits(&self) -> SubagentLimits {
        self.limits
    }

    /// Maximum subagent recursion depth: hardcoded [`MAX_SUBAGENT_DEPTH`].
    #[must_use]
    pub fn max_depth(&self) -> u32 {
        MAX_SUBAGENT_DEPTH
    }

    /// Acquire one general provider-stream permit.
    pub async fn acquire_general_stream(&self) -> Option<OwnedSemaphorePermit> {
        self.general_stream_sem.clone().acquire_owned().await.ok()
    }

    /// Acquire one reserved provider-stream permit.
    pub async fn acquire_reserved_stream(&self) -> Option<OwnedSemaphorePermit> {
        self.reserved_stream_sem.clone().acquire_owned().await.ok()
    }

    /// Number of general provider-stream permits currently available.
    #[must_use]
    pub fn available_general_stream_permits(&self) -> usize {
        self.general_stream_sem.available_permits()
    }

    /// Number of reserved provider-stream permits currently available.
    #[must_use]
    pub fn available_reserved_stream_permits(&self) -> usize {
        self.reserved_stream_sem.available_permits()
    }
}
