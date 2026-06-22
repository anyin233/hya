use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub(crate) struct PtyState {
    inner: Arc<RwLock<PtyRuntime>>,
}

#[derive(Default)]
struct PtyRuntime {
    next: u64,
    sessions: BTreeMap<String, PtyInfo>,
}

#[derive(Clone, Serialize)]
pub(super) struct PtyInfo {
    id: String,
    title: String,
    command: String,
    args: Vec<String>,
    cwd: String,
    status: &'static str,
    pid: u64,
}

pub(super) struct CreatePayload {
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: String,
    pub(super) title: String,
}

pub(super) struct UpdatePayload {
    pub(super) title: Option<String>,
}

impl PtyState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(super) async fn list(&self) -> Vec<PtyInfo> {
        self.inner.read().await.sessions.values().cloned().collect()
    }

    pub(super) async fn create(&self, payload: CreatePayload) -> PtyInfo {
        let mut state = self.inner.write().await;
        state.next = state.next.saturating_add(1);
        let id = format!("pty_{:x}", state.next);
        let info = PtyInfo {
            id: id.clone(),
            title: payload.title,
            command: payload.command,
            args: payload.args,
            cwd: payload.cwd,
            status: "running",
            pid: 0,
        };
        state.sessions.insert(id, info.clone());
        info
    }

    pub(super) async fn get(&self, id: &str) -> Option<PtyInfo> {
        self.inner.read().await.sessions.get(id).cloned()
    }

    pub(super) async fn update(&self, id: &str, payload: UpdatePayload) -> Option<PtyInfo> {
        let mut state = self.inner.write().await;
        let info = state.sessions.get_mut(id)?;
        if let Some(title) = payload.title {
            info.title = title;
        }
        Some(info.clone())
    }

    pub(super) async fn remove(&self, id: &str) -> bool {
        self.inner.write().await.sessions.remove(id).is_some()
    }
}
