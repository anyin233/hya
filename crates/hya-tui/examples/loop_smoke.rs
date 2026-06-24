use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use hya_sdk::{stream_global_events, Client, HttpClient, ServerHandle};
use hya_tui::{
    app::{run, AppEvent},
    state::AppState,
};
use tokio::sync::mpsc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::current_dir()?.display().to_string();
    let server = ServerHandle::spawn(&dir).await?;
    let http = reqwest::Client::new();
    let (tx, rx) = mpsc::unbounded_channel();
    let loop_task = tokio::spawn(run(rx, AppState::default()));

    let keep_streaming = Arc::new(AtomicBool::new(true));
    let stream_flag = Arc::clone(&keep_streaming);
    let stream_tx = tx.clone();
    let stream_http = http.clone();
    let stream_base = server.base_url().to_string();
    let stream_dir = dir.clone();
    let sse_task = tokio::spawn(async move {
        stream_global_events(&stream_http, &stream_base, &stream_dir, |event| {
            let keep_running = stream_flag.load(Ordering::SeqCst);
            let sent = stream_tx.send(AppEvent::Sse(event)).is_ok();
            keep_running && sent
        })
        .await
    });

    let activity = HttpClient::with_http(http, server.base_url(), &dir);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _session = activity.session_create().await?;
    for _ in 0..24 {
        tx.send(AppEvent::Tick)?;
    }
    tokio::time::sleep(Duration::from_secs(5)).await;

    keep_streaming.store(false, Ordering::SeqCst);
    tx.send(AppEvent::Quit)?;
    let stats = loop_task.await?;

    drop(server);
    if let Ok(joined) = tokio::time::timeout(Duration::from_secs(2), sse_task).await {
        let _ = joined?;
    }

    println!(
        "events_applied={} batches={}",
        stats.events_applied, stats.batches
    );
    Ok(())
}
