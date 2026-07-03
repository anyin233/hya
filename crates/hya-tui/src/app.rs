use std::time::Duration;

use hya_sdk::GlobalEvent;
use tokio::sync::mpsc;

use crate::contracts::KeyEvent;
use crate::state::AppState;

pub mod runtime;
pub use runtime::{prompt_request_body, run_tui, AppRunError, RunTuiInput};

pub(crate) mod panes;

#[cfg(test)]
mod harness;

const BATCH_WINDOW: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    ScrollUp,
    ScrollDown,
    Press,
    Other,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    Key(KeyEvent),
    Paste(String),
    Resize(u16, u16),
    Mouse {
        column: u16,
        row: u16,
        kind: MouseKind,
    },
    Sse(GlobalEvent),
    Tick,
    Quit,
    BackendReady,
    /// (agent name, optional provider/model) pairs + the default agent name.
    AgentList(Vec<(String, Option<(String, String)>)>, Option<String>),
    Navigate(String),
    LoadSession(String),
    FileMatches(Vec<String>),
    CommandList(Vec<String>),
    ModelList(Vec<(String, String, String, i64, Vec<String>)>),
    SessionList(Vec<(String, String)>),
    TimelineList(Vec<(String, String, String)>),
    McpStatus(Vec<(String, String)>),
    LspStatus(Vec<(String, String, String)>),
    FormatterStatus(Vec<String>),
    PluginList(Vec<(String, Option<String>)>),
    CopyToClipboard(String),
    CopySessionTranscriptToClipboard(String),
    Toast(String),
    Internal(String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    pub events_applied: usize,
    pub batches: usize,
}

pub async fn run(mut rx: mpsc::UnboundedReceiver<AppEvent>, mut state: AppState) -> RunStats {
    let mut stats = RunStats::default();
    loop {
        let Some(first) = next_event(&mut rx).await else {
            break;
        };
        let batch = collect_batch(first, &mut rx).await;
        let should_quit = apply_batch(&mut state, &batch, &mut stats).await;
        if should_quit {
            break;
        }
    }
    stats
}

async fn next_event(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> Option<AppEvent> {
    tokio::select! {
        event = rx.recv() => event,
    }
}

async fn collect_batch(
    first: AppEvent,
    rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Vec<AppEvent> {
    let mut batch = vec![first];
    drain_ready(rx, &mut batch);

    let deadline = tokio::time::sleep(BATCH_WINDOW);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            () = &mut deadline => break,
            event = rx.recv() => {
                match event {
                    Some(event) => {
                        batch.push(event);
                        drain_ready(rx, &mut batch);
                    }
                    None => break,
                }
            }
        }
    }
    batch
}

fn drain_ready(rx: &mut mpsc::UnboundedReceiver<AppEvent>, batch: &mut Vec<AppEvent>) {
    while let Ok(event) = rx.try_recv() {
        batch.push(event);
    }
}

async fn apply_batch(state: &mut AppState, batch: &[AppEvent], stats: &mut RunStats) -> bool {
    let mut applied = 0usize;
    let mut should_quit = false;
    for event in batch {
        match event {
            AppEvent::Key(_)
            | AppEvent::Paste(_)
            | AppEvent::Resize(_, _)
            | AppEvent::Mouse { .. }
            | AppEvent::Tick => {
                applied += 1;
            }
            AppEvent::BackendReady
            | AppEvent::AgentList(_, _)
            | AppEvent::Navigate(_)
            | AppEvent::LoadSession(_)
            | AppEvent::FileMatches(_)
            | AppEvent::CommandList(_)
            | AppEvent::ModelList(_)
            | AppEvent::SessionList(_)
            | AppEvent::TimelineList(_)
            | AppEvent::McpStatus(_)
            | AppEvent::LspStatus(_)
            | AppEvent::FormatterStatus(_)
            | AppEvent::PluginList(_)
            | AppEvent::CopyToClipboard(_)
            | AppEvent::CopySessionTranscriptToClipboard(_)
            | AppEvent::Toast(_)
            | AppEvent::Internal(_) => {
                applied += 1;
            }
            AppEvent::Sse(event) => {
                if apply_sse(state, event).await {
                    applied += 1;
                }
            }
            AppEvent::Quit => {
                should_quit = true;
            }
        }
    }
    if applied > 0 {
        stats.events_applied += applied;
        stats.batches += 1;
    }
    should_quit
}

async fn apply_sse(state: &mut AppState, event: &GlobalEvent) -> bool {
    if event.is_sync_envelope() || event.is_heartbeat() {
        return false;
    }
    let mut store = state.data.write().await;
    store.apply_event(event)
}
