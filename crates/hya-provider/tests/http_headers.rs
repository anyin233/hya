#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{collections::BTreeMap, time::Duration};

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
        let (base_url, request_rx) = start_sse_server("data: [DONE]\n\n".to_string()).await;
        let provider = HttpProvider::new(
            "openai",
            ProviderKind::OpenAiResponse,
            &base_url,
            "test-token".to_string(),
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
        "oauth-jwt-token".to_string(),
        ["grok-4.5".to_string()],
    )
    .unwrap()
    .with_grok_session_auth("0.33.19", "hya");
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
    assert!(headers.contains("x-grok-client-identifier: hya"));
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
            "test-token".to_string(),
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
        "test-token".to_string(),
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

#[tokio::test]
async fn grok_requires_typed_terminal_while_openai_responses_remains_permissive() {
    for response in ["data: [DONE]\n\n", ""] {
        let grok = response_events(ProviderKind::GrokBuild, response).await;
        let openai = response_events(ProviderKind::OpenAiResponse, response).await;

        assert!(matches!(
            grok.as_slice(),
            [Err(ProviderError::Decode(message))]
                if message == "Responses stream ended without response.completed or response.incomplete"
        ));
        assert!(openai.iter().all(Result::is_ok));
        assert!(matches!(
            openai.last(),
            Some(Ok(Event::MessageFinished {
                finish: FinishReason::Stop,
                ..
            }))
        ));
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
        "test-token".to_string(),
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
        reasoning: Some(ReasoningEffort::Medium),
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
        "test-token".to_string(),
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
    let (base_url, request_rx) = start_sse_server("data: [DONE]\n\n".to_string()).await;
    let provider = HttpProvider::new(
        "openai",
        ProviderKind::OpenAiResponse,
        &base_url,
        "test-token".to_string(),
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

async fn response_events(kind: ProviderKind, response: &str) -> Vec<Result<Event, ProviderError>> {
    let (base_url, _request_rx) = start_sse_server(response.to_string()).await;
    let provider = HttpProvider::new(
        "test",
        kind,
        &base_url,
        "test-token".to_string(),
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
