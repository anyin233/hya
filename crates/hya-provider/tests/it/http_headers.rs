//! HTTP provider transport, retries, timeouts, auth refresh, in-stream error frames,
//! per-provider headers/bodies, and Responses compaction contracts.

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
    AgentName, Envelope, Event, EventSeq, FinishReason, Message, MessageId, ModelRef, Part, PartId,
    PartProjection, Projection, Role, SessionId, SessionKind, TokenUsage, ToolCallId, ToolName,
    ToolPartState, ToolSchema,
};
use hya_provider::test_support::{captured_request, start_scripted_server, start_sse_server};
use hya_provider::{
    AnthropicMessagesProtocol, CatalogDiscoveryRequest, CompletionRequest, GoogleProtocol,
    GrokBuildProtocol, HttpProvider, OpenAiChatProtocol, OpenAiCodexProtocol,
    OpenAiResponsesProtocol, Protocol, Provider as _, ProviderError, ReasoningEffort, RetryConfig,
    RouteCredential, discover_models,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;

/// Protocol of a builtin kind label.
fn kind(label: &str) -> Arc<dyn Protocol> {
    match label {
        "openai" => Arc::new(OpenAiChatProtocol),
        "openai-response" => Arc::new(OpenAiResponsesProtocol),
        "openai-codex" => Arc::new(OpenAiCodexProtocol),
        "grok-build" => Arc::new(GrokBuildProtocol),
        "anthropic" => Arc::new(AnthropicMessagesProtocol),
        "google" => Arc::new(GoogleProtocol),
        other => panic!("no builtin kind {other}"),
    }
}

fn credential(token: &str, account_id: Option<&str>) -> Option<RouteCredential> {
    Some(RouteCredential {
        token: token.to_string(),
        account_id: account_id.map(str::to_string),
    })
}

/// An [`HttpProvider`] for kind `label` with an optional bearer/API key.
fn route(
    id: &str,
    label: &str,
    base_url: &str,
    key: Option<String>,
    models: impl IntoIterator<Item = String>,
) -> Result<HttpProvider, ProviderError> {
    HttpProvider::new(
        id,
        kind(label),
        base_url,
        key.and_then(|key| credential(&key, None)),
        models,
    )
}

fn req(model: &str) -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new(model),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

fn fast_retry(max_attempts: usize) -> RetryConfig {
    RetryConfig {
        max_attempts,
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(5),
    }
}

fn sse_ok(frames: &[&str]) -> String {
    let body: String = frames.iter().map(|f| format!("{f}\n\n")).collect();
    format!("HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}")
}

#[tokio::test]
async fn catalog_auth_headers_match_provider_contracts() {
    let version = format!("x-grok-client-version: {}", hya_version::BACKEND_VERSION);
    let cases: [(&str, Option<RouteCredential>, Vec<&str>); 8] = [
        (
            "openai",
            credential("ChatToken", None),
            vec!["authorization: Bearer ChatToken"],
        ),
        (
            "openai-response",
            credential("ResponseToken", None),
            vec!["authorization: Bearer ResponseToken"],
        ),
        (
            "openai-codex",
            credential("CodexToken", Some("Account")),
            vec![
                "authorization: Bearer CodexToken",
                "chatgpt-account-id: Account",
                "openai-beta: responses=experimental",
                "user-agent: codex_cli_rs",
            ],
        ),
        (
            "openai-codex",
            credential("", Some("Account")),
            vec![
                "openai-beta: responses=experimental",
                "user-agent: codex_cli_rs",
            ],
        ),
        (
            "grok-build",
            credential("GrokToken", None),
            vec![
                "authorization: Bearer GrokToken",
                "x-xai-token-auth: xai-grok-cli",
                &version,
                "x-grok-client-identifier: grok-cli",
            ],
        ),
        (
            "grok-build",
            None,
            vec![&version, "x-grok-client-identifier: grok-cli"],
        ),
        (
            "anthropic",
            credential("AnthropicKey", None),
            vec!["x-api-key: AnthropicKey", "anthropic-version: 2023-06-01"],
        ),
        (
            "google",
            credential("GoogleKey", None),
            vec!["x-goog-api-key: GoogleKey"],
        ),
    ];
    let response = "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
    let (base_url, _, requests) = start_scripted_server(vec![response.into(); cases.len()]).await;
    for (label, auth, expected) in cases {
        discover_models(CatalogDiscoveryRequest::new(
            "test",
            self::kind(label),
            &base_url,
            auth,
        ))
        .await;
        let requests = requests.lock().unwrap();
        let actual: Vec<_> = requests
            .last()
            .unwrap()
            .headers
            .lines()
            .filter(|line| {
                !line.starts_with("GET ")
                    && !line.starts_with("host: ")
                    && *line != "accept: application/json"
                    && !line.is_empty()
            })
            .collect();
        assert_eq!(actual, expected, "{label}");
        assert!(requests.last().unwrap().body.is_empty());
    }
}

#[tokio::test]
async fn http_retry_status_header_timeout_and_budget_contracts() {
    assert_eq!(RetryConfig::default().backoff_base, Duration::from_secs(1));
    let busy = "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nretry-after: 0\r\nconnection: close\r\n\r\nbusy";
    let ok = sse_ok(&["data: [DONE]"]);

    // 1. Retries transient 503 before stream establishment.
    let (url1, conns1, _) = start_scripted_server(vec![busy.into(), ok.clone()]).await;
    let p1 = route(
        "openai",
        "openai",
        &url1,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_retry(fast_retry(3));
    let evs: Vec<_> = p1
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert!(evs.iter().all(Result::is_ok));
    assert_eq!(conns1.load(Ordering::SeqCst), 2);

    // 2. Stalled response headers hit response_header_timeout and retry onto attempt 2.
    let (url2, conns2) = start_stalled_header_server().await;
    let p2 = route(
        "openai",
        "openai",
        &url2,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_response_header_timeout(Duration::from_millis(50))
    .with_retry(fast_retry(3));
    let evs = timeout(Duration::from_secs(5), async {
        p2.stream(req("gpt-5"), SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await
    })
    .await
    .unwrap();
    assert_eq!(conns2.load(Ordering::SeqCst), 2);
    assert!(evs.iter().all(Result::is_ok));

    // 3. max_attempts=1 fails fast without a second request.
    let (url3, conns3, _) = start_scripted_server(vec![busy.into(), ok]).await;
    let p3 = route(
        "openai",
        "openai",
        &url3,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_retry(fast_retry(1));
    let err3 = p3
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(
        err3,
        ProviderError::HttpStatus { status: 503, .. }
    ));
    assert_eq!(conns3.load(Ordering::SeqCst), 1);

    // 4. Exhausted retryable 503 reports attempt count and body.
    let busy_no_ra =
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 4\r\nconnection: close\r\n\r\nbusy";
    let (url4, conns4, _) = start_scripted_server(vec![busy_no_ra.into(); 3]).await;
    let p4 = route(
        "openai",
        "openai",
        &url4,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_retry(fast_retry(3));
    let err4 = p4
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(
        &err4,
        ProviderError::HttpStatus { status: 503, .. }
    ));
    assert_eq!(conns4.load(Ordering::SeqCst), 3);
    assert!(err4.to_string().contains("after 3 attempts") && err4.to_string().contains("busy"));
    assert!(err4.is_retryable_before_stream());
}

#[tokio::test]
async fn http_stream_interruption_idle_stall_body_truncation_and_drop_abort() {
    // 1. Post-header idle stall surfaces once without replay.
    let (idle_url, idle_conns) = start_stalled_mid_stream_server().await;
    let p_idle = route(
        "openai",
        "openai",
        &idle_url,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_idle_timeout(Duration::from_millis(50));
    let idle_evs: Vec<_> = p_idle
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(idle_conns.load(Ordering::SeqCst), 1);
    let [Err(err @ ProviderError::StreamInterrupted(_))] = idle_evs.as_slice() else {
        panic!("expected single idle-stall error, got {idle_evs:?}");
    };
    assert_eq!(
        err.to_string(),
        "http: stalled stream: no SSE frame within 50ms"
    );
    assert!(!err.is_retryable_before_stream() && err.retry_after().is_none());

    // 2. Zero-event truncated body surfaces once without replay.
    let truncated = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 500\r\nconnection: close\r\n\r\n";
    let (z_url, z_conns, _) =
        start_scripted_server(vec![truncated.into(), sse_ok(&["data: [DONE]"])]).await;
    let p_z = route(
        "openai",
        "openai",
        &z_url,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap();
    let z_evs: Vec<_> = p_z
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(z_conns.load(Ordering::SeqCst), 1);
    assert!(matches!(
        z_evs.as_slice(),
        [Err(err @ ProviderError::StreamInterrupted(_))] if err.to_string() == "http: Transport error: error decoding response body"
    ));

    // 3. Post-event truncated body delivers earlier frames then terminal error without replay.
    let chunk = r#"data: {"id":"1","object":"chat.completion.chunk","created":0,"model":"gpt-5","choices":[{"index":0,"delta":{"role":"assistant","content":"Hi"},"finish_reason":null}]}"#;
    let partial = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 900\r\nconnection: close\r\n\r\n{chunk}\n\n"
    );
    let (p_url, p_conns, _) = start_scripted_server(vec![partial]).await;
    let p_part = route(
        "openai",
        "openai",
        &p_url,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap();
    let p_evs: Vec<_> = p_part
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(p_conns.load(Ordering::SeqCst), 1);
    assert_eq!(p_evs.len(), 3);
    assert!(p_evs[0].is_ok() && p_evs[1].is_ok());
    assert!(matches!(p_evs[2], Err(ProviderError::StreamInterrupted(_))));

    // 4. Dropping EventStream aborts the HTTP body immediately.
    let (ka_url, disconnected) = start_keepalive_sse_server().await;
    let p_ka = route(
        "openai",
        "openai",
        &ka_url,
        Some("t".into()),
        ["gpt-5".into()],
    )
    .unwrap()
    .with_idle_timeout(Duration::from_secs(30));
    let stream = p_ka
        .stream(req("gpt-5"), SessionId::new(), MessageId::new())
        .await
        .unwrap();
    drop(stream);
    timeout(Duration::from_millis(500), disconnected)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn in_stream_error_frames_surface_once_without_replay() {
    const CONCURRENCY: &str = "Concurrency limit exceeded for account, please retry later";
    let anth_err = |kind: &str, msg: &str| {
        format!(
            "event: error\ndata: {{\"type\":\"error\",\"error\":{{\"type\":\"{kind}\",\"message\":\"{msg}\"}}}}"
        )
    };
    let anth_ok = sse_ok(&[
        r#"data: {"type":"message_start","message":{}}"#,
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        r#"data: {"type":"message_stop"}"#,
    ]);

    // Zero-event in-stream errors never replay (rate limit, untyped gateway, overloaded, invalid_request, Responses failed).
    for (label, kind, model, frame, expected_status) in [
        ("rate_limit", "anthropic", "claude-sonnet-4", anth_err("rate_limit_error", CONCURRENCY), Some(429)),
        ("untyped_gateway", "anthropic", "claude-sonnet-4", format!("data: {{\"error\":{{\"message\":\"{CONCURRENCY}\"}}}}"), None),
        ("overloaded", "anthropic", "claude-sonnet-4", anth_err("overloaded_error", "Overloaded"), None),
        ("invalid_request", "anthropic", "claude-sonnet-4", anth_err("invalid_request_error", "messages: field required"), Some(400)),
        ("responses_server_error", "openai-response", "gpt-5", r#"data: {"type":"response.failed","response":{"error":{"code":"server_error","message":"The server had an error"}}}"#.to_string(), None),
    ] {
        let (url, conns, _) = start_scripted_server(vec![sse_ok(&[&frame]), anth_ok.clone()]).await;
        let provider = route("p", kind, &url, Some("t".into()), [model.into()])
            .unwrap()
            .with_retry(fast_retry(5));
        let events: Vec<_> = provider
            .stream(req(model), SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert_eq!(conns.load(Ordering::SeqCst), 1, "{label}");
        assert!(matches!(events.as_slice(), [Err(_)]), "{label}: {events:?}");
        if let Some(code) = expected_status {
            let Err(err @ ProviderError::HttpStatus { status, .. }) = &events[0] else {
                panic!("{label}: expected HttpStatus({code}), got {:?}", events[0]);
            };
            assert_eq!(*status, code, "{label}");
            if code == 400 {
                assert!(!err.is_retryable_before_stream());
            }
        }
    }

    // Post-delta in-stream rate limit keeps partial text and surfaces one 429 error without replay.
    let partial_frames = [
        r#"data: {"type":"message_start","message":{}}"#.to_string(),
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#.to_string(),
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial"}}"#.to_string(),
        anth_err("rate_limit_error", CONCURRENCY),
    ];
    let refs: Vec<&str> = partial_frames.iter().map(String::as_str).collect();
    let (url, conns, _) = start_scripted_server(vec![sse_ok(&refs), anth_ok]).await;
    let provider = route(
        "p",
        "anthropic",
        &url,
        Some("t".into()),
        ["claude-sonnet-4".into()],
    )
    .unwrap()
    .with_retry(fast_retry(5));
    let events: Vec<_> = provider
        .stream(req("claude-sonnet-4"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(conns.load(Ordering::SeqCst), 1);
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(Event::TextDelta { delta, .. }) => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "partial");
    assert_eq!(events.iter().filter(|e| e.is_err()).count(), 1);
    assert!(matches!(
        events.last(),
        Some(Err(ProviderError::HttpStatus { status: 429, message, .. })) if message.contains(CONCURRENCY)
    ));
}

#[tokio::test]
async fn http_401_auth_refresh_single_concurrent_exhausted_and_unconfigured() {
    let unauth = "HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\nconnection: close\r\n\r\nstale token.\n";
    let ok = sse_ok(&["data: [DONE]"]);

    // 1. Single 401 triggers one refresh and retries with the rotated bearer token.
    {
        let (url, conns, reqs) = start_scripted_server(vec![unauth.into(), ok.clone()]).await;
        let token = Arc::new(std::sync::Mutex::new("expired-token".to_string()));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let (rt, ft, rc) = (
            Arc::clone(&token),
            Arc::clone(&token),
            Arc::clone(&refreshes),
        );
        let provider = route(
            "openai",
            "openai",
            &url,
            Some("unused".into()),
            ["gpt-5".into()],
        )
        .unwrap()
        .with_bearer_resolver(Arc::new(move || Ok(rt.lock().unwrap().clone())))
        .with_auth_refresher(Arc::new(move |_| {
            *ft.lock().unwrap() = "fresh-token".to_string();
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        let evs: Vec<_> = provider
            .stream(req("gpt-5"), SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect()
            .await;
        assert!(evs.iter().all(Result::is_ok));
        assert_eq!(conns.load(Ordering::SeqCst), 2);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
        let cap = reqs.lock().unwrap();
        assert!(
            cap[0]
                .headers
                .to_ascii_lowercase()
                .contains("authorization: bearer expired-token")
        );
        assert!(
            cap[1]
                .headers
                .to_ascii_lowercase()
                .contains("authorization: bearer fresh-token")
        );
    }

    // 2. Concurrent 401s sharing one expired token perform only one network refresh.
    {
        let first_rotation = Arc::new(Notify::new());
        let (url, conns) = start_concurrent_unauthorized_server(Arc::clone(&first_rotation)).await;
        let generation = Arc::new(AtomicUsize::new(0));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let (rg, fg, rc) = (
            Arc::clone(&generation),
            Arc::clone(&generation),
            Arc::clone(&refreshes),
        );
        let provider = Arc::new(
            route(
                "openai",
                "openai",
                &url,
                Some("unused".into()),
                ["gpt-5".into()],
            )
            .unwrap()
            .with_bearer_resolver(Arc::new(move || {
                let g = rg.load(Ordering::SeqCst);
                Ok(if g == 0 {
                    "expired-token".into()
                } else {
                    format!("fresh-token-{g}")
                })
            }))
            .with_auth_refresher(Arc::new(move |failed| {
                let expected = if failed == "expired-token" {
                    Some(0)
                } else {
                    failed
                        .strip_prefix("fresh-token-")
                        .and_then(|v| v.parse().ok())
                };
                if let Some(exp) = expected
                    && fg
                        .compare_exchange(exp, exp + 1, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    && rc.fetch_add(1, Ordering::SeqCst) == 0
                {
                    first_rotation.notify_one();
                }
                Ok(())
            })),
        );
        let (a, b) = tokio::join!(
            provider.stream(req("gpt-5"), SessionId::new(), MessageId::new()),
            provider.stream(req("gpt-5"), SessionId::new(), MessageId::new()),
        );
        assert!(
            a.unwrap()
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(Result::is_ok)
        );
        assert!(
            b.unwrap()
                .collect::<Vec<_>>()
                .await
                .iter()
                .all(Result::is_ok)
        );
        assert_eq!(conns.load(Ordering::SeqCst), 4);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }

    // 3. Persistent 401 surfaces after one forced refresh; without refresher makes one connection.
    {
        let (url, conns, _) = start_scripted_server(vec![unauth.into(), unauth.into()]).await;
        let token = Arc::new(std::sync::Mutex::new("expired".to_string()));
        let refreshes = Arc::new(AtomicUsize::new(0));
        let (rt, ft, rc) = (
            Arc::clone(&token),
            Arc::clone(&token),
            Arc::clone(&refreshes),
        );
        let provider = route(
            "openai",
            "openai",
            &url,
            Some("unused".into()),
            ["gpt-5".into()],
        )
        .unwrap()
        .with_bearer_resolver(Arc::new(move || Ok(rt.lock().unwrap().clone())))
        .with_auth_refresher(Arc::new(move |_| {
            *ft.lock().unwrap() = "fresh".to_string();
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        assert!(matches!(
            provider
                .stream(req("gpt-5"), SessionId::new(), MessageId::new())
                .await,
            Err(ProviderError::HttpStatus { status: 401, .. })
        ));
        assert_eq!(conns.load(Ordering::SeqCst), 2);
        assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    }
    {
        let (url, conns, _) = start_scripted_server(vec![unauth.into()]).await;
        let provider = route("openai", "openai", &url, Some("t".into()), ["gpt-5".into()]).unwrap();
        assert!(matches!(
            provider
                .stream(req("gpt-5"), SessionId::new(), MessageId::new())
                .await,
            Err(ProviderError::HttpStatus { status: 401, .. })
        ));
        assert_eq!(conns.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn http_provider_routes_request_headers_and_protocol_bodies() {
    // 1. OpenAI Compatible forwards custom headers, system, messages, tools, stream_options.
    let (url, rx) = start_sse_server(
        "data: {\"choices\":[{\"delta\":{\"content\":\"mock openai delta\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n".into(),
    )
    .await;
    let provider = route(
        "openai",
        "openai",
        &url,
        Some("test-token".into()),
        ["gpt-5".into()],
    )
    .unwrap();
    let mut headers = BTreeMap::new();
    headers.insert("x-hya-session".into(), "session-headers".into());
    let chat_req = CompletionRequest {
        model: ModelRef::new("openai/gpt-5"),
        system: Some("be terse".into()),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "hello provider".into(),
            }],
        }],
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            output_schema: None,
        }],
        headers,
        ..req("openai/gpt-5")
    };
    let evs: Vec<_> = provider
        .stream(chat_req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    let captured = captured_request(rx).await;
    let lc = captured.headers.to_ascii_lowercase();
    assert!(
        lc.contains("authorization: bearer test-token")
            && lc.contains("x-hya-session: session-headers")
    );
    assert!(
        captured
            .raw
            .starts_with("POST /chat/completions HTTP/1.1\r\n")
    );
    let body: Value = serde_json::from_str(&captured.body).unwrap();
    assert_eq!(body["model"], "gpt-5");
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    assert!(
        evs.iter().any(
            |e| matches!(e, Ok(Event::TextDelta { delta, .. }) if delta == "mock openai delta")
        )
    );

    // 2. OpenAI Responses across None + all 7 ReasoningEfforts.
    let completed = sse_ok(&[
        r#"data: {"type":"response.completed","response":{"status":"completed"}}"#,
        "data: [DONE]",
    ]);
    let efforts = [
        None,
        Some(ReasoningEffort::Off),
        Some(ReasoningEffort::Minimal),
        Some(ReasoningEffort::Low),
        Some(ReasoningEffort::Medium),
        Some(ReasoningEffort::High),
        Some(ReasoningEffort::XHigh),
        Some(ReasoningEffort::Max),
    ];
    let (r_url, _, r_reqs) = start_scripted_server(vec![completed.clone(); efforts.len()]).await;
    let r_prov = route(
        "openai",
        "openai-response",
        &r_url,
        Some("t".into()),
        ["gpt-5.6-sol".into()],
    )
    .unwrap();
    for effort in efforts {
        let mut r = req("openai/gpt-5.6-sol");
        r.system = Some("be terse".into());
        r.reasoning = effort;
        let _ = r_prov
            .stream(r, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
    }
    for (effort, cap) in efforts.into_iter().zip(r_reqs.lock().unwrap().iter()) {
        assert!(cap.raw.starts_with("POST /responses HTTP/1.1\r\n"));
        let b: Value = serde_json::from_str(&cap.body).unwrap();
        assert_eq!(
            (
                b["model"].as_str(),
                b["stream"].as_bool(),
                b["store"].as_bool()
            ),
            (Some("gpt-5.6-sol"), Some(true), Some(false))
        );
        match effort {
            None => assert!(b.get("reasoning").is_none() && b.get("include").is_none()),
            Some(ReasoningEffort::Off) => {
                assert_eq!(b["reasoning"], json!({"effort": "none", "summary": "auto"}));
                assert!(b.get("include").is_none());
            }
            Some(e) => {
                assert_eq!(
                    b["reasoning"],
                    json!({"effort": e.as_str(), "summary": "auto"})
                );
                assert_eq!(b["include"], json!(["reasoning.encrypted_content"]));
            }
        }
    }

    // 3. Codex & Grok Build encrypted reasoning matrix + session auth headers.
    let enc = Some(json!(["reasoning.encrypted_content"]));
    let matrix = [
        ("openai-codex", None, None),
        ("openai-codex", Some(ReasoningEffort::Off), None),
        ("openai-codex", Some(ReasoningEffort::High), enc.clone()),
        ("grok-build", None, enc.clone()),
        ("grok-build", Some(ReasoningEffort::Off), enc.clone()),
        ("grok-build", Some(ReasoningEffort::High), enc),
    ];
    let (m_url, _, m_reqs) = start_scripted_server(vec![completed; matrix.len()]).await;
    for (kind, effort, expected_include) in matrix {
        let p = HttpProvider::new(
            "route",
            self::kind(kind),
            &m_url,
            credential("tok", Some("acct-42")),
            ["model".into()],
        )
        .unwrap();
        let mut r = req("route/model");
        r.reasoning = effort;
        let _ = p
            .stream(r, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        let cap = m_reqs.lock().unwrap();
        let last = cap.last().unwrap();
        let lc = last.headers.to_ascii_lowercase();
        let b: Value = serde_json::from_str(&last.body).unwrap();
        assert_eq!(
            b.get("include"),
            expected_include.as_ref(),
            "{kind:?} {effort:?}"
        );
        if kind == "openai-codex" {
            assert!(lc.contains("chatgpt-account-id: acct-42"));
        } else {
            assert!(lc.contains("x-xai-token-auth: xai-grok-cli"));
            assert!(lc.contains(&format!(
                "x-grok-client-version: {}",
                hya_version::BACKEND_VERSION
            )));
            assert!(lc.contains("x-grok-client-identifier: grok-cli"));
            assert!(lc.contains("x-grok-model-override: model"));
        }
    }
}

#[tokio::test]
async fn http_provider_responses_stream_decoding_reasoning_tools_errors_and_replay() {
    // 1. Grok reasoning_text.delta + typed terminal.
    let grok_sse = "data: {\"type\":\"response.reasoning_text.delta\",\"output_index\":0,\"delta\":\"Need a file.\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n";
    let grok_evs = response_events("grok-build", grok_sse)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(grok_evs.len(), 4);
    assert!(matches!(&grok_evs[1], Event::ReasoningDelta { delta, .. } if delta == "Need a file."));
    assert!(matches!(
        &grok_evs[3],
        Event::MessageFinished {
            finish: FinishReason::Stop,
            ..
        }
    ));

    // 2. Responses/Grok require typed terminal; Chat stays permissive; all OpenAI-family report malformed JSON.
    for empty_or_done in ["data: [DONE]\n\n", ""] {
        for kind in ["openai-response", "grok-build"] {
            let evs = response_events(kind, empty_or_done).await;
            assert!(matches!(
                evs.as_slice(),
                [Err(ProviderError::Decode(m))] if m == "Responses stream ended without response.completed or response.incomplete"
            ));
        }
        let chat = response_events("openai", empty_or_done).await;
        assert!(matches!(
            chat.last(),
            Some(Ok(Event::MessageFinished {
                finish: FinishReason::Stop,
                ..
            }))
        ));
    }
    for kind in ["openai", "openai-response", "grok-build"] {
        assert!(matches!(
            response_events(kind, "data: {not-json}\n\n")
                .await
                .as_slice(),
            [Err(ProviderError::Json(_))]
        ));
    }

    // 3. Nested response.failed surfaces error message.
    let failed = response_events(
        "openai-response",
        "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"quota exhausted\"}}}\n\n",
    )
    .await;
    assert!(matches!(failed.as_slice(), [Err(ProviderError::Http(m))] if m == "quota exhausted"));

    // 4. Full Responses decoding: reasoning summary + encrypted_content + 2 parallel tools + text + normalized usage.
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
    let (url, _rx) = start_sse_server(sse).await;
    let p = route(
        "openai",
        "openai-response",
        &url,
        Some("t".into()),
        ["gpt-5.6-sol".into()],
    )
    .unwrap();
    let mut r = req("openai/gpt-5.6-sol");
    r.reasoning = Some(ReasoningEffort::Low);
    let evs: Vec<Event> = p
        .stream(r, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(evs.len(), 13);
    assert_eq!(serde_json::to_value(&evs[0]).unwrap()["reason"], "low");
    assert!(matches!(
        &evs[12],
        Event::MessageFinished {
            finish: FinishReason::ToolCalls,
            tokens: Some(TokenUsage {
                input: 8,
                output: 7,
                reasoning: 2,
                cache_read: 3,
                cache_write: 0,
                reasoning_unknown: false
            }),
            ..
        }
    ));

    // 5. Replays completed Responses reasoning + tool round verbatim over HTTP.
    let (rep_url, rep_rx) = start_sse_server(
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
            .into(),
    )
    .await;
    let rep_p = route(
        "openai",
        "openai-response",
        &rep_url,
        Some("t".into()),
        ["gpt-5.6-sol".into()],
    )
    .unwrap();
    let call_id = ToolCallId::new();
    let pdata = json!({"id": "rs_123", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Need a file."}], "encrypted_content": "opaque"});
    let rep_req = CompletionRequest {
        reasoning: Some(ReasoningEffort::Medium),
        messages: vec![
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "read a.txt".into(),
                }],
            },
            Message::Assistant {
                id: MessageId::new(),
                agent: AgentName::new("build"),
                model: ModelRef::new("openai/gpt-5.6-sol"),
                parts: vec![
                    Part::Reasoning {
                        id: PartId::new(),
                        text: "Need a file.".into(),
                        provider_data: Some(pdata.clone()),
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
                        provider_data: None,
                    },
                ],
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
        ],
        ..req("openai/gpt-5.6-sol")
    };
    let _ = rep_p
        .stream(rep_req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;
    let body: Value = serde_json::from_str(&captured_request(rep_rx).await.body).unwrap();
    assert_eq!(body["input"][1], pdata);
    assert_eq!(body["input"][2]["call_id"], call_id.to_string());
    assert_eq!(body["input"][3]["output"], "contents");
}

#[tokio::test]
async fn http_provider_responses_terminal_output_and_summary_edge_cases() {
    // 1. Terminal output closes unfinished reasoning (streamed or unstreamed) once, and never recloses finished reasoning.
    let item = json!({"id": "rs_1", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Plan."}], "encrypted_content": "opaque"});
    let completed = json!({"type": "response.completed", "response": {"status": "completed", "output": [item, {"type": "message", "content": []}], "input_transformations": [{"action": "removed", "type": "reasoning", "reason": "prefix_mismatch"}]}});
    for streamed in [
        "",
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Plan.\"}\n\n",
    ] {
        let evs = response_events(
            "openai-response",
            &format!("{streamed}data: {completed}\n\n"),
        )
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, Event::ReasoningEnd { .. }))
                .count(),
            1
        );
        assert!(evs.iter().any(
            |e| matches!(e, Event::ReasoningEnd { provider_data: Some(d), .. } if d == &item)
        ));
    }
    let already_closed = concat!(
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"summary_index\":0,\"delta\":\"Plan.\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"id\":\"rs_1\",\"type\":\"reasoning\",\"encrypted_content\":\"opaque\"}]}}\n\n",
    );
    let closed_evs = response_events("openai-response", already_closed)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        closed_evs
            .iter()
            .filter(|e| matches!(e, Event::ReasoningEnd { .. }))
            .count(),
        1
    );

    // 2. Announced reasoning keeps its place before a streamed call when folded and replayed;
    //    unannounced terminal reasoning after streamed call is skipped.
    let reasoning =
        json!({"id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "opaque"});
    let fc = json!({"id": "fc_1", "type": "function_call", "call_id": "call_up", "name": "read", "arguments": "{\"path\":\"a.txt\"}"});
    let announced_sse = [
        json!({"type": "response.output_item.added", "output_index": 0, "item": {"id": "rs_1", "type": "reasoning", "summary": []}}),
        json!({"type": "response.output_item.added", "output_index": 1, "item": {"id": "fc_1", "type": "function_call", "call_id": "call_up", "name": "read", "arguments": ""}}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": fc}),
        json!({"type": "response.completed", "response": {"status": "completed", "output": [reasoning, fc]}}),
    ]
    .iter()
    .map(|f| format!("data: {f}\n\n"))
    .collect::<String>();
    let (session, user, message) = (SessionId::new(), MessageId::new(), MessageId::new());
    let (u, _) = start_sse_server(announced_sse).await;
    let p = route(
        "openai",
        "openai-response",
        &u,
        Some("t".into()),
        ["model".into()],
    )
    .unwrap();
    let mut r = req("openai/model");
    r.reasoning = Some(ReasoningEffort::High);
    let decoded: Vec<Event> = p
        .stream(r, session, message)
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap();
    let (tool_part, call) = decoded
        .iter()
        .find_map(|e| match e {
            Event::ToolCallRequested { part, call, .. } => Some((*part, *call)),
            _ => None,
        })
        .unwrap();
    let user_part = PartId::new();
    let mut log = vec![
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("openai/model"),
            workdir: "/tmp".into(),
            project: None,
            kind: SessionKind::Project,
        },
        Event::MessageStarted {
            session,
            message: user,
            role: Role::User,
            agent: None,
            model: None,
        },
        Event::TextStart {
            session,
            message: user,
            part: user_part,
        },
        Event::TextDelta {
            session,
            message: user,
            part: user_part,
            delta: "read a.txt".into(),
        },
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
    ];
    log.extend(decoded);
    log.push(Event::ToolResult {
        session,
        message,
        part: tool_part,
        call,
        output: json!("contents"),
        time_ms: 3,
    });
    let proj = Projection::from_events(
        &(1..)
            .zip(log)
            .map(|(s, event)| Envelope {
                seq: EventSeq(s),
                ts_millis: 0,
                event,
            })
            .collect::<Vec<_>>(),
    );
    let parts = proj
        .session
        .messages
        .iter()
        .find(|row| row.id == message)
        .unwrap()
        .parts
        .iter()
        .map(|part| match part {
            PartProjection::Text { id, text } => Part::Text {
                id: *id,
                text: text.clone(),
            },
            PartProjection::Reasoning {
                id,
                text,
                provider_data,
                ..
            } => Part::Reasoning {
                id: *id,
                text: text.clone(),
                provider_data: provider_data.clone(),
            },
            PartProjection::Tool {
                id,
                call,
                name,
                state,
                provider_data,
                ..
            } => Part::Tool {
                id: *id,
                call_id: *call,
                name: name.clone(),
                state: state.clone(),
                provider_data: provider_data.clone(),
            },
        })
        .collect();
    let next = CompletionRequest {
        reasoning: Some(ReasoningEffort::High),
        messages: vec![
            Message::User {
                id: user,
                parts: vec![Part::Text {
                    id: user_part,
                    text: "read a.txt".into(),
                }],
            },
            Message::Assistant {
                id: message,
                agent: AgentName::new("build"),
                model: ModelRef::new("openai/model"),
                parts,
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
        ],
        ..req("openai/model")
    };
    let encoded = OpenAiResponsesProtocol.encode(&next).unwrap();
    assert_eq!(encoded["input"][1], reasoning);
    assert_eq!(encoded["input"][2]["call_id"], "call_up");

    let unannounced_sse = [
        json!({"type": "response.output_item.added", "output_index": 1, "item": fc}),
        json!({"type": "response.output_item.done", "output_index": 1, "item": fc}),
        json!({"type": "response.completed", "response": {"status": "completed", "output": [reasoning, fc]}}),
    ]
    .iter()
    .map(|f| format!("data: {f}\n\n"))
    .collect::<String>();
    let unannounced_evs = response_events("openai-response", &unannounced_sse)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        !unannounced_evs
            .iter()
            .any(|e| matches!(e, Event::ReasoningStart { .. } | Event::ReasoningEnd { .. }))
    );

    // 3. Summary-only done replaced by encrypted terminal item; earlier open text closes before terminal reasoning;
    //    response.incomplete closes reasoning before Length; summary parts join with \n\n.
    let summary = json!({"id": "rs_1", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Need a file."}]});
    let mut encrypted = summary.clone();
    encrypted["encrypted_content"] = json!("opaque");
    for (term, expected) in [
        (
            json!({"type": "response.completed", "response": {"status": "completed", "output": [encrypted]}}),
            encrypted.clone(),
        ),
        (
            json!({"type": "response.completed", "response": {"status": "completed"}}),
            summary.clone(),
        ),
    ] {
        let sse = [
            json!({"type": "response.reasoning_summary_text.delta", "output_index": 0, "summary_index": 0, "delta": "Need a file."}),
            json!({"type": "response.output_item.done", "output_index": 0, "item": summary}),
            term,
        ].iter().map(|f| format!("data: {f}\n\n")).collect::<String>();
        let evs = response_events("openai-response", &sse)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(matches!(
            evs.as_slice(),
            [Event::ReasoningStart { .. }, Event::ReasoningDelta { delta, .. }, Event::ReasoningEnd { provider_data: Some(d), .. }, Event::MessageFinished { .. }]
                if delta == "Need a file." && d == &expected
        ));
    }

    let text_then_r = [
        json!({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": "Reading"}),
        json!({"type": "response.completed", "response": {"status": "completed", "output": [{"type": "message", "content": [{"type": "output_text", "text": "Reading"}]}, reasoning]}}),
    ].iter().map(|f| format!("data: {f}\n\n")).collect::<String>();
    let tr_evs = response_events("openai-response", &text_then_r)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(matches!(
        tr_evs.as_slice(),
        [
            Event::TextStart { .. },
            Event::TextDelta { .. },
            Event::TextEnd { .. },
            Event::ReasoningStart { .. },
            Event::ReasoningEnd {
                provider_data: Some(_),
                ..
            },
            Event::MessageFinished { .. }
        ]
    ));

    let inc_evs = response_events(
        "openai-response",
        &format!("data: {}\n\n", json!({"type": "response.incomplete", "response": {"status": "incomplete", "output": [reasoning]}})),
    ).await.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert!(matches!(
        inc_evs.as_slice(),
        [
            Event::ReasoningStart { .. },
            Event::ReasoningEnd {
                provider_data: Some(_),
                ..
            },
            Event::MessageFinished {
                finish: FinishReason::Length,
                ..
            }
        ]
    ));

    for part_added in [
        "",
        "data: {\"type\":\"response.reasoning_summary_part.added\",\"output_index\":0,\"summary_index\":1,\"part\":{\"type\":\"summary_text\",\"text\":\"\"}}\n\n",
    ] {
        let idx = if part_added.is_empty() {
            "\"summary_index\":1,"
        } else {
            ""
        };
        let sse = format!(
            "data: {{\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,\"summary_index\":0,\"delta\":\"A\"}}\n\n{part_added}data: {{\"type\":\"response.reasoning_summary_text.delta\",\"output_index\":0,{idx}\"delta\":\"B\"}}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{\"status\":\"completed\"}}}}\n\n"
        );
        let text: String = response_events("openai-response", &sse)
            .await
            .into_iter()
            .map(Result::unwrap)
            .filter_map(|e| match e {
                Event::ReasoningDelta { delta, .. } => Some(delta),
                _ => None,
            })
            .collect();
        assert_eq!(text, "A\n\nB");
    }
}

#[tokio::test]
async fn http_provider_anthropic_body_cache_breakpoints_thinking_and_binding_beta() {
    let sse = sse_ok(&[
        r#"data: {"type":"message_start","message":{"id":"msg_1","model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":1}}}"#,
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"mock anthropic delta"}}"#,
        r#"data: {"type":"content_block_stop","index":0}"#,
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
        r#"data: {"type":"message_stop"}"#,
    ]);

    // 1. Anthropic cache_control breakpoints + headers + body.
    let (url, _, reqs) = start_scripted_server(vec![sse.clone(); 5]).await;
    let p_sonnet = route(
        "anthropic",
        "anthropic",
        &url,
        Some("test-token".into()),
        ["claude-sonnet-4-20250514".into()],
    )
    .unwrap();
    let r1 = CompletionRequest {
        system: Some("be helpful".into()),
        max_output_tokens: Some(128),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "explain the file".into(),
            }],
        }],
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            output_schema: None,
        }],
        ..req("anthropic/claude-sonnet-4-20250514")
    };
    let evs: Vec<_> = p_sonnet
        .stream(r1, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect()
        .await;
    assert!(evs.iter().any(
        |e| matches!(e, Ok(Event::TextDelta { delta, .. }) if delta == "mock anthropic delta")
    ));

    let turn2_messages = vec![
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "start".into(),
            }],
        },
        Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new("claude-opus-4-7"),
            parts: vec![
                Part::Reasoning {
                    id: PartId::new(),
                    text: "reason".into(),
                    provider_data: Some(
                        json!({"type": "thinking", "thinking": "reason", "signature": "sig_1"}),
                    ),
                },
                Part::Tool {
                    id: PartId::new(),
                    call_id: ToolCallId::new(),
                    name: ToolName::new("bash"),
                    state: ToolPartState::Completed {
                        input: json!({}),
                        output: json!("ok"),
                        time_ms: 1,
                    },
                    provider_data: Some(json!({"call_id": "toolu_1"})),
                },
            ],
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: None,
        },
    ];

    // 2..5. thinking-binding-controls-2026-08-01 beta header matrix + signed thinking replay.
    let p_opus = route(
        "anthropic",
        "anthropic",
        &url,
        Some("t".into()),
        [
            "claude-opus-4-7".into(),
            "claude-sonnet-4-5-20250929".into(),
        ],
    )
    .unwrap();
    let mut merged_headers = BTreeMap::new();
    merged_headers.insert(
        "Anthropic-Beta".into(),
        "interleaved-thinking-2025-05-14".into(),
    );
    let matrix = [
        (
            "anthropic/claude-opus-4-7",
            turn2_messages.clone(),
            Some(ReasoningEffort::High),
            merged_headers,
            true,
        ),
        (
            "anthropic/claude-opus-4-7",
            vec![turn2_messages[0].clone()],
            Some(ReasoningEffort::High),
            BTreeMap::new(),
            false,
        ),
        (
            "anthropic/claude-opus-4-7",
            turn2_messages.clone(),
            Some(ReasoningEffort::Off),
            BTreeMap::new(),
            false,
        ),
        (
            "anthropic/claude-sonnet-4-5-20250929",
            turn2_messages,
            Some(ReasoningEffort::High),
            BTreeMap::new(),
            false,
        ),
    ];
    for (model, messages, reasoning, headers, _) in &matrix {
        let mut r = req(model);
        r.messages = messages.clone();
        r.reasoning = *reasoning;
        r.headers = headers.clone();
        let _ = p_opus
            .stream(r, SessionId::new(), MessageId::new())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
    }

    let cap = reqs.lock().unwrap();
    // Assert #1 cache breakpoints & headers.
    let lc0 = cap[0].headers.to_ascii_lowercase();
    assert!(lc0.contains("x-api-key: test-token") && lc0.contains("anthropic-version: 2023-06-01"));
    assert!(cap[0].raw.starts_with("POST /messages HTTP/1.1\r\n"));
    let b0: Value = serde_json::from_str(&cap[0].body).unwrap();
    assert_eq!(
        b0["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        b0["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        b0["messages"][0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );

    // Assert #2 signed thinking before tool_use + beta header merge.
    let b1: Value = serde_json::from_str(&cap[1].body).unwrap();
    assert_eq!(
        b1["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "reason", "signature": "sig_1"})
    );
    assert_eq!(b1["messages"][1]["content"][1]["type"], "tool_use");
    let beta_line = cap[1]
        .headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("anthropic-beta:"))
        .unwrap();
    assert!(beta_line.contains("interleaved-thinking-2025-05-14"));
    assert_eq!(
        beta_line
            .matches("thinking-binding-controls-2026-08-01")
            .count(),
        1
    );

    for (idx, (_, _, _, _, expect_beta)) in matrix.iter().enumerate() {
        let has_beta = cap[idx + 1]
            .headers
            .to_ascii_lowercase()
            .contains("thinking-binding-controls-2026-08-01");
        assert_eq!(has_beta, *expect_beta, "case {idx}");
    }
}

#[tokio::test]
async fn http_provider_google_route_thought_policy_media_and_gemini3_thinking_level() {
    let (url, rx) = start_sse_server(
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"plan\",\"thoughtSignature\":\"s1\"},{\"text\":\"ok\"}],\"role\":\"model\"},\"finishReason\":\"STOP\"}]}\n\n".into(),
    )
    .await;
    let provider = route(
        "google",
        "google",
        &url,
        Some("g-token".into()),
        ["gemini-3-pro-preview".into()],
    )
    .unwrap();
    assert_eq!(
        provider.reasoning_replay_policy(),
        hya_provider::ReasoningReplayPolicy::GoogleThought
    );
    assert!(!provider.reasoning_replay_policy().expands_next_prompt());

    let r = CompletionRequest {
        system: Some("sys".into()),
        reasoning: Some(ReasoningEffort::Medium),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![
                Part::Text {
                    id: PartId::new(),
                    text: "hi".into(),
                },
                Part::Media {
                    id: PartId::new(),
                    media_type: "AuDio/WAV".into(),
                    data: "data:AUDIO/wav;BASE64,Zg==".into(),
                    filename: None,
                },
            ],
        }],
        ..req("google/gemini-3-pro-preview")
    };
    let evs: Vec<_> = provider
        .stream(r, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let captured = captured_request(rx).await;
    assert!(
        captured
            .headers
            .lines()
            .any(|l| l == "x-goog-api-key: g-token")
    );
    assert!(captured.raw.starts_with(
        "POST /v1beta/models/gemini-3-pro-preview:streamGenerateContent?alt=sse HTTP/1.1\r\n"
    ));
    let body: Value = serde_json::from_str(&captured.body).unwrap();
    assert!(
        captured
            .body
            .contains(r#"{"inlineData":{"mimeType":"audio/wav","data":"Zg=="}}"#)
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "medium", "includeThoughts": true})
    );
    assert!(evs.iter().any(|e| matches!(
        e, Event::ReasoningEnd { provider_data: Some(d), .. } if d["type"] == "google_thought" && d["thought_signature"] == "s1"
    )));
}

#[tokio::test]
async fn responses_compact_headers_body_multibyte_error_and_header_deadline() {
    // 1. Grok compaction preserves proxy headers & body.
    let ok_json = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 13\r\nconnection: close\r\n\r\n{\"output\":[]}";
    let body_429 = "界".repeat(600);
    let err_429 = format!(
        "HTTP/1.1 429 Too Many Requests\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\nretry-after: 2\r\nconnection: close\r\n\r\n{body_429}",
        body_429.len()
    );
    let (url, conns, reqs) = start_scripted_server(vec![ok_json.into(), err_429]).await;
    let grok = route(
        "grok",
        "grok-build",
        &format!("{url}/v1/"),
        Some("GrokToken".into()),
        ["grok-4.5".into()],
    )
    .unwrap();
    grok.compact_responses(&ModelRef::new("grok/grok-4.5"), &[], Some("summary"))
        .await
        .unwrap();

    // 2. Multibyte 429 error bounds cleanly to 500 chars and parses retry-after.
    let oai = route("openai", "openai-response", &url, None, ["gpt-5".into()]).unwrap();
    let error = oai
        .compact_responses(&ModelRef::new("gpt-5"), &[], None)
        .await
        .unwrap_err();
    let ProviderError::HttpStatus {
        status,
        message,
        retry_after,
    } = error
    else {
        panic!("expected HttpStatus, got {error:?}");
    };
    assert_eq!(
        (status, message.chars().count(), retry_after),
        (429, 500, Some(Duration::from_secs(2)))
    );
    assert_eq!(conns.load(Ordering::SeqCst), 2);
    {
        let cap = reqs.lock().unwrap();
        assert!(
            cap[0]
                .raw
                .starts_with("POST /v1/responses/compact HTTP/1.1\r\n")
        );
        let version = format!("x-grok-client-version: {}", hya_version::BACKEND_VERSION);
        for h in [
            "authorization: Bearer GrokToken",
            "x-xai-token-auth: xai-grok-cli",
            &version,
            "x-grok-client-identifier: grok-cli",
            "x-grok-model-override: grok-4.5",
        ] {
            assert!(cap[0].headers.lines().any(|l| l == h), "missing {h}");
        }
        assert_eq!(
            cap[0].body,
            r#"{"model":"grok-4.5","input":[{"role":"system","content":"summary"}]}"#
        );
        assert!(cap[1].raw.starts_with("POST /responses/compact "));
    }

    // 3. Compaction honors route response_header_timeout without retry.
    let (stall_url, stall_conns) = start_stalled_header_server().await;
    let stall_p = route(
        "openai",
        "openai-response",
        &stall_url,
        None,
        ["gpt-5".into()],
    )
    .unwrap()
    .with_response_header_timeout(Duration::from_millis(50));
    let res = stall_p
        .compact_responses(&ModelRef::new("gpt-5"), &[], None)
        .await;
    assert!(matches!(res, Err(ProviderError::Transport(m)) if m.contains("50ms")));
    assert_eq!(stall_conns.load(Ordering::SeqCst), 1);
}

/// A protocol with its own compaction wire and paginated model list.
struct WireProtocol;

struct TestCompactWire;

impl hya_provider::CompactWire for TestCompactWire {
    fn endpoint(&self, base: &str) -> String {
        format!("{base}/compact-it")
    }
    fn encode(
        &self,
        messages: &[Message],
        system: Option<&str>,
        model_id: &str,
        images: bool,
    ) -> Result<Value, ProviderError> {
        Ok(json!({"model": model_id, "count": messages.len(), "system": system, "images": images}))
    }
    fn parse(&self, payload: &Value) -> Result<Vec<Value>, ProviderError> {
        payload["kept"]
            .as_array()
            .cloned()
            .ok_or_else(|| ProviderError::Decode("no kept".into()))
    }
}

impl Protocol for WireProtocol {
    fn kind(&self) -> &str {
        "test-wire"
    }
    fn description(&self) -> &str {
        "test protocol"
    }
    fn compaction(&self) -> Option<&dyn hya_provider::CompactWire> {
        Some(&TestCompactWire)
    }
    fn models_page_url(
        &self,
        endpoint: &str,
        cursor: Option<&str>,
    ) -> Result<String, hya_provider::CatalogFailure> {
        Ok(match cursor {
            Some(cursor) => format!("{endpoint}?page={cursor}"),
            None => endpoint.to_string(),
        })
    }
    fn models_next_cursor(
        &self,
        payload: &Value,
    ) -> Result<Option<String>, hya_provider::CatalogFailure> {
        Ok(payload["next"].as_str().map(str::to_string))
    }
    fn encode(&self, req: &CompletionRequest) -> Result<Value, ProviderError> {
        OpenAiChatProtocol.encode(req)
    }
    fn decoder(
        &self,
        session: SessionId,
        message: MessageId,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn hya_provider::Decoder> {
        OpenAiChatProtocol.decoder(session, message, reasoning)
    }
}

fn json_ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[tokio::test]
async fn compact_responses_uses_protocol_compact_wire() {
    let (url, _, reqs) = start_scripted_server(vec![json_ok(r#"{"kept":[{"x":1}]}"#)]).await;
    let provider = HttpProvider::new(
        "wire",
        Arc::new(WireProtocol),
        &url,
        credential("tok", None),
        ["m".into()],
    )
    .unwrap();
    let window = provider
        .compact_responses(&ModelRef::new("wire/m"), &[], Some("sys"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(window.items, [json!({"x": 1})]);
    {
        let cap = reqs.lock().unwrap();
        assert!(cap[0].raw.starts_with("POST /compact-it HTTP/1.1\r\n"));
        assert!(
            cap[0]
                .headers
                .lines()
                .any(|l| l == "authorization: Bearer tok")
        );
        assert_eq!(
            serde_json::from_str::<Value>(&cap[0].body).unwrap(),
            json!({"model": "m", "count": 0, "system": "sys", "images": false})
        );
    }

    // A protocol without a compaction wire has no native compaction.
    let chat = route("chat", "openai", &url, None, ["m".into()]).unwrap();
    assert!(
        chat.compact_responses(&ModelRef::new("chat/m"), &[], None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn discovery_loop_follows_protocol_pagination_within_bounds() {
    let pages = vec![
        json_ok(r#"{"data":[{"id":"a"}],"next":"2"}"#),
        json_ok(r#"{"data":[{"id":"b"},{"id":"a"}],"next":"3"}"#),
        json_ok(r#"{"data":[{"id":"c"}]}"#),
    ];
    let (url, _, reqs) = start_scripted_server(pages).await;
    let outcome = discover_models(CatalogDiscoveryRequest::new(
        "wire",
        Arc::new(WireProtocol),
        &url,
        None,
    ))
    .await;
    let hya_provider::ProviderDiscoveryOutcome::Discovered { models, auth } = outcome else {
        panic!("expected models");
    };
    assert_eq!(auth, hya_provider::AuthPresence::Unauthenticated);
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    let paths: Vec<_> = reqs
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.raw.lines().next().unwrap().to_string())
        .collect();
    assert_eq!(
        paths,
        [
            "GET /models HTTP/1.1",
            "GET /models?page=2 HTTP/1.1",
            "GET /models?page=3 HTTP/1.1"
        ]
    );

    // A repeated cursor ends the loop instead of following it forever.
    let looping = vec![json_ok(r#"{"data":[{"id":"a"}],"next":"1"}"#); 2];
    let (url, conns, _) = start_scripted_server(looping).await;
    let outcome = discover_models(CatalogDiscoveryRequest::new(
        "wire",
        Arc::new(WireProtocol),
        &url,
        None,
    ))
    .await;
    assert!(matches!(
        outcome,
        hya_provider::ProviderDiscoveryOutcome::Failed {
            error: hya_provider::CatalogFailure::PaginationLimit
        }
    ));
    assert_eq!(conns.load(Ordering::SeqCst), 2);
}

// ---- mock HTTP test servers ----

async fn response_events(kind: &str, response: &str) -> Vec<Result<Event, ProviderError>> {
    let (base_url, _rx) = start_sse_server(response.to_string()).await;
    let provider = route(
        "test",
        kind,
        &base_url,
        Some("test-token".into()),
        ["model".into()],
    )
    .unwrap();
    provider
        .stream(req("test/model"), SessionId::new(), MessageId::new())
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
        assert!(n != 0, "socket closed before request headers");
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
}

async fn start_stalled_header_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = Arc::clone(&attempts);
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let seq = server_attempts.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut socket = socket;
                read_request_head(&mut socket).await;
                if seq == 0 {
                    futures::future::pending::<()>().await;
                }
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n")
                    .await;
            });
        }
    });
    (format!("http://{addr}"), attempts)
}

async fn start_keepalive_sse_server() -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request_head(&mut socket).await;
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0_u8; 8];
        loop {
            tokio::select! {
                res = socket.read(&mut buf) => {
                    if matches!(res, Ok(0) | Err(_)) {
                        let _ = tx.send(());
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    if socket.write_all(b": ping\n\n").await.is_err() {
                        let _ = tx.send(());
                        return;
                    }
                }
            }
        }
    });
    (format!("http://{addr}"), rx)
}

async fn start_stalled_mid_stream_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let sc = Arc::clone(&connections);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            sc.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                read_request_head(&mut socket).await;
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n")
                    .await;
                futures::future::pending::<()>().await;
            });
        }
    });
    (format!("http://{addr}"), connections)
}

async fn start_concurrent_unauthorized_server(
    first_rotation: Arc<Notify>,
) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let sc = Arc::clone(&connections);
    tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        sc.fetch_add(1, Ordering::SeqCst);
        let (mut second, _) = listener.accept().await.unwrap();
        sc.fetch_add(1, Ordering::SeqCst);
        read_request_head(&mut first).await;
        read_request_head(&mut second).await;
        let unauth = b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        first.write_all(unauth).await.unwrap();
        first_rotation.notified().await;
        second.write_all(unauth).await.unwrap();
        for _ in 0..2 {
            let (mut retry, _) = listener.accept().await.unwrap();
            sc.fetch_add(1, Ordering::SeqCst);
            read_request_head(&mut retry).await;
            retry
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: [DONE]\n\n")
                .await
                .unwrap();
        }
    });
    (format!("http://{addr}"), connections)
}
