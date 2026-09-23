//! In-stream provider error frames (HTTP 200 + an SSE `error` payload):
//! classification and the zero-event replay window.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt as _;
use hya_proto::{Event, MessageId, ModelRef, SessionId};
use hya_provider::{
    CompletionRequest, HttpProvider, Provider as _, ProviderError, ProviderKind, RetryConfig,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::time::timeout;

const CONCURRENCY_MESSAGE: &str = "Concurrency limit exceeded for account, please retry later";

fn sse_response(frames: &[String]) -> String {
    let body: String = frames.iter().map(|frame| format!("{frame}\n\n")).collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

fn anthropic_error_frame(kind: &str, message: &str) -> String {
    format!(
        "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"{kind}\",\"message\":\"{message}\"}}}}"
    )
}

fn anthropic_success_frames(text: &str) -> Vec<String> {
    vec![
        r#"data: {"type":"message_start","message":{}}"#.to_string(),
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#
            .to_string(),
        format!(
            r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{text}"}}}}"#
        ),
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#.to_string(),
        r#"data: {"type":"message_stop"}"#.to_string(),
    ]
}

fn anthropic_provider(base_url: &str, max_attempts: usize) -> HttpProvider {
    HttpProvider::new(
        "12th",
        ProviderKind::Anthropic,
        base_url,
        Some("test-token".to_string()),
        ["claude-sonnet-4".to_string()],
    )
    .unwrap()
    .with_retry(RetryConfig {
        max_attempts,
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
    })
}

fn request() -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new("claude-sonnet-4"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

async fn collect(provider: &HttpProvider) -> Vec<Result<Event, ProviderError>> {
    timeout(Duration::from_secs(15), async {
        provider
            .stream(request(), SessionId::new(), MessageId::new())
            .await
            .expect("the first response is a 200 stream")
            .collect::<Vec<_>>()
            .await
    })
    .await
    .expect("stream should finish within the guard")
}

fn text_of(events: &[Result<Event, ProviderError>]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Ok(Event::TextDelta { delta, .. }) => Some(delta.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn in_stream_rate_limit_before_any_event_is_retried_and_succeeds() {
    let (base_url, connections) = start_scripted_server(vec![
        sse_response(&[anthropic_error_frame(
            "rate_limit_error",
            CONCURRENCY_MESSAGE,
        )]),
        sse_response(&anthropic_success_frames("recovered")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 5);

    let events = collect(&provider).await;

    assert!(events.iter().all(Result::is_ok), "events: {events:?}");
    assert_eq!(text_of(&events), "recovered");
    assert_eq!(
        connections.load(Ordering::SeqCst),
        2,
        "one retry, then success"
    );
}

#[tokio::test]
async fn untyped_gateway_retry_later_error_before_any_event_is_retried() {
    // The observed gateway shape: no `type`, only a message asking to retry.
    let untyped = format!("data: {{\"error\":{{\"message\":\"{CONCURRENCY_MESSAGE}\"}}}}");
    let (base_url, connections) = start_scripted_server(vec![
        sse_response(&[untyped]),
        sse_response(&anthropic_success_frames("ok")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 5);

    let events = collect(&provider).await;

    assert!(events.iter().all(Result::is_ok), "events: {events:?}");
    assert_eq!(text_of(&events), "ok");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn in_stream_overloaded_before_any_event_is_retried() {
    let (base_url, connections) = start_scripted_server(vec![
        sse_response(&[anthropic_error_frame("overloaded_error", "Overloaded")]),
        sse_response(&anthropic_success_frames("ok")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 3);

    let events = collect(&provider).await;

    assert!(events.iter().all(Result::is_ok), "events: {events:?}");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn in_stream_rate_limit_after_a_delivered_delta_surfaces_without_retry() {
    let mut frames = anthropic_success_frames("partial");
    frames.truncate(3); // message_start, content_block_start, one delta
    frames.push(anthropic_error_frame(
        "rate_limit_error",
        CONCURRENCY_MESSAGE,
    ));
    let (base_url, connections) = start_scripted_server(vec![
        sse_response(&frames),
        sse_response(&anthropic_success_frames("must not be replayed")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 5);

    let events = collect(&provider).await;

    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "a delivered event closes the replay window"
    );
    assert_eq!(text_of(&events), "partial");
    let errors: Vec<_> = events.iter().filter_map(|e| e.as_ref().err()).collect();
    assert_eq!(errors.len(), 1, "exactly one terminal error: {events:?}");
    assert!(
        matches!(events.last(), Some(Err(ProviderError::HttpStatus { status: 429, message, .. })) if message.contains(CONCURRENCY_MESSAGE)),
        "events: {events:?}"
    );
}

#[tokio::test]
async fn non_retryable_in_stream_error_is_not_retried() {
    let (base_url, connections) = start_scripted_server(vec![
        sse_response(&[anthropic_error_frame(
            "invalid_request_error",
            "messages: field required",
        )]),
        sse_response(&anthropic_success_frames("must not be requested")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 5);

    let events = collect(&provider).await;

    assert_eq!(connections.load(Ordering::SeqCst), 1);
    assert!(
        matches!(
            events.as_slice(),
            [Err(ProviderError::HttpStatus { status: 400, .. })]
        ),
        "events: {events:?}"
    );
    assert!(!events[0].as_ref().unwrap_err().is_retryable_before_stream());
}

#[tokio::test]
async fn in_stream_retries_respect_max_attempts() {
    let rate_limited = sse_response(&[anthropic_error_frame(
        "rate_limit_error",
        CONCURRENCY_MESSAGE,
    )]);
    let (base_url, connections) = start_scripted_server(vec![
        rate_limited.clone(),
        rate_limited.clone(),
        rate_limited,
        sse_response(&anthropic_success_frames("must not be requested")),
    ])
    .await;
    let provider = anthropic_provider(&base_url, 3);

    let events = collect(&provider).await;

    assert_eq!(
        connections.load(Ordering::SeqCst),
        3,
        "the shared attempt budget bounds in-stream retries"
    );
    assert!(
        matches!(
            events.as_slice(),
            [Err(ProviderError::HttpStatus { status: 429, .. })]
        ),
        "events: {events:?}"
    );
}

#[tokio::test]
async fn responses_failed_with_server_error_code_before_any_event_is_retried() {
    let failed = r#"data: {"type":"response.failed","response":{"error":{"code":"server_error","message":"The server had an error"}}}"#.to_string();
    let completed = [
        r#"data: {"type":"response.output_text.delta","output_index":0,"delta":"done"}"#
            .to_string(),
        r#"data: {"type":"response.completed","response":{}}"#.to_string(),
    ];
    let (base_url, connections) =
        start_scripted_server(vec![sse_response(&[failed]), sse_response(&completed)]).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap()
    .with_retry(RetryConfig {
        max_attempts: 3,
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
    });

    let events = timeout(Duration::from_secs(15), async {
        provider
            .stream(
                CompletionRequest {
                    model: ModelRef::new("gpt-5"),
                    ..request()
                },
                SessionId::new(),
                MessageId::new(),
            )
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    })
    .await
    .unwrap();

    assert!(events.iter().all(Result::is_ok), "events: {events:?}");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

/// Sequential scripted-response server: connection N receives `responses[N]`.
async fn start_scripted_server(responses: Vec<String>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            server_connections.fetch_add(1, Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            let header_end = loop {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n != 0, "socket closed before request headers");
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
            let body_len = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + body_len {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n != 0, "socket closed before request body");
                buf.extend_from_slice(&chunk[..n]);
            }
            socket.write_all(response.as_bytes()).await.unwrap();
            let _ = socket.shutdown().await;
        }
    });
    (format!("http://{addr}"), connections)
}
