use std::collections::{BTreeMap, VecDeque};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::process::{ChildStdin, Command};
use tokio::sync::{Mutex, RwLock, broadcast};

use super::pty_runtime::{kill_pid, replay_from, spawn_output_reader, spawn_waiter};

const CONNECT_TICKET_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub(crate) struct PtyState {
    inner: Arc<RwLock<PtyRuntime>>,
}

#[derive(Default)]
pub(super) struct PtyRuntime {
    next: u64,
    pub(super) sessions: BTreeMap<String, PtySession>,
    pub(super) exited: VecDeque<String>,
    pub(super) tickets: BTreeMap<String, ConnectTicket>,
}

pub(super) struct PtySession {
    pub(super) info: PtyInfo,
    pub(super) stdin: Arc<Mutex<ChildStdin>>,
    pub(super) buffer: String,
    pub(super) buffer_cursor: u64,
    pub(super) cursor: u64,
    pub(super) output: broadcast::Sender<PtyEvent>,
}

pub(super) struct ConnectTicket {
    pub(super) pty_id: String,
    expires_at: Instant,
}

#[derive(Clone, Serialize)]
pub(super) struct PtyInfo {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) command: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: String,
    pub(super) status: &'static str,
    pub(super) pid: u64,
    #[serde(rename = "exitCode", skip_serializing_if = "Option::is_none")]
    pub(super) exit_code: Option<u64>,
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

#[derive(Clone)]
pub(super) enum PtyEvent {
    Data(String),
    End,
}

pub(super) struct PtyAttachment {
    pub(super) replay: String,
    pub(super) cursor: u64,
    pub(super) events: broadcast::Receiver<PtyEvent>,
}

impl PtyState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(super) async fn list(&self) -> Vec<PtyInfo> {
        self.inner
            .read()
            .await
            .sessions
            .values()
            .map(|session| session.info.clone())
            .collect()
    }

    pub(super) async fn create(&self, payload: CreatePayload) -> Result<PtyInfo, String> {
        std::fs::create_dir_all(&payload.cwd).map_err(|e| e.to_string())?;
        let mut command = Command::new(&payload.command);
        command
            .args(&payload.args)
            .current_dir(&payload.cwd)
            .env("TERM", "xterm-256color")
            .env("OPENCODE_TERMINAL", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "pty stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "pty stdout unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "pty stderr unavailable".to_string())?;
        let pid = child.id().map(u64::from).unwrap_or(0);
        let (output, _) = broadcast::channel(256);
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
            pid,
            exit_code: None,
        };
        state.sessions.insert(
            id.clone(),
            PtySession {
                info: info.clone(),
                stdin: Arc::new(Mutex::new(stdin)),
                buffer: String::new(),
                buffer_cursor: 0,
                cursor: 0,
                output,
            },
        );
        drop(state);
        spawn_output_reader(Arc::clone(&self.inner), id.clone(), stdout);
        spawn_output_reader(Arc::clone(&self.inner), id.clone(), stderr);
        spawn_waiter(Arc::clone(&self.inner), id, child);
        Ok(info)
    }

    pub(super) async fn get(&self, id: &str) -> Option<PtyInfo> {
        self.inner
            .read()
            .await
            .sessions
            .get(id)
            .map(|session| session.info.clone())
    }

    pub(super) async fn update(&self, id: &str, payload: UpdatePayload) -> Option<PtyInfo> {
        let mut state = self.inner.write().await;
        let session = state.sessions.get_mut(id)?;
        if let Some(title) = payload.title {
            session.info.title = title;
        }
        Some(session.info.clone())
    }

    pub(super) async fn remove(&self, id: &str) -> bool {
        let mut state = self.inner.write().await;
        state.tickets.retain(|_, ticket| ticket.pty_id != id);
        state.exited.retain(|exited| exited != id);
        let Some(session) = state.sessions.remove(id) else {
            return false;
        };
        let _ = session.output.send(PtyEvent::End);
        kill_pid(session.info.pid);
        true
    }

    pub(super) async fn issue_ticket(&self, id: &str) -> Option<(String, u64)> {
        let mut state = self.inner.write().await;
        if !matches!(
            state.sessions.get(id).map(|session| session.info.status),
            Some("running")
        ) {
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
        if !matches!(
            state.sessions.get(id).map(|session| session.info.status),
            Some("running")
        ) {
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

    pub(super) async fn attach(&self, id: &str, cursor: Option<i64>) -> Option<PtyAttachment> {
        let state = self.inner.read().await;
        let session = state.sessions.get(id)?;
        if session.info.status != "running" {
            return None;
        }
        let events = session.output.subscribe();
        let end = session.cursor;
        let from = match cursor {
            Some(-1) => end,
            Some(value) if value >= 0 => u64::try_from(value).unwrap_or(0),
            _ => 0,
        };
        let replay = replay_from(session, from);
        Some(PtyAttachment {
            replay,
            cursor: end,
            events,
        })
    }

    pub(super) async fn write(&self, id: &str, data: &str) -> bool {
        let stdin = {
            let state = self.inner.read().await;
            let Some(session) = state.sessions.get(id) else {
                return false;
            };
            Arc::clone(&session.stdin)
        };
        stdin.lock().await.write_all(data.as_bytes()).await.is_ok()
    }
}
