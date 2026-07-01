#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeMap, time::Duration};

use futures::StreamExt as _;
use hya_proto::{
    Event, Message, MessageId, ModelRef, Part, PartId, SessionId, ToolName, ToolSchema,
};
use hya_provider::{CompletionRequest, HttpProvider, Provider as _, ProviderKind};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[derive(Debug)]
struct CapturedRequest {
    raw: String,
    headers: String,
    body: String,
}

#[tokio::test]
async fn http_provider_forwards_completion_request_headers() {
    let (base_url, request_rx) = start_sse_server("data: [DONE]\n\n".to_string()).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        "test-token".to_string(),
        ["gpt-5".to_string()],
    )
    .unwrap();
    let mut headers = BTreeMap::new();
    headers.insert("x-hya-session".to_string(), "session-headers".to_string());

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
    let request = captured_request(request_rx).await;
    let headers = request.headers.to_ascii_lowercase();

    assert!(events.iter().all(Result::is_ok));
    assert!(headers.contains("authorization: bearer test-token"));
    assert!(headers.contains("x-hya-session: session-headers"));
}

#[tokio::test]
async fn http_provider_posts_openai_compatible_body_to_mock_endpoint() {
    let mock_text = "mock openai delta";
    let response = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{mock_text}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
    );
    let (base_url, request_rx) = start_sse_server(response).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        "test-token".to_string(),
        ["gpt-5".to_string()],
    )
    .unwrap();

    let req = CompletionRequest {
        model: ModelRef::new("openai/gpt-5"),
        system: Some("be terse".to_string()),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "hello provider".to_string(),
            }],
        }],
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            output_schema: None,
        }],
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events: Vec<_> = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    let request = captured_request(request_rx).await;
    let headers = request.headers.to_ascii_lowercase();
    let body: Value = serde_json::from_str(&request.body).unwrap();
    let text_deltas: Vec<_> = events
        .into_iter()
        .map(Result::unwrap)
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } => Some(delta),
            _ => None,
        })
        .collect();

    assert!(headers.contains("authorization: bearer test-token"));
    assert!(
        request
            .raw
            .starts_with("POST /chat/completions HTTP/1.1\r\n")
    );
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hello provider"}
        ])
    );
    assert_eq!(
        body["tools"],
        json!([
            {
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "read a file",
                    "parameters": {
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }
                }
            }
        ])
    );
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert!(text_deltas.iter().any(|delta| delta == mock_text));
}

#[tokio::test]
async fn http_provider_posts_anthropic_compatible_body_to_mock_endpoint() {
    let mock_text = "mock anthropic delta";
    let response = [
        r#"data: {"type":"message_start","message":{}}"#.to_string(),
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#.to_string(),
        format!(
            r#"data: {{"type":"content_block_delta","index":0,"delta":{{"type":"text_delta","text":"{}"}}}}"#,
            mock_text,
        ),
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#.to_string(),
        r#"data: {"type":"message_stop"}"#.to_string(),
    ]
    .join("\n\n")
        + "\n\n";
    let (base_url, request_rx) = start_sse_server(response).await;
    let provider = HttpProvider::new(
        "anthropic",
        ProviderKind::Anthropic,
        &base_url,
        "test-token".to_string(),
        ["claude-sonnet-4-20250514".to_string()],
    )
    .unwrap();

    let req = CompletionRequest {
        model: ModelRef::new("anthropic/claude-sonnet-4-20250514"),
        system: Some("be helpful".to_string()),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "explain the file".to_string(),
            }],
        }],
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
            output_schema: None,
        }],
        temperature: None,
        max_output_tokens: Some(128),
        reasoning: None,
        headers: Default::default(),
    };

    let events: Vec<_> = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    let request = captured_request(request_rx).await;
    let headers = request.headers.to_ascii_lowercase();
    let body: Value = serde_json::from_str(&request.body).unwrap();
    let text_deltas: Vec<_> = events
        .into_iter()
        .map(Result::unwrap)
        .filter_map(|event| match event {
            Event::TextDelta { delta, .. } => Some(delta),
            _ => None,
        })
        .collect();

    assert!(headers.contains("x-api-key: test-token"));
    assert!(headers.contains("anthropic-version: 2023-06-01"));
    assert!(request.raw.starts_with("POST /messages HTTP/1.1\r\n"));
    assert_eq!(body["model"], "claude-sonnet-4-20250514");
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": "explain the file"}
        ])
    );
    assert_eq!(
        body["tools"],
        json!([
            {
                "name": "read",
                "description": "read a file",
                "input_schema": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }
        ])
    );
    assert_eq!(body["max_tokens"], 128);
    assert_eq!(body["system"], "be helpful");
    assert!(text_deltas.iter().any(|delta| delta == mock_text));
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

async fn captured_request(request_rx: oneshot::Receiver<CapturedRequest>) -> CapturedRequest {
    timeout(Duration::from_secs(3), request_rx)
        .await
        .unwrap()
        .unwrap()
}

async fn start_sse_server(response: String) -> (String, oneshot::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (request_tx, request_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
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

        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let body_len = content_length(&headers);
        while buf.len() < header_end + body_len {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n != 0, "socket closed before request body");
            buf.extend_from_slice(&chunk[..n]);
        }

        let body_end = header_end + body_len;
        let body = String::from_utf8_lossy(&buf[header_end..body_end]).to_string();
        let raw = String::from_utf8_lossy(&buf[..body_end]).to_string();
        request_tx
            .send(CapturedRequest { raw, headers, body })
            .unwrap();

        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{response}"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    (format!("http://{addr}"), request_rx)
}
