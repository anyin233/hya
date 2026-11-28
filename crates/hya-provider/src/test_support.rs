//! Fixtures for protocol tests: local HTTP servers that capture requests,
//! a stub protocol with any kind label, and the canonical decoding check
//! every protocol must pass.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hya_proto::{Event, MessageId, SessionId, TokenUsage};
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::{
    CompletionRequest, Decoder, OpenAiChatProtocol, Protocol, ProviderError, ReasoningEffort,
};

/// One HTTP request a test server received.
#[derive(Debug)]
pub struct CapturedRequest {
    /// Request line, headers, and body as received.
    pub raw: String,
    /// Request line and headers (up to and including the blank line).
    pub headers: String,
    /// Request body.
    pub body: String,
}

fn content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().unwrap())
        })
        .unwrap_or(0)
}

/// Read one request (headers and `content-length` body) from `socket`.
pub async fn read_full_request(socket: &mut TcpStream) -> CapturedRequest {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n != 0, "socket closed before request headers");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let body_len = content_length(&headers);
    while buf.len() < header_end + body_len {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n != 0, "socket closed before request body");
        buf.extend_from_slice(&chunk[..n]);
    }
    let body_end = header_end + body_len;
    CapturedRequest {
        raw: String::from_utf8_lossy(&buf[..body_end]).to_string(),
        headers,
        body: String::from_utf8_lossy(&buf[header_end..body_end]).to_string(),
    }
}

/// Wait (at most 3s) for the request a [`start_sse_server`] captured.
pub async fn captured_request(request_rx: oneshot::Receiver<CapturedRequest>) -> CapturedRequest {
    tokio::time::timeout(Duration::from_secs(3), request_rx)
        .await
        .unwrap()
        .unwrap()
}

/// Serve one request with an SSE `200 OK` whose body is `response`; returns
/// the base URL and the captured request.
pub async fn start_sse_server(response: String) -> (String, oneshot::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let captured = read_full_request(&mut socket).await;
        let _ = tx.send(captured);
        let _ = socket
            .write_all(
                format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{response}")
                    .as_bytes(),
            )
            .await;
    });
    (format!("http://{addr}"), rx)
}

/// Serve one connection per entry of `responses` (raw HTTP responses), in
/// order; returns the base URL, the connection count, and the captured requests.
pub async fn start_scripted_server(
    responses: Vec<String>,
) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sc = Arc::clone(&connections);
    let sr = Arc::clone(&requests);
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            sc.fetch_add(1, Ordering::SeqCst);
            let captured = read_full_request(&mut socket).await;
            sr.lock().unwrap().push(captured);
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{addr}"), connections, requests)
}

struct StubProtocol(String);

impl Protocol for StubProtocol {
    fn kind(&self) -> &str {
        &self.0
    }

    fn description(&self) -> &str {
        "stub protocol (OpenAI Chat wire)"
    }

    fn endpoint_url(&self, base: &str, model: &str) -> String {
        OpenAiChatProtocol.endpoint_url(base, model)
    }

    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        OpenAiChatProtocol.encode(req)
    }

    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn Decoder> {
        OpenAiChatProtocol.decoder(session, message, reasoning)
    }
}

/// A protocol registered under `label` that speaks the OpenAI Chat wire.
#[must_use]
pub fn stub_protocol(label: &str) -> Arc<dyn Protocol> {
    Arc::new(StubProtocol(label.to_string()))
}

/// Decode SSE data `frames` with a fresh decoder of `protocol`.
pub fn decode_frames(protocol: &dyn Protocol, frames: &[&str]) -> Vec<Event> {
    let mut decoder = protocol.decoder(SessionId::new(), MessageId::new(), None);
    let mut out = Vec::new();
    for frame in frames {
        out.extend(decoder.push(frame).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

/// One line per event: the shape canonical decoding is compared on.
#[must_use]
pub fn summarize(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::TextStart { .. } => "text_start".to_string(),
            Event::TextDelta { delta, .. } => format!("text_delta:{delta}"),
            Event::TextEnd { .. } => "text_end".to_string(),
            Event::ReasoningStart { .. } => "reasoning_start".to_string(),
            Event::ReasoningDelta { delta, .. } => format!("reasoning_delta:{delta}"),
            Event::ReasoningEnd { .. } => "reasoning_end".to_string(),
            Event::ToolInputStart { name, .. } => format!("tool_start:{name}"),
            Event::ToolInputDelta { .. } => "tool_input_delta".to_string(),
            Event::ToolCallRequested { name, input, .. } => format!("tool_call:{name}:{input}"),
            Event::MessageFinished { finish, .. } => format!("finish:{finish:?}"),
            other => format!("other:{other:?}"),
        })
        .collect()
}

/// Assert that `protocol` decodes into the canonical event shapes:
/// `text_frames` answer "Hello world" and stop, `tool_frames` call `read`
/// with `{"path":"/tmp/a"}`, and `usage_frames` finish with `usage`.
pub fn assert_canonical_text_tool_usage(
    protocol: &dyn Protocol,
    text_frames: &[&str],
    tool_frames: &[&str],
    usage_frames: &[&str],
    usage: TokenUsage,
) {
    let kind = protocol.kind();
    let text = summarize(&decode_frames(protocol, text_frames));
    let answer: String = text
        .iter()
        .filter_map(|line| line.strip_prefix("text_delta:"))
        .collect();
    assert_eq!(answer, "Hello world", "{kind} text");
    assert_eq!(
        text.first().map(String::as_str),
        Some("text_start"),
        "{kind}"
    );
    assert_eq!(
        text.iter()
            .rev()
            .take(2)
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["finish:Stop", "text_end"],
        "{kind} text"
    );

    let tool = summarize(&decode_frames(protocol, tool_frames));
    assert_eq!(
        tool.first().map(String::as_str),
        Some("tool_start:read"),
        "{kind}"
    );
    assert_eq!(
        tool.iter()
            .rev()
            .take(2)
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["finish:ToolCalls", "tool_call:read:{\"path\":\"/tmp/a\"}"],
        "{kind} tool"
    );

    let finished =
        decode_frames(protocol, usage_frames)
            .into_iter()
            .find_map(|event| match event {
                Event::MessageFinished { tokens, .. } => tokens,
                _ => None,
            });
    assert_eq!(finished, Some(usage), "{kind} usage");
}
