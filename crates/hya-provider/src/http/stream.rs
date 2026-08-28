use eventsource_stream::Eventsource as _;
use futures::StreamExt as _;
use hya_proto::Event;
use serde_json::Value;
use tokio::{sync::mpsc, time::timeout};

use crate::{Decoder, ProviderError};

pub(super) async fn pump(
    resp: reqwest::Response,
    mut decoder: Box<dyn Decoder>,
    tx: mpsc::Sender<Result<Event, ProviderError>>,
    idle_timeout: std::time::Duration,
) {
    let mut sse = resp.bytes_stream().eventsource();
    loop {
        // The window opens at headers (first event) and resets on every frame
        // (inter-event silence). A miss is post-stream under the no-replay
        // boundary: it surfaces once here and is never retried or failed over.
        let next = match timeout(idle_timeout, sse.next()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_elapsed) => {
                let _ = tx
                    .send(Err(ProviderError::Http(format!(
                        "stalled stream: no SSE frame within {idle_timeout:#?}"
                    ))))
                    .await;
                return;
            }
        };
        let frame = match next {
            Ok(f) => f,
            Err(e) => {
                let _ = tx.send(Err(ProviderError::Http(e.to_string()))).await;
                return;
            }
        };
        if frame.data.contains("\"error\"")
            && let Ok(value) = serde_json::from_str::<Value>(&frame.data)
            && let Some(err) = value.get("error")
        {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("provider returned an error");
            let _ = tx.send(Err(ProviderError::Http(msg.to_string()))).await;
            return;
        }
        match decoder.push(&frame.data) {
            Ok(events) => {
                for event in events {
                    if tx.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                return;
            }
        }
    }
    match decoder.finish() {
        Ok(events) => {
            for event in events {
                if tx.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        }
        Err(e) => {
            let _ = tx.send(Err(e)).await;
        }
    }
}
