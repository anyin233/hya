use eventsource_stream::Eventsource as _;
use futures::StreamExt as _;
use hya_proto::Event;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{Decoder, ProviderError};

pub(super) async fn pump(
    resp: reqwest::Response,
    mut decoder: Box<dyn Decoder>,
    tx: mpsc::Sender<Result<Event, ProviderError>>,
) {
    let mut sse = resp.bytes_stream().eventsource();
    while let Some(frame) = sse.next().await {
        let frame = match frame {
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
