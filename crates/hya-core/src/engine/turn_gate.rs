//! Engine-owned single-active-turn invariant.
//!
//! A session is one agent, and an agent runs at most ONE turn at any time.
//! Every path that opens an assistant turn — `run_turn*` (exec, serve, goal and
//! loop drivers, Workflow stages, subagents), shell turns, and resident actor
//! wakes — first takes the session's [`TurnLease`] from the engine's
//! [`TurnGate`]. A second claim on a busy session is refused with
//! [`CoreError::TurnAlreadyActive`] (non-blocking claims) or waits for the
//! active turn to end (queued `run_turn*` calls). Separate sessions — team
//! members, subagents — hold separate leases and stream concurrently.
//!
//! When a lease is released the gate wakes queued `run_turn*` callers and
//! notifies the installed [`TurnBoundaryObserver`] (the resident supervisor),
//! which delivers wakes that were deferred while the turn ran (child mail to a
//! lead, the team-quiescence synthesis notice) at that turn boundary.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use hya_proto::SessionId;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::error::CoreError;

tokio::task_local! {
    /// Sessions whose turn lease the current task already holds, innermost
    /// last. Lets a nested same-session `run_turn*` fail fast with
    /// [`CoreError::TurnAlreadyActive`] instead of waiting on itself forever.
    static HELD_TURNS: Vec<SessionId>;
}

/// Observer of turn boundaries, installed by the resident supervisor.
///
/// [`TurnBoundaryObserver::turn_released`] runs after a session's lease is
/// released, on a fresh task when a Tokio runtime is available, so it never
/// runs while the releasing caller still holds its own locks.
pub trait TurnBoundaryObserver: Send + Sync {
    /// `session` has no active turn any more.
    fn turn_released(&self, session: SessionId);
}

/// Per-engine registry of the one active turn per session.
#[derive(Default)]
pub(crate) struct TurnGate {
    active: Mutex<HashMap<SessionId, u64>>,
    next_id: AtomicU64,
    released: Notify,
    observer: RwLock<Option<Weak<dyn TurnBoundaryObserver>>>,
}

impl TurnGate {
    fn active(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, u64>> {
        match self.active.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Claim `session` now, or report that its turn is already active.
    pub(crate) fn try_acquire(
        self: &Arc<Self>,
        session: SessionId,
    ) -> Result<TurnLease, CoreError> {
        let mut active = self.active();
        if active.contains_key(&session) {
            return Err(CoreError::TurnAlreadyActive { session });
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        active.insert(session, id);
        Ok(TurnLease {
            gate: Arc::clone(self),
            session,
            id,
        })
    }

    /// Claim `session`, queueing behind its active turn. `Ok(None)` when
    /// `cancel` fires first; a nested claim by the task that already holds the
    /// lease is refused rather than deadlocking.
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        session: SessionId,
        cancel: &CancellationToken,
    ) -> Result<Option<TurnLease>, CoreError> {
        if HELD_TURNS
            .try_with(|held| held.contains(&session))
            .unwrap_or(false)
        {
            return Err(CoreError::TurnAlreadyActive { session });
        }
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            match self.try_acquire(session) {
                Ok(lease) => return Ok(Some(lease)),
                Err(CoreError::TurnAlreadyActive { .. }) => {}
                Err(error) => return Err(error),
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(None),
                () = &mut released => {}
            }
        }
    }

    pub(crate) fn is_active(&self, session: SessionId) -> bool {
        self.active().contains_key(&session)
    }

    pub(crate) fn set_observer(&self, observer: Weak<dyn TurnBoundaryObserver>) {
        match self.observer.write() {
            Ok(mut slot) => *slot = Some(observer),
            Err(poisoned) => *poisoned.into_inner() = Some(observer),
        }
    }

    fn observer(&self) -> Option<Arc<dyn TurnBoundaryObserver>> {
        let slot = match self.observer.read() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.as_ref().and_then(Weak::upgrade)
    }

    fn release(&self, session: SessionId, id: u64) {
        let released = {
            let mut active = self.active();
            if active.get(&session) == Some(&id) {
                active.remove(&session);
                true
            } else {
                false
            }
        };
        if !released {
            return;
        }
        self.released.notify_waiters();
        if let Some(observer) = self.observer() {
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    runtime.spawn(async move { observer.turn_released(session) });
                }
                Err(_) => observer.turn_released(session),
            }
        }
    }
}

/// Proof that the holder owns `session`'s one active turn. Dropping it ends
/// the turn for admission purposes and delivers queued wakes.
#[must_use = "dropping the lease immediately ends the turn claim"]
pub struct TurnLease {
    gate: Arc<TurnGate>,
    session: SessionId,
    id: u64,
}

impl TurnLease {
    /// The session this lease owns.
    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }
}

impl std::fmt::Debug for TurnLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnLease")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.gate.release(self.session, self.id);
    }
}

/// Run `future` with `session` recorded as held by the current task.
pub(crate) async fn scope_held_turn<F: std::future::Future>(
    session: SessionId,
    future: F,
) -> F::Output {
    let mut held = HELD_TURNS.try_with(Clone::clone).unwrap_or_default();
    held.push(session);
    HELD_TURNS.scope(held, future).await
}
