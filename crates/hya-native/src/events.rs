//! In-process `/global/event` bridge.
//!
//! Oneshots the SSE route on the in-process hya `Router` and drains its streaming body in-process,
//! reusing the backend's `Envelope -> GlobalEvent` projection (`hya-server/src/compat/event.rs`)
//! and hya-sdk's `GlobalEvent` decoder. No TCP, no reqwest. The bridge resolves the `oneshot`
//! immediately (the `Sse` handler returns a streaming body) and then forwards frames lazily as the
//! backend publishes them — exactly mirroring the HTTP SSE path in `crates/hya/src/main.rs`.

use axum::body::Body;
use axum::http::Request;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tower::ServiceExt;

use hya_sdk::{GlobalEvent, DIRECTORY_HEADER};

/// Spawn a task that subscribes to the in-process `GET /global/event` SSE stream and forwards each
/// decoded [`GlobalEvent`] to `tx`. Reconnects on stream end with a short backoff; ends when `tx`
/// is closed (the receiver dropped) or a request/response cannot be constructed.
#[must_use]
pub fn spawn_event_bridge(
    router: axum::Router,
    directory: String,
    tx: mpsc::UnboundedSender<GlobalEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let request = match Request::builder()
                .method("GET")
                .uri("/global/event")
                .header(DIRECTORY_HEADER, &directory)
                .body(Body::empty())
            {
                Ok(request) => request,
                Err(_) => break,
            };

            let response = match router.clone().oneshot(request).await {
                Ok(response) => response,
                Err(_) => break,
            };

            // `into_data_stream()` yields `Result<Bytes, _>` — exactly what `eventsource-stream` wants
            // (the same shape `hya_sdk::events` feeds from `reqwest::bytes_stream()`).
            let mut stream = response.into_body().into_data_stream().eventsource();
            while let Some(event) = stream.next().await {
                let event = match event {
                    Ok(event) => event,
                    Err(_) => break,
                };
                if event.data.is_empty() {
                    continue;
                }
                // Tolerate undecodable frames (e.g. the broadcast-lag `resync` marker) — forward-compat.
                if let Ok(global) = serde_json::from_str::<GlobalEvent>(&event.data) {
                    if tx.send(global).is_err() {
                        return; // receiver gone → stop the bridge
                    }
                }
            }
            // Body ended (shutdown or transient); brief backoff, then re-subscribe.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hya_app::{HyaRuntime, RuntimeOptions};
    use std::time::Duration;

    async fn offline_runtime() -> HyaRuntime {
        HyaRuntime::start(RuntimeOptions {
            model: None,
            db: String::new(),
            yolo: true,
            default_agent: None,
            include_global_agents: false,
            force_offline: true,
        })
        .await
        .expect("offline runtime should start")
    }

    #[tokio::test]
    async fn bridge_emits_connected_first() {
        let rt = offline_runtime().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _bridge = spawn_event_bridge(rt.router().clone(), "/tmp".to_owned(), tx);
        let first = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a frame should arrive within 5s")
            .expect("channel should yield the first frame");
        assert_eq!(
            first.payload.kind, "server.connected",
            "expected the first bridged frame to be server.connected, got {}",
            first.payload.kind
        );
    }

    #[tokio::test]
    async fn bridge_stops_when_receiver_dropped() {
        let rt = offline_runtime().await;
        let (tx, rx) = mpsc::unbounded_channel();
        let bridge = spawn_event_bridge(rt.router().clone(), "/tmp".to_owned(), tx);
        drop(rx); // receiver gone
                  // The bridge must observe the closed channel on its next send and return (not hang).
        tokio::time::timeout(Duration::from_secs(5), bridge)
            .await
            .expect("bridge task should finish promptly after the receiver is dropped")
            .expect("bridge task should not panic");
    }

    // Long-running guard for the §6.3 fallback trigger: proves the SSE heartbeat keeps the oneshot
    // body open past the connected frame. Run with `--ignored`.
    #[tokio::test]
    #[ignore = "long-running (~10-35s): validates the SSE heartbeat keeps the body open"]
    async fn bridge_holds_open_for_heartbeat() {
        let rt = offline_runtime().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _bridge = spawn_event_bridge(rt.router().clone(), "/tmp".to_owned(), tx);
        let _connected = rx.recv().await; // server.connected
        let next = tokio::time::timeout(Duration::from_secs(35), rx.recv()).await;
        assert!(
            next.is_ok(),
            "expected at least one heartbeat frame within 35s"
        );
    }
}
