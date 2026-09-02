//! Per-request header construction and provider auth styles.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt as _;
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, SessionId,
    TokenUsage, ToolCallId, ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{
    CompletionRequest, HttpProvider, Provider as _, ProviderError, ProviderKind, ReasoningEffort,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;

#[derive(Debug)]
struct CapturedRequest {
    raw: String,
    headers: String,
    body: String,
}

#[tokio::test]
async fn http_provider_retries_retryable_status_before_returning_a_stream() {
    let (base_url, attempts) = start_retry_server().await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(events.iter().all(Result::is_ok));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn http_provider_retries_a_route_that_never_sends_response_headers() {
    let (base_url, connections) = start_stalled_header_server().await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap()
    .with_response_header_timeout(Duration::from_millis(200));
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    // Attempt one sits past the shortened header deadline; recovery must come
    // from the bounded timeout plus the standard retry path inside this
    // real-clock guard. Without the timeout the guard fires instead.
    let events = timeout(Duration::from_secs(15), async {
        provider
            .stream(req, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    })
    .await
    .expect("header stall should hit the deadline and retry within the guard");

    // A second connection happens only if attempt one exhausted the deadline.
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert!(events.iter().all(Result::is_ok));
}

#[tokio::test]
async fn http_provider_idle_stall_yields_one_error_without_second_request() {
    let (base_url, connections) = start_stalled_mid_stream_server().await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap()
    .with_idle_timeout(Duration::from_millis(250));
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = timeout(Duration::from_secs(15), async {
        provider
            .stream(req, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    })
    .await
    .expect("post-header idle stall should end the stream within the guard");

    assert_eq!(
        connections.load(Ordering::SeqCst),
        1,
        "post-stream failures sit behind the no-replay boundary"
    );
    assert_eq!(events.len(), 1);
    match &events[0] {
        Err(ProviderError::Http(message)) => {
            assert!(
                message.contains("no SSE frame"),
                "unexpected error: {message}"
            );
        }
        other => panic!("expected a single idle-stall error, got {other:?}"),
    }
}

#[tokio::test]
async fn http_provider_forwards_completion_request_headers() {
    let (base_url, request_rx) = start_sse_server("data: [DONE]\n\n".to_string()).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("test-token".to_string()),
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
        Some("test-token".to_string()),
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
async fn http_provider_posts_responses_body_with_every_reasoning_effort() {
    for effort in [
        ReasoningEffort::Off,
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::XHigh,
        ReasoningEffort::Max,
    ] {
        let (base_url, request_rx) = start_sse_server(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\ndata: [DONE]\n\n"
                .to_string(),
        )
        .await;
        let provider = HttpProvider::new(
            "openai",
            ProviderKind::OpenAiResponse,
            &base_url,
            Some("test-token".to_string()),
            ["gpt-5.6-sol".to_string()],
        )
        .unwrap();
        let req = CompletionRequest {
            model: ModelRef::new("openai/gpt-5.6-sol"),
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
            reasoning: Some(effort),
            headers: Default::default(),
        };

        let events: Vec<_> = provider
            .stream(req, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect()
            .await;
        let request = captured_request(request_rx).await;
        let body: Value = serde_json::from_str(&request.body).unwrap();

        assert!(events.iter().all(Result::is_ok));
        assert!(request.raw.starts_with("POST /responses HTTP/1.1\r\n"));
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["instructions"], "be terse");
        assert_eq!(
            body["input"],
            json!([{"role": "user", "content": "hello provider"}])
        );
        assert_eq!(
            body["tools"],
            json!([{
                "type": "function",
                "name": "read",
                "description": "read a file",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }])
        );
        assert_eq!(
            body["reasoning"],
            json!({"effort": effort.as_str(), "summary": "auto"})
        );
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(body.get("include").is_none());
    }
}

#[tokio::test]
async fn http_provider_codex_session_sends_account_id_header() {
    let response = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let (base_url, request_rx) = start_sse_server(response).await;
    let provider = HttpProvider::new(
        "codex",
        ProviderKind::OpenAiCodex,
        &base_url,
        Some("codex-jwt".to_string()),
        ["gpt-5.3-codex".to_string()],
    )
    .unwrap()
    .with_codex_session_auth(Some("acct-42".to_string()));
    let req = CompletionRequest {
        model: ModelRef::new("codex/gpt-5.3-codex"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let request = captured_request(request_rx).await;
    let headers = request.headers.to_ascii_lowercase();

    assert!(events.iter().all(Result::is_ok));
    assert!(headers.contains("authorization: bearer codex-jwt"));
    assert!(headers.contains("chatgpt-account-id: acct-42"));
    assert!(request.raw.starts_with("POST /responses HTTP/1.1\r\n"));
}

#[tokio::test]
async fn http_provider_grok_session_sends_oauth_proxy_headers() {
    let response = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        "data: [DONE]\n\n",
    )
    .to_string();
    let (base_url, request_rx) = start_sse_server(response).await;
    let provider = HttpProvider::new(
        "grok",
        ProviderKind::GrokBuild,
        &base_url,
        Some("oauth-jwt-token".to_string()),
        ["grok-4.5".to_string()],
    )
    .unwrap()
    .with_grok_session_auth("0.33.19", "grok-cli");
    let req = CompletionRequest {
        model: ModelRef::new("grok/grok-4.5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::High),
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let request = captured_request(request_rx).await;
    let headers = request.headers.to_ascii_lowercase();

    assert!(events.iter().all(Result::is_ok));
    assert!(headers.contains("authorization: bearer oauth-jwt-token"));
    assert!(headers.contains("x-xai-token-auth: xai-grok-cli"));
    assert!(headers.contains("x-grok-client-version: 0.33.19"));
    assert!(headers.contains("x-grok-client-identifier: grok-cli"));
    assert!(headers.contains("x-grok-model-override: grok-4.5"));
    assert!(request.raw.starts_with("POST /responses HTTP/1.1\r\n"));
}

#[tokio::test]
async fn http_provider_posts_grok_build_responses_body() {
    for effort in [
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ] {
        let response = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
            "data: [DONE]\n\n",
        )
        .to_string();
        let (base_url, request_rx) = start_sse_server(response).await;
        let provider = HttpProvider::new(
            "grok",
            ProviderKind::GrokBuild,
            &base_url,
            Some("test-token".to_string()),
            ["grok-4.5".to_string()],
        )
        .unwrap();
        let req = CompletionRequest {
            model: ModelRef::new("grok/grok-4.5"),
            system: None,
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            reasoning: Some(effort),
            headers: Default::default(),
        };

        let events = provider
            .stream(req, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let request = captured_request(request_rx).await;
        let body: Value = serde_json::from_str(&request.body).unwrap();

        assert!(events.iter().all(Result::is_ok));
        assert!(
            request
                .headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-token")
        );
        assert!(request.raw.starts_with("POST /responses HTTP/1.1\r\n"));
        assert_eq!(body["model"], "grok-4.5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            body["reasoning"],
            json!({"effort": effort.as_str(), "summary": "auto"})
        );
    }
}

#[tokio::test]
async fn http_provider_decodes_grok_reasoning_text_delta_and_typed_terminal() {
    let sse = [
        r#"data: {"type":"response.reasoning_text.delta","output_index":0,"delta":"Need a file."}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
    ]
    .join("\n\n")
        + "\n\n";
    let (base_url, _request_rx) = start_sse_server(sse).await;
    let provider = HttpProvider::new(
        "grok",
        ProviderKind::GrokBuild,
        &base_url,
        Some("test-token".to_string()),
        ["grok-4.5".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("grok/grok-4.5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::High),
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert_eq!(events.len(), 4);
    let reasoning_part = match &events[0] {
        Event::ReasoningStart { part, .. } => *part,
        event => panic!("expected reasoning start, got {event:?}"),
    };
    assert!(matches!(
        &events[1],
        Event::ReasoningDelta { part, delta, .. }
            if *part == reasoning_part && delta == "Need a file."
    ));
    assert!(matches!(
        &events[2],
        Event::ReasoningEnd { part, .. } if *part == reasoning_part
    ));
    assert!(matches!(
        &events[3],
        Event::MessageFinished {
            finish: FinishReason::Stop,
            ..
        }
    ));
}

/// Responses routes require typed terminal events; Chat Completions keeps its permissive close.
#[tokio::test]
async fn responses_routes_require_typed_terminal_while_chat_completions_remains_permissive() {
    for response in ["data: [DONE]\n\n", ""] {
        for kind in [ProviderKind::OpenAiResponse, ProviderKind::GrokBuild] {
            let events = response_events(kind, response).await;
            assert!(matches!(
                events.as_slice(),
                [Err(ProviderError::Decode(message))]
                    if message == "Responses stream ended without response.completed or response.incomplete"
            ));
        }

        let chat = response_events(ProviderKind::OpenAiCompatible, response).await;
        assert!(chat.iter().all(Result::is_ok));
        assert!(matches!(
            chat.last(),
            Some(Ok(Event::MessageFinished {
                finish: FinishReason::Stop,
                ..
            }))
        ));
    }
}

/// Every OpenAI-family streaming route reports malformed JSON as `ProviderError::Json`.
#[tokio::test]
async fn openai_family_routes_report_malformed_json_frames() {
    for kind in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::OpenAiResponse,
        ProviderKind::GrokBuild,
    ] {
        let events = response_events(kind, "data: {not-json}\n\n").await;
        assert!(matches!(events.as_slice(), [Err(ProviderError::Json(_))]));
    }
}

#[tokio::test]
async fn http_provider_decodes_responses_reasoning_text_tool_and_usage() {
    let sse = [
        r#"data: {"type":"response.reasoning_summary_text.delta","output_index":0,"summary_index":0,"delta":"Need a file."}"#,
        r#"data: {"type":"response.output_item.done","output_index":0,"item":{"id":"rs_123","type":"reasoning","summary":[{"type":"summary_text","text":"Need a file."}],"encrypted_content":"opaque"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":1,"item":{"id":"fc_123","type":"function_call","call_id":"call_provider","name":"read","arguments":"","status":"in_progress"}}"#,
        r#"data: {"type":"response.output_item.added","output_index":2,"item":{"id":"fc_456","type":"function_call","call_id":"call_provider_2","name":"search","arguments":"","status":"in_progress"}}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":1,"delta":"{\"path\":\"a.txt\"}"}"#,
        r#"data: {"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"query\":\"needle\"}"}"#,
        r#"data: {"type":"response.output_item.done","output_index":1,"item":{"id":"fc_123","type":"function_call","call_id":"call_provider","name":"read","arguments":"{\"path\":\"a.txt\"}","status":"completed"}}"#,
        r#"data: {"type":"response.output_item.done","output_index":2,"item":{"id":"fc_456","type":"function_call","call_id":"call_provider_2","name":"search","arguments":"{\"query\":\"needle\"}","status":"completed"}}"#,
        r#"data: {"type":"response.output_text.delta","output_index":3,"content_index":0,"delta":"Reading"}"#,
        r#"data: {"type":"response.output_text.done","output_index":3,"content_index":0,"text":"Reading"}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":11,"input_tokens_details":{"cached_tokens":3},"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2}}}}"#,
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
    ]
    .join("\n\n")
        + "\n\n";
    let (base_url, _request_rx) = start_sse_server(sse).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5.6-sol".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("openai/gpt-5.6-sol"),
        system: None,
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "read a.txt".to_string(),
            }],
        }],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::Low),
        headers: Default::default(),
    };

    let events: Vec<Event> = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();

    assert_eq!(events.len(), 13);
    let reasoning_part = match &events[0] {
        Event::ReasoningStart { part, .. } => *part,
        event => panic!("expected reasoning start, got {event:?}"),
    };
    let reasoning_start = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(
        reasoning_start["reason"], "low",
        "reasoning start must retain the requested effort"
    );
    assert!(matches!(
        &events[1],
        Event::ReasoningDelta { part, delta, .. }
            if *part == reasoning_part && delta == "Need a file."
    ));
    assert!(matches!(
        &events[2],
        Event::ReasoningEnd { part, provider_data: Some(data), .. }
            if *part == reasoning_part
                && data == &json!({
                    "id": "rs_123",
                    "type": "reasoning",
                    "summary": [{"type": "summary_text", "text": "Need a file."}],
                    "encrypted_content": "opaque"
                })
    ));
    let (tool_part, tool_call) = match &events[3] {
        Event::ToolInputStart {
            part, call, name, ..
        } if name.as_str() == "read" => (*part, *call),
        event => panic!("expected tool input start, got {event:?}"),
    };
    let (second_tool_part, second_tool_call) = match &events[4] {
        Event::ToolInputStart {
            part, call, name, ..
        } if name.as_str() == "search" => (*part, *call),
        event => panic!("expected second tool input start, got {event:?}"),
    };
    assert_ne!(tool_call, second_tool_call);
    assert!(matches!(
        &events[5],
        Event::ToolInputDelta { part, call, name, delta, .. }
            if *part == tool_part && *call == tool_call && name.as_str() == "read"
                && delta == "{\"path\":\"a.txt\"}"
    ));
    assert!(matches!(
        &events[6],
        Event::ToolInputDelta { part, call, name, delta, .. }
            if *part == second_tool_part && *call == second_tool_call
                && name.as_str() == "search" && delta == "{\"query\":\"needle\"}"
    ));
    assert!(matches!(
        &events[7],
        Event::ToolCallRequested { part, call, name, input, .. }
            if *part == tool_part && *call == tool_call && name.as_str() == "read"
                && input == &json!({"path": "a.txt"})
    ));
    assert!(matches!(
        &events[8],
        Event::ToolCallRequested { part, call, name, input, .. }
            if *part == second_tool_part && *call == second_tool_call
                && name.as_str() == "search" && input == &json!({"query": "needle"})
    ));
    let text_part = match &events[9] {
        Event::TextStart { part, .. } => *part,
        event => panic!("expected text start, got {event:?}"),
    };
    assert!(matches!(
        &events[10],
        Event::TextDelta { part, delta, .. } if *part == text_part && delta == "Reading"
    ));
    assert!(matches!(
        &events[11],
        Event::TextEnd { part, .. } if *part == text_part
    ));
    assert!(matches!(
        &events[12],
        Event::MessageFinished {
            finish: FinishReason::ToolCalls,
            tokens: Some(TokenUsage {
                input: 11,
                output: 7,
                reasoning: 2,
                cache_read: 3,
                cache_write: 0,
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn http_provider_reports_nested_responses_failure() {
    let (base_url, _request_rx) = start_sse_server(
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"quota exhausted\"}}}\n\n"
            .to_string(),
    )
    .await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5.6-sol".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("openai/gpt-5.6-sol"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Http(message))] if message == "quota exhausted"
    ));
}

#[tokio::test]
async fn http_provider_replays_completed_responses_reasoning_and_tool_round() {
    let (base_url, request_rx) = start_sse_server(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\ndata: [DONE]\n\n"
            .to_string(),
    )
    .await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5.6-sol".to_string()],
    )
    .unwrap();
    let call_id = ToolCallId::new();
    let provider_data = json!({
        "id": "rs_123",
        "type": "reasoning",
        "summary": [{"type": "summary_text", "text": "Need a file."}],
        "encrypted_content": "opaque"
    });
    let req = CompletionRequest {
        model: ModelRef::new("openai/gpt-5.6-sol"),
        system: None,
        messages: vec![
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "read a.txt".to_string(),
                }],
            },
            Message::Assistant {
                id: MessageId::new(),
                agent: AgentName::new("build"),
                model: ModelRef::new("openai/gpt-5.6-sol"),
                parts: vec![
                    Part::Reasoning {
                        id: PartId::new(),
                        text: "Need a file.".to_string(),
                        provider_data: Some(provider_data.clone()),
                    },
                    Part::Tool {
                        id: PartId::new(),
                        call_id,
                        name: ToolName::new("read"),
                        state: ToolPartState::Completed {
                            input: json!({"path": "a.txt"}),
                            output: json!("contents"),
                            time_ms: 3,
                        },
                    },
                ],
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::Medium),
        headers: Default::default(),
    };

    let events = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let request = captured_request(request_rx).await;
    let body: Value = serde_json::from_str(&request.body).unwrap();

    assert!(events.iter().all(Result::is_ok));
    assert_eq!(body["input"][1], provider_data);
    assert_eq!(
        body["input"][2],
        json!({
            "type": "function_call",
            "call_id": call_id.to_string(),
            "name": "read",
            "arguments": "{\"path\":\"a.txt\"}"
        })
    );
    assert_eq!(
        body["input"][3],
        json!({
            "type": "function_call_output",
            "call_id": call_id.to_string(),
            "output": "contents"
        })
    );
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
        Some("test-token".to_string()),
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

#[tokio::test]
async fn http_provider_forces_single_auth_refresh_on_401_and_retries_once() {
    let (base_url, connections, requests) = start_scripted_server(vec![
        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\nconnection: close\r\n\r\nstale token.\n".to_string(),
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n".to_string(),
    ])
    .await;
    let token = Arc::new(std::sync::Mutex::new("expired-token".to_string()));
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let resolver_token = Arc::clone(&token);
    let refresher_token = Arc::clone(&token);
    let refresher_calls = Arc::clone(&refresh_calls);
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("static-unused".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap()
    .with_bearer_resolver(Arc::new(move || Ok(resolver_token.lock().unwrap().clone())))
    .with_auth_refresher(Arc::new(move |_failed_token| {
        *refresher_token.lock().unwrap() = "fresh-token".to_string();
        refresher_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = timeout(Duration::from_secs(15), async {
        provider
            .stream(req, SessionId::new(), MessageId::new())
            .await
            .expect("recovered request should establish a stream")
            .collect::<Vec<_>>()
            .await
    })
    .await
    .expect("forced-refresh recovery should complete within the guard");

    assert!(events.iter().all(Result::is_ok));
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    let captured = requests.lock().unwrap();
    assert_eq!(captured.len(), 2);
    assert!(
        captured[0]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer expired-token")
    );
    assert!(
        captured[1]
            .headers
            .to_ascii_lowercase()
            .contains("authorization: bearer fresh-token")
    );
}

#[tokio::test]
async fn concurrent_401s_refresh_the_credential_used_by_each_request_once() {
    let first_rotation = Arc::new(Notify::new());
    let (base_url, connections) =
        start_concurrent_unauthorized_server(Arc::clone(&first_rotation)).await;
    let token_generation = Arc::new(AtomicUsize::new(0));
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    let resolver_generation = Arc::clone(&token_generation);
    let refresher_generation = Arc::clone(&token_generation);
    let refresher_calls = Arc::clone(&refresh_calls);
    let provider = Arc::new(
        HttpProvider::new(
            "openai",
            ProviderKind::OpenAiCompatible,
            &base_url,
            Some("static-unused".to_string()),
            ["gpt-5".to_string()],
        )
        .unwrap()
        .with_bearer_resolver(Arc::new(move || {
            let generation = resolver_generation.load(Ordering::SeqCst);
            Ok(if generation == 0 {
                "expired-token".to_string()
            } else {
                format!("fresh-token-{generation}")
            })
        }))
        .with_auth_refresher(Arc::new(move |failed_token| {
            let expected_generation = if failed_token == "expired-token" {
                Some(0)
            } else {
                failed_token
                    .strip_prefix("fresh-token-")
                    .and_then(|value| value.parse::<usize>().ok())
            };
            if let Some(expected) = expected_generation
                && refresher_generation
                    .compare_exchange(expected, expected + 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let call = refresher_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call == 1 {
                    first_rotation.notify_one();
                }
            }
            Ok(())
        })),
    );
    let request = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let first = provider.stream(request.clone(), SessionId::new(), MessageId::new());
    let second = provider.stream(request, SessionId::new(), MessageId::new());
    let (first, second) = timeout(Duration::from_secs(15), async {
        tokio::join!(first, second)
    })
    .await
    .expect("both concurrent requests should recover within the guard");
    let first_events = first.unwrap().collect::<Vec<_>>().await;
    let second_events = second.unwrap().collect::<Vec<_>>().await;

    assert!(first_events.iter().all(Result::is_ok));
    assert!(second_events.iter().all(Result::is_ok));
    assert_eq!(connections.load(Ordering::SeqCst), 4);
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        1,
        "both requests sent the same expired token, so only one network refresh is valid"
    );
    assert_eq!(token_generation.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_provider_surfaces_401_after_a_single_forced_refresh_attempt() {
    let unauthorized = "HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\nconnection: close\r\n\r\nstale token.\n"
        .to_string();
    let (base_url, connections, _requests) =
        start_scripted_server(vec![unauthorized.clone(), unauthorized]).await;
    let refresh_calls = Arc::new(AtomicUsize::new(0));
    // The hook really rotates the live credential: connection one fails on
    // "expired-token", recovery installs "fresh-token", but the server still
    // answers 401. Only then must the original status surface, unrefreshed.
    let token = Arc::new(std::sync::Mutex::new("expired-token".to_string()));
    let resolver_token = Arc::clone(&token);
    let refresher_token = Arc::clone(&token);
    let refresher_calls = Arc::clone(&refresh_calls);
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("static-unused".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap()
    .with_bearer_resolver(Arc::new(move || Ok(resolver_token.lock().unwrap().clone())))
    .with_auth_refresher(Arc::new(move |_failed_token| {
        *refresher_token.lock().unwrap() = "fresh-token".to_string();
        refresher_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }));
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let result = timeout(
        Duration::from_secs(15),
        provider.stream(req, SessionId::new(), MessageId::new()),
    )
    .await
    .expect("401 recovery must surface an error without hanging");

    let Err(error) = result else {
        panic!("401 without recovery budget must fail before any stream");
    };
    assert!(
        matches!(&error, ProviderError::HttpStatus { status: 401, .. }),
        "expected the original 401 surfaced unchanged, got {error:?}"
    );
    assert_eq!(connections.load(Ordering::SeqCst), 2);
    assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn http_provider_without_refresher_makes_one_connection_on_401() {
    let (base_url, connections, _requests) = start_scripted_server(vec![
        "HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\nconnection: close\r\n\r\nstale token.\n"
            .to_string(),
    ])
    .await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiCompatible,
        &base_url,
        Some("test-token".to_string()),
        ["gpt-5".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("gpt-5"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let result = provider
        .stream(req, SessionId::new(), MessageId::new())
        .await;

    assert!(matches!(
        result,
        Err(ProviderError::HttpStatus { status: 401, .. })
    ));
    assert_eq!(connections.load(Ordering::SeqCst), 1);
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

async fn response_events(kind: ProviderKind, response: &str) -> Vec<Result<Event, ProviderError>> {
    let (base_url, _request_rx) = start_sse_server(response.to_string()).await;
    let provider = HttpProvider::new(
        "test",
        kind,
        &base_url,
        Some("test-token".to_string()),
        ["model".to_string()],
    )
    .unwrap();
    let req = CompletionRequest {
        model: ModelRef::new("test/model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    provider
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await
}

async fn read_request_head(socket: &mut TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let n = socket.read(&mut chunk).await.unwrap();
        assert!(n != 0, "socket closed before request completed");
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            // The JSON body may still be arriving; neither stall nor success
            // branches depend on it (success responds over the buffered bytes,
            // matching the existing mock servers here).
            break;
        }
    }
}

/// Connection 1 receives the request and then falls silent before response
/// headers; every later connection receives a complete successful SSE reply.
async fn start_stalled_header_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let seq = server_attempts.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut socket = socket;
                read_request_head(&mut socket).await;
                if seq == 0 {
                    // Stall exactly where the response-header deadline bites.
                    futures::future::pending::<()>().await;
                }
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n",
                    )
                    .await
                    .unwrap();
            });
        }
    });
    (format!("http://{addr}"), attempts)
}

/// Responds with successful stream headers immediately, then sends no SSE
/// frame and never closes the socket.
async fn start_stalled_mid_stream_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = connections.clone();
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut socket = socket;
                read_request_head(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                    .await
                    .unwrap();
                // Hold the connection open in silence so the idle deadline is
                // the only way the client can make progress.
                futures::future::pending::<()>().await;
            });
        }
    });
    (format!("http://{addr}"), connections)
}

async fn start_retry_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    tokio::spawn(async move {
        for response in [
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nretry-after: 0\r\nconnection: close\r\n\r\nbusy".to_string(),
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n".to_string(),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            server_attempts.fetch_add(1, Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n != 0, "socket closed before request headers");
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{addr}"), attempts)
}

/// Sequential scripted-response server: connection N receives `responses[N]`,
/// and every request head/body pair is recorded for assertions.
async fn start_scripted_server(
    responses: Vec<String>,
) -> (
    String,
    Arc<AtomicUsize>,
    Arc<std::sync::Mutex<Vec<CapturedRequest>>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server_connections = Arc::clone(&connections);
    let server_requests = Arc::clone(&requests);
    tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
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
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
            let body_len = content_length(&headers);
            while buf.len() < header_end + body_len {
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n != 0, "socket closed before request body");
                buf.extend_from_slice(&chunk[..n]);
            }
            let body_end = header_end + body_len;
            server_requests.lock().unwrap().push(CapturedRequest {
                raw: String::from_utf8_lossy(&buf[..body_end]).to_string(),
                headers,
                body: String::from_utf8_lossy(&buf[header_end..body_end]).to_string(),
            });
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{addr}"), connections, requests)
}

/// Hold two requests that used the original bearer, release the second 401 only
/// after the first request rotates storage, then serve both retries.
async fn start_concurrent_unauthorized_server(
    first_rotation: Arc<Notify>,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let server_connections = Arc::clone(&connections);
    tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        server_connections.fetch_add(1, Ordering::SeqCst);
        let (mut second, _) = listener.accept().await.unwrap();
        server_connections.fetch_add(1, Ordering::SeqCst);
        read_request_head(&mut first).await;
        read_request_head(&mut second).await;
        let unauthorized =
            b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        first.write_all(unauthorized).await.unwrap();
        first_rotation.notified().await;
        second.write_all(unauthorized).await.unwrap();

        for _ in 0..2 {
            let (mut retry, _) = listener.accept().await.unwrap();
            server_connections.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut retry).await;
            retry
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n",
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}"), connections)
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
