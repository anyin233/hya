use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use hya_proto::SessionId;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// Process-local registry of server-started runs, one per session. Every
/// router and gRPC binding built from one `AppState` shares it.
#[derive(Clone, Default)]
pub(crate) struct RunRegistry {
    inner: Arc<RunRegistryInner>,
}

struct RunRegistryInner {
    next: AtomicU64,
    runs: Mutex<HashMap<SessionId, ActiveRun>>,
    /// The session of every run that started or ended (busy re-check signal).
    changes: broadcast::Sender<SessionId>,
}

impl Default for RunRegistryInner {
    fn default() -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            next: AtomicU64::default(),
            runs: Mutex::default(),
            changes,
        }
    }
}

struct ActiveRun {
    id: u64,
    token: CancellationToken,
}

pub(crate) struct RunGuard {
    registry: RunRegistry,
    session: SessionId,
    id: u64,
    token: CancellationToken,
    /// No change signal on start or end (no busy notice).
    quiet: bool,
}

#[derive(Clone, Debug, Serialize)]
#[allow(dead_code)]
pub(crate) struct RunStatus {
    #[serde(rename = "type")]
    status_type: &'static str,
}

impl RunRegistry {
    #[allow(dead_code)]
    pub(crate) fn start(&self, session: SessionId) -> Option<RunGuard> {
        self.start_with(session, false)
    }

    /// Like [`Self::start`], without the change signal on start and end, so
    /// no busy notice is published (a reservation that is not a turn).
    pub(crate) fn start_quiet(&self, session: SessionId) -> Option<RunGuard> {
        self.start_with(session, true)
    }

    fn start_with(&self, session: SessionId, quiet: bool) -> Option<RunGuard> {
        let mut runs = self.lock_runs();
        if runs.contains_key(&session) {
            return None;
        }
        let id = self.inner.next.fetch_add(1, Ordering::Relaxed);
        let token = CancellationToken::new();
        runs.insert(
            session,
            ActiveRun {
                id,
                token: token.clone(),
            },
        );
        drop(runs);
        if !quiet {
            let _ = self.inner.changes.send(session);
        }
        Some(RunGuard {
            registry: self.clone(),
            session,
            id,
            token,
            quiet,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn cancel(&self, session: SessionId) -> bool {
        let token = {
            let runs = self.lock_runs();
            runs.get(&session).map(|run| run.token.clone())
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

    #[allow(dead_code)]
    pub(crate) fn is_busy(&self, session: SessionId) -> bool {
        self.lock_runs().contains_key(&session)
    }

    #[allow(dead_code)]
    pub(crate) fn statuses(&self) -> BTreeMap<String, RunStatus> {
        self.lock_runs()
            .keys()
            .map(|session| {
                (
                    session.to_string(),
                    RunStatus {
                        status_type: "busy",
                    },
                )
            })
            .collect()
    }

    fn lock_runs(&self) -> MutexGuard<'_, HashMap<SessionId, ActiveRun>> {
        match self.inner.runs.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl RunGuard {
    #[allow(dead_code)]
    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let removed = {
            let mut runs = self.registry.lock_runs();
            if matches!(runs.get(&self.session), Some(active) if active.id == self.id) {
                runs.remove(&self.session);
                true
            } else {
                false
            }
        };
        if removed && !self.quiet {
            let _ = self.registry.inner.changes.send(self.session);
        }
    }
}
