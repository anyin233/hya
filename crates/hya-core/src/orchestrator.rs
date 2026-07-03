//! Bounded subagent orchestration primitives.
//!
//! [`SubagentLimits`] carries the user-configurable caps that keep nested,
//! massively-parallel subagent fan-out safe: a maximum recursion depth, a global
//! cap on concurrently-*streaming* members, and a per-top-level-run budget on the
//! total number of members that may be spawned. The [`SubagentGovernor`] that
//! enforces these lands with the orchestration workstream; this module defines the
//! limits type so config parsing (`hya-app`) can resolve it independently of the
//! engine.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hya_proto::SessionId;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Configurable caps for nested/parallel subagent execution.
///
/// - `max_depth`: how many levels a subagent tree may recurse (the interactive
///   lead session is depth 0; its direct subagents are depth 1, and so on).
/// - `max_concurrency`: global ceiling on members whose provider stream is running
///   at the same time. Excess members park until a slot frees, which is the
///   backpressure that keeps 100+ agents from exhausting resources.
/// - `per_run_budget`: maximum total number of members that may be spawned under a
///   single top-level run, bounding the total task fan-out.
/// - `per_team_turn_budget`: maximum total number of resident *turns* a single
///   team may run (ADR-0002). A resident that is re-woken forever (e.g. by a mail
///   ping-pong) trips this and the team is killed.
/// - `per_team_message_budget`: maximum total number of `MailSent` a single team
///   may emit. The message-loop backstop: a runaway A↔B exchange trips this and
///   the team is killed instead of spending forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubagentLimits {
    pub max_depth: u32,
    pub max_concurrency: usize,
    pub per_run_budget: u64,
    pub per_team_turn_budget: u64,
    pub per_team_message_budget: u64,
}

impl Default for SubagentLimits {
    fn default() -> Self {
        Self {
            max_depth: 5,
            max_concurrency: 128,
            // Raised from 256 so a large resident swarm (100+) comfortably fits
            // under one team's total-spawn ceiling (decision 7).
            per_run_budget: 1024,
            per_team_turn_budget: 1024,
            per_team_message_budget: 1024,
        }
    }
}

/// Result of charging a per-team turn/message against its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamBudget {
    /// The charge was within budget; work may proceed.
    Ok,
    /// The charge tripped the budget; the team must be killed.
    Exceeded,
}

/// Enforces [`SubagentLimits`] at runtime.
///
/// - `acquire_stream` hands out a permit from a global semaphore sized to
///   `max_concurrency`; the turn loop holds it only around provider streaming
///   (never across tool execution), which bounds concurrently-streaming members
///   without risking a nested-spawn deadlock.
/// - `reserve` draws from a per-top-level-run budget so the total number of
///   members spawned under one run cannot exceed `per_run_budget`.
/// - `release` frees a completed root's budget entry so the map cannot leak.
/// - `charge_team_turn`/`charge_team_message` guard per-team runaway activity;
///   `release_team` drops the counters when a team ends.
#[derive(Clone)]
pub struct SubagentGovernor {
    limits: SubagentLimits,
    stream_sem: Arc<Semaphore>,
    budgets: Arc<Mutex<HashMap<SessionId, u64>>>,
    /// Per-team running totals of resident turns and mail messages, keyed by the
    /// team-root session. Separate from `budgets` (which is per-run spawn count)
    /// because these guard runaway *activity*, not fan-out.
    team_turns: Arc<Mutex<HashMap<SessionId, u64>>>,
    team_messages: Arc<Mutex<HashMap<SessionId, u64>>>,
}

impl SubagentGovernor {
    #[must_use]
    pub fn new(limits: SubagentLimits) -> Self {
        let permits = limits.max_concurrency.max(1);
        Self {
            limits,
            stream_sem: Arc::new(Semaphore::new(permits)),
            budgets: Arc::new(Mutex::new(HashMap::new())),
            team_turns: Arc::new(Mutex::new(HashMap::new())),
            team_messages: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[must_use]
    pub fn limits(&self) -> SubagentLimits {
        self.limits
    }

    #[must_use]
    pub fn max_depth(&self) -> u32 {
        self.limits.max_depth
    }

    /// Acquire one streaming permit, parking until a slot frees. The permit lives
    /// as long as the returned guard; drop it as soon as streaming ends. Returns
    /// `None` only if the semaphore was closed (never done in practice).
    pub async fn acquire_stream(&self) -> Option<OwnedSemaphorePermit> {
        self.stream_sem.clone().acquire_owned().await.ok()
    }

    /// Number of streaming permits currently available (for tests/metrics).
    #[must_use]
    pub fn available_permits(&self) -> usize {
        self.stream_sem.available_permits()
    }

    /// Reserve up to `want` member slots against `root`'s budget. On first sight of
    /// a root the budget is seeded to `per_run_budget`. Returns how many slots were
    /// actually granted (`0` when the budget is exhausted).
    pub fn reserve(&self, root: SessionId, want: u64) -> u64 {
        let mut budgets = self.lock_budgets();
        let remaining = budgets.entry(root).or_insert(self.limits.per_run_budget);
        let granted = want.min(*remaining);
        *remaining -= granted;
        granted
    }

    /// Release a completed root's budget entry so long-lived roots do not leak.
    pub fn release(&self, root: SessionId) {
        self.lock_budgets().remove(&root);
    }

    /// Charge one resident turn against `root`'s per-team turn budget. Returns
    /// [`TeamBudget::Exceeded`] the first time the running total exceeds the
    /// configured budget so the caller can kill the team exactly once.
    pub fn charge_team_turn(&self, root: SessionId) -> TeamBudget {
        charge(&self.team_turns, root, self.limits.per_team_turn_budget)
    }

    /// Charge one mail message against `root`'s per-team message budget. Returns
    /// [`TeamBudget::Exceeded`] the first time the running total exceeds the
    /// configured budget (the message-loop backstop).
    pub fn charge_team_message(&self, root: SessionId) -> TeamBudget {
        charge(
            &self.team_messages,
            root,
            self.limits.per_team_message_budget,
        )
    }

    /// Drop a finished/killed team's turn + message counters so long-lived roots
    /// do not leak entries.
    pub fn release_team(&self, root: SessionId) {
        lock(&self.team_turns).remove(&root);
        lock(&self.team_messages).remove(&root);
    }

    fn lock_budgets(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, u64>> {
        lock(&self.budgets)
    }
}

/// Increment `root`'s counter in `map` and report whether it now exceeds `budget`.
/// A `budget` of 0 means "unbounded" (never trips), so a misconfiguration cannot
/// wedge every team immediately.
fn charge(map: &Arc<Mutex<HashMap<SessionId, u64>>>, root: SessionId, budget: u64) -> TeamBudget {
    if budget == 0 {
        return TeamBudget::Ok;
    }
    let mut guard = lock(map);
    let count = guard.entry(root).or_insert(0);
    *count = count.saturating_add(1);
    if *count > budget {
        TeamBudget::Exceeded
    } else {
        TeamBudget::Ok
    }
}

fn lock<K, V>(m: &Arc<Mutex<HashMap<K, V>>>) -> std::sync::MutexGuard<'_, HashMap<K, V>> {
    match m.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn reserve_grants_up_to_budget_then_stops() {
        let gov = SubagentGovernor::new(SubagentLimits {
            max_depth: 5,
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

    #[tokio::test]
    async fn acquire_stream_caps_concurrency() {
        let gov = SubagentGovernor::new(SubagentLimits {
            max_depth: 5,
            max_concurrency: 2,
            per_run_budget: 100,
            ..SubagentLimits::default()
        });
        let p1 = gov.acquire_stream().await.expect("permit 1");
        let _p2 = gov.acquire_stream().await.expect("permit 2");
        assert_eq!(gov.available_permits(), 0);
        // A third acquire would block; confirm it is not immediately available.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), gov.acquire_stream())
                .await
                .is_err(),
            "third permit must block past capacity"
        );
        drop(p1);
        assert_eq!(gov.available_permits(), 1, "dropping a permit frees a slot");
    }

    #[test]
    fn team_message_budget_trips_once_then_release_resets() {
        let gov = SubagentGovernor::new(SubagentLimits {
            per_team_message_budget: 2,
            ..SubagentLimits::default()
        });
        let root = SessionId::new();
        assert_eq!(gov.charge_team_message(root), TeamBudget::Ok, "1st ok");
        assert_eq!(gov.charge_team_message(root), TeamBudget::Ok, "2nd ok");
        assert_eq!(
            gov.charge_team_message(root),
            TeamBudget::Exceeded,
            "3rd exceeds budget of 2"
        );
        // A distinct team has its own fresh counter.
        assert_eq!(gov.charge_team_message(SessionId::new()), TeamBudget::Ok);
        // Release resets the counter so a reused root starts clean.
        gov.release_team(root);
        assert_eq!(gov.charge_team_message(root), TeamBudget::Ok, "reset");
    }

    #[test]
    fn zero_team_budget_is_unbounded() {
        let gov = SubagentGovernor::new(SubagentLimits {
            per_team_turn_budget: 0,
            ..SubagentLimits::default()
        });
        let root = SessionId::new();
        for _ in 0..10_000 {
            assert_eq!(gov.charge_team_turn(root), TeamBudget::Ok);
        }
    }
}
