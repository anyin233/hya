use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use hya_proto::SessionId;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

/// Process-local registry of server-started runs, one per session. Every
/// router and gRPC binding built from one `AppState` shares it.
#[derive(Clone, Default)]
pub(crate) struct RunRegistry {
    inner: Arc<RunRegistryInner>,
}

struct RunRegistryInner {
    next: AtomicU64,
    state: Mutex<RegistryState>,
    draining: AtomicBool,
    released: Notify,
    /// The session of every run that started or ended (busy re-check signal).
    changes: broadcast::Sender<SessionId>,
}

impl Default for RunRegistryInner {
    fn default() -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            next: AtomicU64::default(),
            state: Mutex::default(),
            draining: AtomicBool::default(),
            released: Notify::default(),
            changes,
        }
    }
}

#[derive(Default)]
struct RegistryState {
    runs: HashMap<SessionId, ActiveRun>,
    deleting: HashSet<SessionId>,
}

pub(crate) struct DeleteGuard {
    registry: RunRegistry,
    sessions: Vec<SessionId>,
}

struct ActiveRun {
    id: u64,
    token: CancellationToken,
    admission_closed: Arc<AtomicBool>,
}

pub(crate) struct RunGuard {
    registry: RunRegistry,
    session: SessionId,
    id: u64,
    token: CancellationToken,
    admission_closed: Arc<AtomicBool>,
    /// No change signal on start or end (no busy notice).
    quiet: bool,
}

impl RunRegistry {
    pub(crate) fn start(&self, session: SessionId) -> Option<RunGuard> {
        self.start_with(session, false)
    }

    /// Like [`Self::start`], without the change signal on start and end, so
    /// no busy notice is published (a reservation that is not a turn).
    pub(crate) fn start_quiet(&self, session: SessionId) -> Option<RunGuard> {
        self.start_with(session, true)
    }

    fn start_with(&self, session: SessionId, quiet: bool) -> Option<RunGuard> {
        let mut state = self.lock_state();
        if self.inner.draining.load(Ordering::Acquire)
            || state.deleting.contains(&session)
            || state.runs.contains_key(&session)
        {
            return None;
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        let admission_closed = Arc::new(AtomicBool::new(false));
        state.runs.insert(
            session,
            ActiveRun {
                id,
                token: token.clone(),
                admission_closed: admission_closed.clone(),
            },
        );
        drop(state);
        if !quiet {
            let _ = self.inner.changes.send(session);
        }
        Some(RunGuard {
            registry: self.clone(),
            session,
            id,
            token,
            admission_closed,
            quiet,
        })
    }

    pub(crate) fn cancel(&self, session: SessionId) -> bool {
        let token = {
            let state = self.lock_state();
            state.runs.get(&session).map(|run| run.token.clone())
        };
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Subscribe to the sessions whose run started or ended.
    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<SessionId> {
        self.inner.changes.subscribe()
    }

    pub(crate) fn is_busy(&self, session: SessionId) -> bool {
        self.lock_state().runs.contains_key(&session)
    }

    /// Fence admission while existing writers settle and their rows are deleted.
    pub(crate) fn begin_delete(&self, sessions: Vec<SessionId>) -> Option<DeleteGuard> {
        let mut state = self.lock_state();
        if sessions
            .iter()
            .any(|session| state.deleting.contains(session))
        {
            return None;
        }
        for session in &sessions {
            if let Some(run) = state.runs.get(session) {
                run.admission_closed.store(true, Ordering::Release);
            }
        }
        state.deleting.extend(sessions.iter().copied());
        Some(DeleteGuard {
            registry: self.clone(),
            sessions,
        })
    }

    pub(crate) async fn wait_sessions_idle(&self, sessions: &[SessionId]) {
        loop {
            let released = self.inner.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            let idle = {
                let state = self.lock_state();
                sessions
                    .iter()
                    .all(|session| !state.runs.contains_key(session))
            };
            if idle {
                return;
            }
            released.await;
        }
    }

    /// Refuse every new run from now on (`start` returns `None`).
    pub(crate) fn begin_drain(&self) {
        self.inner.draining.store(true, Ordering::Release);
    }

    /// Whether a drain began. A run reserved before it must not admit work.
    pub(crate) fn draining(&self) -> bool {
        self.inner.draining.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_idle(&self, deadline: tokio::time::Instant) -> bool {
        loop {
            let released = self.inner.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            if self.lock_state().runs.is_empty() {
                return true;
            }
            if tokio::time::timeout_at(deadline, released).await.is_err() {
                return self.lock_state().runs.is_empty();
            }
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, RegistryState> {
        match self.inner.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl RunGuard {
    /// Delete discards a not-yet-admitted prompt; user cancellation still
    /// admits and answers it with UserCancel. Keep this after a delete timeout.
    pub(crate) fn admission_closed(&self) -> bool {
        self.admission_closed.load(Ordering::Acquire)
    }

    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let removed = {
            let mut state = self.registry.lock_state();
            if matches!(state.runs.get(&self.session), Some(active) if active.id == self.id) {
                state.runs.remove(&self.session);
                true
            } else {
                false
            }
        };
        if removed {
            self.registry.inner.released.notify_waiters();
            if !self.quiet {
                let _ = self.registry.inner.changes.send(self.session);
            }
        }
    }
}

impl Drop for DeleteGuard {
    fn drop(&mut self) {
        let mut state = self.registry.lock_state();
        for session in &self.sessions {
            state.deleting.remove(session);
        }
    }
}
