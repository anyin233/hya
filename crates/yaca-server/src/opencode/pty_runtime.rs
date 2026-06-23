use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::RwLock;

use super::pty_state::{PtyEvent, PtyRuntime, PtySession};

const BUFFER_LIMIT: usize = 2 * 1024 * 1024;
const EXITED_LIMIT: usize = 25;

pub(super) fn replay_from(session: &PtySession, from: u64) -> String {
    if session.buffer.is_empty() || from >= session.cursor {
        return String::new();
    }
    let offset = from.saturating_sub(session.buffer_cursor);
    let Ok(offset) = usize::try_from(offset) else {
        return String::new();
    };
    if offset >= session.buffer.len() {
        String::new()
    } else {
        session.buffer[offset..].to_string()
    }
}

pub(super) fn spawn_output_reader<R>(inner: Arc<RwLock<PtyRuntime>>, id: String, mut reader: R)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut buf).await {
                Ok(0) => return,
                Ok(read) => read,
                Err(error) => {
                    tracing::debug!(%error, %id, "pty output reader failed");
                    return;
                }
            };
            let chunk = String::from_utf8_lossy(&buf[..read]).into_owned();
            push_output(&inner, &id, chunk).await;
        }
    });
}

async fn push_output(inner: &Arc<RwLock<PtyRuntime>>, id: &str, chunk: String) {
    let mut state = inner.write().await;
    let Some(session) = state.sessions.get_mut(id) else {
        return;
    };
    session.cursor = session
        .cursor
        .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
    session.buffer.push_str(&chunk);
    trim_buffer(session);
    let _ = session.output.send(PtyEvent::Data(chunk));
}

fn trim_buffer(session: &mut PtySession) {
    if session.buffer.len() <= BUFFER_LIMIT {
        return;
    }
    let excess = session.buffer.len() - BUFFER_LIMIT;
    session.buffer.drain(..excess);
    session.buffer_cursor = session
        .buffer_cursor
        .saturating_add(u64::try_from(excess).unwrap_or(u64::MAX));
}

pub(super) fn spawn_waiter(
    inner: Arc<RwLock<PtyRuntime>>,
    id: String,
    mut child: tokio::process::Child,
) {
    tokio::spawn(async move {
        let status = child.wait().await;
        let code = status
            .ok()
            .and_then(|status| status.code())
            .and_then(|code| u64::try_from(code).ok())
            .unwrap_or(0);
        let mut state = inner.write().await;
        {
            let Some(session) = state.sessions.get_mut(&id) else {
                return;
            };
            session.info.status = "exited";
            session.info.exit_code = Some(code);
            let _ = session.output.send(PtyEvent::End);
        }
        state.exited.push_back(id);
        while state.exited.len() > EXITED_LIMIT {
            let Some(oldest) = state.exited.pop_front() else {
                break;
            };
            state.tickets.retain(|_, ticket| ticket.pty_id != oldest);
            if let Some(session) = state.sessions.remove(&oldest) {
                let _ = session.output.send(PtyEvent::End);
            }
        }
    });
}

pub(super) fn kill_pid(pid: u64) {
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        tokio::spawn(async move {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()
                .await;
        });
    }
}
