use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use hya_sdk::{stream_global_events, Client, GlobalEvent};
use hya_tui::app::AppEvent;
use tokio::sync::mpsc;

/// Forward `GlobalEvent`s from the native bridge into the TUI event loop as `AppEvent::Sse`,
/// matching the shape the HTTP SSE path delivers.
pub(crate) fn forward_events(
    mut events: mpsc::UnboundedReceiver<GlobalEvent>,
    tx: mpsc::UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            if tx.send(AppEvent::Sse(event)).is_err() {
                break;
            }
        }
    });
}

const COMMAND_FETCH_ATTEMPTS: u32 = 5;
const COMMAND_FETCH_RETRY_DELAY: Duration = Duration::from_secs(2);

pub(crate) fn spawn_background_fetches(
    client: &Arc<dyn Client>,
    tx: &mpsc::UnboundedSender<AppEvent>,
) {
    let command_client = Arc::clone(client);
    let command_tx = tx.clone();
    tokio::spawn(async move {
        for attempt in 0..COMMAND_FETCH_ATTEMPTS {
            match command_client.commands().await {
                Ok(names) => {
                    let _ = command_tx.send(AppEvent::CommandList(names));
                    return;
                }
                Err(_) if attempt + 1 < COMMAND_FETCH_ATTEMPTS => {
                    tokio::time::sleep(COMMAND_FETCH_RETRY_DELAY).await;
                }
                Err(error) => {
                    let _ = command_tx.send(AppEvent::Toast(format!(
                        "slash commands unavailable: {error}"
                    )));
                }
            }
        }
    });

    let model_client = Arc::clone(client);
    let model_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(models) = model_client.models().await {
            let _ = model_tx.send(AppEvent::ModelList(models));
        }
    });

    let mcp_client = Arc::clone(client);
    let mcp_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(status) = mcp_client.mcp_status().await {
                if mcp_tx.send(AppEvent::McpStatus(status)).is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let lsp_client = Arc::clone(client);
    let lsp_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(status) = lsp_client.lsp_status().await {
                if lsp_tx.send(AppEvent::LspStatus(status)).is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let formatter_client = Arc::clone(client);
    let formatter_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(status) = formatter_client.formatter_status().await {
            let _ = formatter_tx.send(AppEvent::FormatterStatus(status));
        }
    });

    let plugin_client = Arc::clone(client);
    let plugin_tx = tx.clone();
    tokio::spawn(async move {
        if let Ok(plugins) = plugin_client.plugins().await {
            let _ = plugin_tx.send(AppEvent::PluginList(plugins));
        }
    });
}

pub(crate) fn spawn_sse_task(
    http: reqwest::Client,
    base_url: String,
    directory: String,
    tx: mpsc::UnboundedSender<AppEvent>,
    keep_streaming: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let _ = stream_global_events(&http, &base_url, &directory, |event| {
                let keep_running = keep_streaming.load(Ordering::SeqCst);
                let sent = tx.send(AppEvent::Sse(event)).is_ok();
                keep_running && sent
            })
            .await;
            if !keep_streaming.load(Ordering::SeqCst) {
                break;
            }
            let _ = tx.send(AppEvent::Toast("Reconnecting to server\u{2026}".to_owned()));
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    })
}
