use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::RwLock;

const CONNECT_TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(crate) struct PtyState {
    inner: Arc<RwLock<PtyRuntime>>,
}

#[derive(Default)]
struct PtyRuntime {
    next: u64,
    sessions: BTreeMap<String, PtyInfo>,
    tickets: BTreeMap<String, ConnectTicket>,
}

struct ConnectTicket {
    pty_id: String,
    expires_at: Instant,
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

pub(super) enum TicketStatus {
    Accepted,
    Invalid,
    NotFound,
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
        let mut state = self.inner.write().await;
        state.tickets.retain(|_, ticket| ticket.pty_id != id);
        state.sessions.remove(id).is_some()
    }

    pub(super) async fn issue_ticket(&self, id: &str) -> Option<(String, u64)> {
        let mut state = self.inner.write().await;
        if !state.sessions.contains_key(id) {
            return None;
        }
        let now = Instant::now();
        state.tickets.retain(|_, ticket| ticket.expires_at > now);
        let ticket = uuid::Uuid::new_v4().to_string();
        state.tickets.insert(
            ticket.clone(),
            ConnectTicket {
                pty_id: id.to_string(),
                expires_at: now + CONNECT_TICKET_TTL,
            },
        );
        Some((ticket, CONNECT_TICKET_TTL.as_secs()))
    }

    pub(super) async fn consume_ticket(&self, id: &str, ticket: &str) -> TicketStatus {
        let mut state = self.inner.write().await;
        if !state.sessions.contains_key(id) {
            return TicketStatus::NotFound;
        }
        let Some(stored) = state.tickets.remove(ticket) else {
            return TicketStatus::Invalid;
        };
        if stored.pty_id != id || stored.expires_at <= Instant::now() {
            return TicketStatus::Invalid;
        }
        TicketStatus::Accepted
    }
}
