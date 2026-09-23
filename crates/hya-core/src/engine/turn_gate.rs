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
//!
//! Each active turn also carries an engine-owned cancellation token and an
//! optional [`FinishCause`]. [`TurnGate::cancel`] stops one session's turn
//! with a cause (a user abort); [`TurnGate::begin_drain`] stops every active
//! turn in every session and refuses new claims (graceful process stop). The
//! turn records the cause on its closing `MessageFinished`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use hya_proto::{FinishCause, SessionId};
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

    /// A turn on `session` claimed its lease and is about to run. Called
    /// synchronously on the turn's task.
    fn turn_started(&self, _session: SessionId) {}

    /// A turn on `session` failed with a runtime/provider error (never a
    /// cancel). Called synchronously on the turn's task BEFORE its lease is
    /// released, so wakes the release would deliver can be held back.
    fn turn_failed(&self, _session: SessionId) {}
}

/// One session's active turn: its lease id, engine-owned cancel token, and
/// the cause recorded when the engine cancelled it.
struct ActiveTurn {
    id: u64,
    cancel: CancellationToken,
    cause: Option<FinishCause>,
}

#[derive(Default)]
struct GateState {
    active: HashMap<SessionId, ActiveTurn>,
    /// Set once a drain began: every new claim is refused.
    draining: Option<FinishCause>,
}

/// Per-engine registry of the one active turn per session.
#[derive(Default)]
pub(crate) struct TurnGate {
    state: Mutex<GateState>,
    next_id: AtomicU64,
    released: Notify,
    observer: RwLock<Option<Weak<dyn TurnBoundaryObserver>>>,
}

impl TurnGate {
    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Claim `session` now, or report that its turn is already active.
    /// A draining gate refuses every claim with [`CoreError::Cancelled`].
    pub(crate) fn try_acquire(
        self: &Arc<Self>,
        session: SessionId,
    ) -> Result<TurnLease, CoreError> {
        let mut state = self.state();
        if state.draining.is_some() {
            return Err(CoreError::Cancelled);
        }
        if state.active.contains_key(&session) {
            return Err(CoreError::TurnAlreadyActive { session });
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        state.active.insert(
            session,
            ActiveTurn {
                id,
                cancel: CancellationToken::new(),
                cause: None,
            },
        );
        Ok(TurnLease {
            gate: Arc::clone(self),
            session,
            id,
        })
    }

    /// Claim `session`, queueing behind its active turn. `Ok(None)` when
    /// `cancel` fires first or the gate starts draining; a nested claim by the
    /// task that already holds the lease is refused rather than deadlocking.
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
                Err(CoreError::Cancelled) => return Ok(None),
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
        self.state().active.contains_key(&session)
    }

    /// Sessions that currently hold a turn.
    pub(crate) fn active_sessions(&self) -> Vec<SessionId> {
        self.state().active.keys().copied().collect()
    }

    /// Cancel `session`'s active turn, recording `cause` unless one is
    /// already recorded. `false` when the session has no active turn.
    pub(crate) fn cancel(&self, session: SessionId, cause: FinishCause) -> bool {
        let token = {
            let mut state = self.state();
            let Some(turn) = state.active.get_mut(&session) else {
                return false;
            };
            turn.cause.get_or_insert(cause);
            turn.cancel.clone()
        };
        token.cancel();
        true
    }

    /// The cause recorded when the engine cancelled `session`'s active turn.
    pub(crate) fn cancel_cause(&self, session: SessionId) -> Option<FinishCause> {
        self.state()
            .active
            .get(&session)
            .and_then(|turn| turn.cause)
    }

    /// Start draining: refuse every new claim and cancel every active turn
    /// with `cause`. Returns the sessions whose turns were cancelled.
    /// Idempotent; the first drain's cause wins.
    pub(crate) fn begin_drain(&self, cause: FinishCause) -> Vec<SessionId> {
        let (sessions, tokens) = {
            let mut state = self.state();
            state.draining.get_or_insert(cause);
            let mut sessions = Vec::with_capacity(state.active.len());
            let mut tokens = Vec::with_capacity(state.active.len());
            for (session, turn) in &mut state.active {
                turn.cause.get_or_insert(cause);
                sessions.push(*session);
                tokens.push(turn.cancel.clone());
            }
            (sessions, tokens)
        };
        for token in tokens {
            token.cancel();
        }
        // Queued claims re-check and observe the drain.
        self.released.notify_waiters();
        sessions
    }

    /// Whether a drain has begun.
    pub(crate) fn draining(&self) -> Option<FinishCause> {
        self.state().draining
    }

    /// Wait until no session holds a turn, up to `deadline`. `true` when the
    /// gate went idle in time.
    pub(crate) async fn wait_idle(&self, deadline: tokio::time::Instant) -> bool {
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if self.state().active.is_empty() {
                return true;
            }
            if tokio::time::timeout_at(deadline, released).await.is_err() {
                return self.state().active.is_empty();
            }
        }
    }

    /// Wait until `session` holds no turn, up to `deadline`. `true` when its
    /// turn (if any) ended in time.
    pub(crate) async fn wait_released(
        &self,
        session: SessionId,
        deadline: tokio::time::Instant,
    ) -> bool {
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if !self.state().active.contains_key(&session) {
                return true;
            }
            if tokio::time::timeout_at(deadline, released).await.is_err() {
                return !self.state().active.contains_key(&session);
            }
        }
    }

    /// Bind the turn's effective cancel token: a child of the caller's token
    /// that the engine can also cancel. A cancel that raced ahead of the
    /// binding is carried over.
    fn bind_cancel(
        &self,
        session: SessionId,
        id: u64,
        caller: &CancellationToken,
    ) -> CancellationToken {
        let token = caller.child_token();
        let mut state = self.state();
        if let Some(turn) = state.active.get_mut(&session)
            && turn.id == id
        {
            if turn.cancel.is_cancelled() {
                token.cancel();
            }
            turn.cancel = token.clone();
        }
        token
    }

    pub(crate) fn set_observer(&self, observer: Weak<dyn TurnBoundaryObserver>) {
        match self.observer.write() {
            Ok(mut slot) => *slot = Some(observer),
            Err(poisoned) => *poisoned.into_inner() = Some(observer),
        }
    }

    pub(crate) fn observer(&self) -> Option<Arc<dyn TurnBoundaryObserver>> {
        let slot = match self.observer.read() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.as_ref().and_then(Weak::upgrade)
    }

    fn release(&self, session: SessionId, id: u64) {
        let released = {
            let mut state = self.state();
            if state.active.get(&session).is_some_and(|turn| turn.id == id) {
                state.active.remove(&session);
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

    /// The turn's effective cancel token: fires when `caller` fires or when
    /// the engine cancels this turn (user abort, drain).
    pub(crate) fn bind_cancel(&self, caller: &CancellationToken) -> CancellationToken {
        self.gate.bind_cancel(self.session, self.id, caller)
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
