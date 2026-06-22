use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use tokio_util::sync::CancellationToken;
use yaca_proto::SessionId;

#[derive(Clone, Default)]
pub(crate) struct RunRegistry {
    inner: Arc<RunRegistryInner>,
}

#[derive(Default)]
struct RunRegistryInner {
    next: AtomicU64,
    runs: Mutex<HashMap<SessionId, ActiveRun>>,
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
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RunStatus {
    #[serde(rename = "type")]
    status_type: &'static str,
}

impl RunRegistry {
    pub(crate) fn start(&self, session: SessionId) -> Option<RunGuard> {
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
        Some(RunGuard {
            registry: self.clone(),
            session,
            id,
            token,
        })
    }

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
    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut runs = self.registry.lock_runs();
        if matches!(runs.get(&self.session), Some(active) if active.id == self.id) {
            runs.remove(&self.session);
        }
    }
}
