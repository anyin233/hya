#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use futures::StreamExt as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use yaca_proto::{MessageId, ModelRef, SessionId};
use yaca_provider::{CompletionRequest, HttpProvider, Provider as _, ProviderKind};

#[tokio::test]
async fn http_provider_forwards_completion_request_headers() {
    let (base_url, request_rx) = start_sse_server().await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        "test-token".to_string(),
        ["gpt-5".to_string()],
    )
    .unwrap();
    let mut headers = BTreeMap::new();
    headers.insert("x-yaca-session".to_string(), "session-headers".to_string());

    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers,
    };

    let stream = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap();
    let events: Vec<_> = stream.collect().await;
    let raw_request = request_rx.await.unwrap();

    assert!(events.iter().all(Result::is_ok));
    assert!(raw_request.contains("authorization: Bearer test-token"));
    assert!(raw_request.contains("x-yaca-session: session-headers"));
}

async fn start_sse_server() -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0_u8; 8192];
        let mut read = 0_usize;
        loop {
            let n = socket.read(&mut buf[read..]).await.unwrap();
            if n == 0 {
                break;
            }
            read += n;
            if buf[..read].windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let raw = String::from_utf8_lossy(&buf[..read]).to_string();
        request_tx.send(raw).unwrap();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n",
            )
            .await
            .unwrap();
    });
    (format!("http://{addr}"), request_rx)
}
