//! Bounded subagent orchestration primitives.
//!
//! [`SubagentLimits`] carries the user-configurable caps that keep nested,
//! massively-parallel subagent fan-out safe: a maximum recursion depth, a cap on
//! concurrently streaming general members, and a per-top-level-run budget on the
//! total number of members that may be spawned. The [`SubagentGovernor`] that
//! enforces these lands with the orchestration workstream; this module defines the
//! limits type so config parsing (`hya-app`) can resolve it independently of the
//! engine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hya_proto::{OperationId, SessionId};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

const DEFAULT_GENERAL_STREAM_PERMITS: usize = 100;
const MAX_GENERAL_STREAM_PERMITS: usize = DEFAULT_GENERAL_STREAM_PERMITS;
const RESERVED_STREAM_PERMITS: usize = 28;

/// Configurable caps for nested/parallel subagent execution.
///
/// - `max_concurrency`: configurable ceiling on general members whose provider
///   stream is running at the same time. It is normalized to `1..=100`; excess
///   members park until a slot frees. The default live stream budget is 128,
///   split into 100 general 28 reserved permits.
/// - `per_run_budget`: maximum total number of members that may be spawned under a
///   single top-level run, bounding the total task fan-out.
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
    /// Max members spawnable under one top-level run.
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
            // Raised from 256 so a large resident swarm (100+) comfortably fits
            // under one team's total-spawn ceiling (decision 7).
            per_run_budget: 1024,
            per_team_turn_budget: 1024,
            per_team_message_budget: 1024,
        }
    }
}

/// Result of reserving a spawn against the per-run budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationReservation {
    /// New units charged for this operation.
    Acquired,
    /// Operation already reserved; reuse existing debit.
    Existing,
    /// Operation id conflicts with a different root/debit.
    Conflict,
    /// Run budget exhausted.
    Overloaded,
}

#[derive(Default)]
struct BudgetState {
    remaining: HashMap<SessionId, u64>,
    operations: HashMap<OperationId, OperationDebit>,
}

struct OperationDebit {
    root: SessionId,
    units: u64,
    cancel: CancellationToken,
}

/// Enforces [`SubagentLimits`] at runtime.
///
/// - General and reserved provider streams use independent live semaphores. The
///   general class is bounded by normalized `max_concurrency`; the reserved class
///   is fixed at 28 and cannot be borrowed by general work. The turn loop holds a
///   permit only around provider streaming (never across tool execution). These
///   are execution bounds, never durable admission or ordering truth.
/// - `reserve` draws from a per-top-level-run budget so the total number of
///   members spawned under one run cannot exceed `per_run_budget`.
/// - `release` frees a completed root's budget entry so the map cannot leak.
///
/// Per-team activity counters belong to the resident team, so replacing a
/// killed team always starts with fresh counters.
#[derive(Clone)]
pub struct SubagentGovernor {
    limits: SubagentLimits,
    general_stream_sem: Arc<Semaphore>,
    reserved_stream_sem: Arc<Semaphore>,
    budgets: Arc<Mutex<BudgetState>>,
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
            budgets: Arc::new(Mutex::new(BudgetState::default())),
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

    /// Reserve up to `want` member slots against `root`'s budget. On first sight of
    /// a root the budget is seeded to `per_run_budget`. Returns how many slots were
    /// actually granted (`0` when the budget is exhausted).
    pub fn reserve(&self, root: SessionId, want: u64) -> u64 {
        let mut budgets = self.lock_budgets();
        let remaining = budgets
            .remaining
            .entry(root)
            .or_insert(self.limits.per_run_budget);
        let granted = want.min(*remaining);
        *remaining -= granted;
        granted
    }

    /// Atomically reserve all `want` member slots or none of them.
    ///
    /// The background spawn boundary uses this before creating a child session,
    /// because a partial reservation cannot be represented by its request-level
    /// typed overload response.
    pub fn try_reserve_exact(&self, root: SessionId, want: u64) -> bool {
        let mut budgets = self.lock_budgets();
        let remaining = budgets
            .remaining
            .entry(root)
            .or_insert(self.limits.per_run_budget);
        if *remaining < want {
            return false;
        }
        *remaining -= want;
        true
    }

    /// Debit one durable operation exactly once.
    pub fn try_reserve_operation(
        &self,
        root: SessionId,
        operation: OperationId,
        units: u64,
        cancel: CancellationToken,
    ) -> OperationReservation {
        let mut budgets = self.lock_budgets();
        if let Some(existing) = budgets.operations.get(&operation) {
            return if existing.root == root && existing.units == units {
                OperationReservation::Existing
            } else {
                OperationReservation::Conflict
            };
        }
        let remaining = budgets
            .remaining
            .entry(root)
            .or_insert(self.limits.per_run_budget);
        if units == 0 || *remaining < units {
            return OperationReservation::Overloaded;
        }
        *remaining -= units;
        budgets.operations.insert(
            operation,
            OperationDebit {
                root,
                units,
                cancel,
            },
        );
        OperationReservation::Acquired
    }

    /// Release a recorded operation debit. A repeated release is a no-op.
    pub fn release_operation(&self, operation: OperationId) -> bool {
        let mut budgets = self.lock_budgets();
        let Some(debit) = budgets.operations.remove(&operation) else {
            return false;
        };
        if let Some(remaining) = budgets.remaining.get_mut(&debit.root) {
            *remaining = remaining
                .saturating_add(debit.units)
                .min(self.limits.per_run_budget);
        }
        true
    }

    /// Cancel all live operations for a root without releasing their debit.
    /// Durable finalization remains the sole authority for release.
    pub fn cancel_operations(&self, root: SessionId) -> Vec<OperationId> {
        let budgets = self.lock_budgets();
        let mut operations = Vec::new();
        for (operation, debit) in &budgets.operations {
            if debit.root == root {
                debit.cancel.cancel();
                operations.push(*operation);
            }
        }
        operations
    }

    #[must_use]
    /// Remaining per-run spawn budget for `root`, if tracked.
    pub fn remaining_budget(&self, root: SessionId) -> u64 {
        self.lock_budgets()
            .remaining
            .get(&root)
            .copied()
            .unwrap_or(self.limits.per_run_budget)
    }

    /// Return an exact in-process reservation without disturbing durable
    /// operation debits owned by other schedulers under the same root.
    pub(crate) fn refund_reserved(&self, root: SessionId, units: u64) {
        let mut budgets = self.lock_budgets();
        let Some(remaining) = budgets.remaining.get_mut(&root) else {
            return;
        };
        *remaining = remaining
            .saturating_add(units)
            .min(self.limits.per_run_budget);
        let restored = *remaining == self.limits.per_run_budget;
        let has_operations = budgets.operations.values().any(|debit| debit.root == root);
        if restored && !has_operations {
            budgets.remaining.remove(&root);
        }
    }

    /// Release a completed root's budget entry so long-lived roots do not leak.
    pub fn release(&self, root: SessionId) {
        let mut budgets = self.lock_budgets();
        budgets.remaining.remove(&root);
        let operations: Vec<OperationId> = budgets
            .operations
            .iter()
            .filter_map(|(operation, debit)| (debit.root == root).then_some(*operation))
            .collect();
        for operation in operations {
            if let Some(debit) = budgets.operations.remove(&operation) {
                debit.cancel.cancel();
            }
        }
    }

    fn lock_budgets(&self) -> std::sync::MutexGuard<'_, BudgetState> {
        lock(&self.budgets)
    }
}

fn lock<T>(m: &Arc<Mutex<T>>) -> std::sync::MutexGuard<'_, T> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_proto::{OperationId, ToolCallId};
    use tokio_util::sync::CancellationToken;

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

    #[test]
    fn reserve_grants_up_to_budget_then_stops() {
        let gov = SubagentGovernor::new(SubagentLimits {
            max_concurrency: 4,
            per_run_budget: 3,
            ..SubagentLimits::default()
        });
        let root = SessionId::new();
        assert_eq!(gov.reserve(root, 2), 2, "first reserve grants requested");
        assert_eq!(
            gov.reserve(root, 5),
            1,
            "second reserve clamps to remaining"
        );
        assert_eq!(gov.reserve(root, 1), 0, "budget exhausted");
        // A distinct root has its own fresh budget.
        assert_eq!(gov.reserve(SessionId::new(), 3), 3);
        // Release lets a root be reused with a fresh budget.
        gov.release(root);
        assert_eq!(gov.reserve(root, 3), 3, "released root reseeds budget");
    }

    #[test]
    fn exact_reserve_is_all_or_none() {
        let gov = SubagentGovernor::new(SubagentLimits {
            per_run_budget: 3,
            ..SubagentLimits::default()
        });
        let root = SessionId::new();

        assert!(
            !gov.try_reserve_exact(root, 4),
            "oversized request must be rejected"
        );
        assert!(
            gov.try_reserve_exact(root, 3),
            "failed exact reserve must not consume any budget"
        );
        assert!(
            !gov.try_reserve_exact(root, 1),
            "successful exact reserve consumes the requested budget"
        );
    }

    #[test]
    fn operation_debit_and_release_are_exactly_once() {
        let gov = SubagentGovernor::new(SubagentLimits {
            per_run_budget: 3,
            ..SubagentLimits::default()
        });
        let root = SessionId::new();
        let operation = OperationId::from_tool_call(ToolCallId::new());

        assert_eq!(
            gov.try_reserve_operation(root, operation, 2, CancellationToken::new()),
            OperationReservation::Acquired
        );
        assert_eq!(
            gov.try_reserve_operation(root, operation, 2, CancellationToken::new()),
            OperationReservation::Existing
        );
        assert_eq!(gov.remaining_budget(root), 1);
        assert!(gov.release_operation(operation));
        assert_eq!(gov.remaining_budget(root), 3);
        assert!(!gov.release_operation(operation));
        assert_eq!(gov.remaining_budget(root), 3);
    }
}
