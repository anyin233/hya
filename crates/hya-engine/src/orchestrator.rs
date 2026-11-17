//! Bounded subagent orchestration primitives.
//!
//! [`SubagentGovernor`] bounds concurrent provider streams. The store enforces
//! live-member capacity with active claims; team activity counters belong to
//! the resident team.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const DEFAULT_GENERAL_STREAM_PERMITS: usize = 100;
const MAX_GENERAL_STREAM_PERMITS: usize = DEFAULT_GENERAL_STREAM_PERMITS;
const RESERVED_STREAM_PERMITS: usize = 28;

/// Configurable caps for nested/parallel subagent execution.
///
/// - `max_concurrency`: configurable ceiling on general members whose provider
///   stream is running at the same time. It is normalized to `1..=100`; excess
///   members park until a slot frees. The default live stream budget is 128,
///   split into 100 general 28 reserved permits.
/// - `per_run_budget`: maximum live members per team root at once. Active claims
///   enforce it across root turns and restart; archive frees capacity.
/// - `per_team_turn_budget`: maximum total number of resident *turns* a single
///   team may run (ADR-0002). A resident that is re-woken forever (e.g. by a mail
///   ping-pong) trips this and the team is killed.
/// - `per_team_message_budget`: maximum total number of `MailSent` a single team
///   may emit. The message-loop backstop: a runaway A↔B exchange trips this and
///   the team is killed instead of spending forever.
///
/// Recursion depth is **not** here: it is the hardcoded
/// [`MAX_SUBAGENT_DEPTH`](crate::MAX_SUBAGENT_DEPTH) (ADR-0015).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentLimits {
    /// Concurrent general streaming slots (clamped 1..=100).
    pub max_concurrency: usize,
    /// Max live members per team root (including parked Workflow actors).
    pub per_run_budget: u64,
    /// Max resident turns per team root.
    pub per_team_turn_budget: u64,
    /// Max MailSent events per team root.
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

/// Enforces [`SubagentLimits`] at runtime.
///
/// - General and reserved provider streams use independent live semaphores. The
///   general class is bounded by normalized `max_concurrency`; the reserved class
///   is fixed at 28 and cannot be borrowed by general work. The turn loop holds a
///   permit only around provider streaming (never across tool execution). These
///   are execution bounds, never durable admission or ordering truth.
///
/// Per-team activity counters belong to the resident team, so replacing a
/// killed team always starts with fresh counters.
#[derive(Clone)]
pub struct SubagentGovernor {
    limits: SubagentLimits,
    general_stream_sem: Arc<Semaphore>,
    reserved_stream_sem: Arc<Semaphore>,
}

impl SubagentGovernor {
    #[must_use]
    /// Create a governor enforcing `limits` (concurrency clamped to 1..=100).
    pub fn new(limits: SubagentLimits) -> Self {
        let mut limits = limits;
        limits.max_concurrency = limits.max_concurrency.clamp(1, MAX_GENERAL_STREAM_PERMITS);
        Self {
            limits,
            general_stream_sem: Arc::new(Semaphore::new(limits.max_concurrency)),
            reserved_stream_sem: Arc::new(Semaphore::new(RESERVED_STREAM_PERMITS)),
        }
    }

    #[must_use]
    /// Return the configured limits.
    pub fn limits(&self) -> SubagentLimits {
        self.limits
    }

    #[must_use]
    /// Maximum subagent recursion depth: the hardcoded
    /// [`MAX_SUBAGENT_DEPTH`](crate::MAX_SUBAGENT_DEPTH) (ADR-0015).
    pub fn max_depth(&self) -> u32 {
        crate::MAX_SUBAGENT_DEPTH
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// ADR-0015: exactly two subagent layers, engine-owned and not
    /// configurable — no limits field can raise it.
    #[test]
    fn subagent_depth_is_hardcoded_to_two_layers() {
        assert_eq!(crate::MAX_SUBAGENT_DEPTH, 2);
        assert_eq!(
            SubagentGovernor::new(SubagentLimits::default()).max_depth(),
            2
        );
        assert_eq!(
            SubagentGovernor::new(SubagentLimits {
                max_concurrency: 100,
                per_run_budget: 1024,
                ..SubagentLimits::default()
            })
            .max_depth(),
            2
        );
    }
}
