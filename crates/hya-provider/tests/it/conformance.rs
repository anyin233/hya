//! Cross-provider conformance of the shared protocol surface.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use hya_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, Role, SessionId,
    TokenUsage, ToolCallId, ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{
    AnthropicMessagesProtocol, Capabilities, CompletionRequest, EventStream, FakeProvider,
    FakeStep, GoogleProtocol, OpenAiChatProtocol, OpenAiResponsesProtocol, Protocol, Provider,
    ProviderError, ProviderRouter, ReasoningEffort,
};
use serde_json::json;

fn summarize(events: &[Event]) -> Vec<String> {
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

struct RecordingProvider {
    capabilities: Capabilities,
    recorded: Mutex<Option<CompletionRequest>>,
}

impl RecordingProvider {
    fn new(capabilities: Capabilities) -> Self {
        Self {
            capabilities,
            recorded: Mutex::new(None),
        }
    }
}

struct FailingProvider {
    calls: AtomicUsize,
    fail_after_stream_start: bool,
    retryable: bool,
}

#[async_trait]
impl Provider for FailingProvider {
    fn id(&self) -> &str {
        "failing"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities::default())
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
        _session: SessionId,
        _message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let failure = if self.retryable {
            ProviderError::Transport("connection reset".to_string())
        } else {
            ProviderError::HttpStatus {
                status: 400,
                message: "invalid request".to_string(),
                retry_after: None,
            }
        };
        if self.fail_after_stream_start {
            Ok(Box::pin(futures::stream::iter([Err(failure)])))
        } else {
            Err(failure)
        }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(self.capabilities.clone())
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        *self.recorded.lock().unwrap() = Some(req);
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

fn decode_all(protocol: &OpenAiChatProtocol, lines: &[&str]) -> Vec<Event> {
    let s = SessionId::new();
    let m = MessageId::new();
    let mut decoder = protocol.decoder(s, m, None);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

fn finished_tokens(events: &[Event]) -> Option<TokenUsage> {
    events.iter().find_map(|event| {
        if let Event::MessageFinished { tokens, .. } = event {
            *tokens
        } else {
            None
        }
    })
}

#[tokio::test]
async fn fake_provider_round_trips_canonical_events() {
    let provider = FakeProvider::scripted(vec![
        FakeStep::Text("Hello world".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ]);
    let router = ProviderRouter::new().with(Arc::new(provider));
    let req = CompletionRequest {
        model: ModelRef::new("fake"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let stream = router
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap();
    let events: Vec<Event> = stream.map(Result::unwrap).collect().await;
    assert_eq!(
        summarize(&events),
        vec![
            "text_start",
            "text_delta:Hello world",
            "text_end",
            "finish:Stop",
        ]
    );
}

/// An empty router rejects an unconfigured model before any provider traffic.
#[tokio::test]
async fn empty_router_returns_unknown_model() {
    let request = CompletionRequest {
        model: ModelRef::new("locally-unconfigured"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let error = match ProviderRouter::new()
        .stream(request, SessionId::new(), MessageId::new())
        .await
    {
        Ok(_) => panic!("an empty router unexpectedly accepted a model"),
        Err(error) => error,
    };
    assert!(matches!(error, ProviderError::UnknownModel(model) if model == "locally-unconfigured"));
}

#[tokio::test]
async fn router_clears_reasoning_for_provider_without_reasoning_request() {
    let provider = Arc::new(RecordingProvider::new(Capabilities {
        reasoning_request: false,
        ..Capabilities::default()
    }));
    let router = ProviderRouter::new().with(provider.clone());
    let req = CompletionRequest {
        model: ModelRef::new("recording"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::High),
        headers: Default::default(),
    };

    let _stream = router
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap();

    let recorded = provider.recorded.lock().unwrap();
    assert_eq!(recorded.as_ref().unwrap().reasoning, None);
}

#[tokio::test]
async fn router_returns_retryable_pre_stream_failure_without_failover() {
    let primary = Arc::new(FailingProvider {
        calls: AtomicUsize::new(0),
        fail_after_stream_start: false,
        retryable: true,
    });
    let backup = Arc::new(RecordingProvider::new(Capabilities::default()));
    let router = ProviderRouter::new()
        .with(primary.clone())
        .with(backup.clone());
    let req = CompletionRequest {
        model: ModelRef::new("shared-model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let result = router.stream(req, SessionId::new(), MessageId::new()).await;
    assert!(matches!(result, Err(ProviderError::Transport(_))));
    assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
    assert!(backup.recorded.lock().unwrap().is_none());
}

#[tokio::test]
async fn router_does_not_fail_over_after_non_retryable_failure() {
    let primary = Arc::new(FailingProvider {
        calls: AtomicUsize::new(0),
        fail_after_stream_start: false,
        retryable: false,
    });
    let backup = Arc::new(RecordingProvider::new(Capabilities::default()));
    let router = ProviderRouter::new()
        .with(primary.clone())
        .with(backup.clone());
    let req = CompletionRequest {
        model: ModelRef::new("shared-model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let error = match router.stream(req, SessionId::new(), MessageId::new()).await {
        Err(error) => error,
        Ok(_) => panic!("non-retryable failure must be returned"),
    };

    assert!(matches!(
        error,
        ProviderError::HttpStatus { status: 400, .. }
    ));
    assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
    assert!(backup.recorded.lock().unwrap().is_none());
}

#[tokio::test]
async fn router_does_not_fail_over_after_stream_is_returned() {
    let primary = Arc::new(FailingProvider {
        calls: AtomicUsize::new(0),
        fail_after_stream_start: true,
        retryable: true,
    });
    let backup = Arc::new(RecordingProvider::new(Capabilities::default()));
    let router = ProviderRouter::new()
        .with(primary.clone())
        .with(backup.clone());
    let req = CompletionRequest {
        model: ModelRef::new("shared-model"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let events = router
        .stream(req, SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .collect::<Vec<_>>()
        .await;

    assert!(matches!(
        events.as_slice(),
        [Err(ProviderError::Transport(_))]
    ));
    assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
    assert!(backup.recorded.lock().unwrap().is_none());
}

#[tokio::test]
async fn openai_decodes_streamed_text() {
    let protocol = OpenAiChatProtocol;
    let fixture = [
        r#"{"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"content":"Hel"},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        "[DONE]",
    ];
    let events = decode_all(&protocol, &fixture);
    assert_eq!(
        summarize(&events),
        vec![
            "text_start",
            "text_delta:Hel",
            "text_delta:lo",
            "text_end",
            "finish:Stop"
        ]
    );
}

#[tokio::test]
async fn openai_decodes_usage_when_usage_arrives_after_finish_reason() {
    let protocol = OpenAiChatProtocol;
    let fixture = [
        r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
        "[DONE]",
    ];

    let events = decode_all(&protocol, &fixture);

    assert_eq!(
        finished_tokens(&events),
        // Normalized: OpenAI `prompt_tokens` includes cached tokens.
        Some(TokenUsage {
            input: 6,
            output: 3,
            reasoning: 2,
            cache_read: 5,
            cache_write: 0,
            reasoning_unknown: false,
        })
    );
}

#[tokio::test]
async fn openai_usage_without_reasoning_details_reports_unknown_thinking() {
    let protocol = OpenAiChatProtocol;
    let fixture = [
        r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":8,"cache_creation_tokens":2}}}"#,
        "[DONE]",
    ];

    let events = decode_all(&protocol, &fixture);

    assert_eq!(
        finished_tokens(&events),
        Some(TokenUsage {
            input: 10,
            output: 4,
            reasoning: 0,
            cache_read: 8,
            cache_write: 2,
            reasoning_unknown: true,
        })
    );
}

#[tokio::test]
async fn openai_cached_tokens_never_underflow_input() {
    let protocol = OpenAiChatProtocol;
    let fixture = [
        r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#,
        r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":9},"completion_tokens_details":{"reasoning_tokens":0}}}"#,
        "[DONE]",
    ];

    let events = decode_all(&protocol, &fixture);

    let tokens = finished_tokens(&events).unwrap();
    assert_eq!((tokens.input, tokens.cache_read), (0, 9));
}

#[tokio::test]
async fn openai_decodes_streamed_tool_call() {
    let protocol = OpenAiChatProtocol;
    let fixture = [
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"/tmp/a\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let events = decode_all(&protocol, &fixture);
    assert_eq!(
        summarize(&events),
        vec![
            "tool_start:read",
            "tool_input_delta",
            "tool_input_delta",
            "tool_call:read:{\"path\":\"/tmp/a\"}",
            "finish:ToolCalls",
        ]
    );
}

#[test]
fn openai_chat_replays_streamed_reasoning_content_with_tool_calls() {
    let protocol = OpenAiChatProtocol;
    let events = decode_all(
        &protocol,
        &[
            r#"{"choices":[{"delta":{"reasoning_content":"Check "},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"the repo","content":"I'll look."},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    let reasoning = events
        .iter()
        .filter_map(|event| match event {
            Event::ReasoningDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect::<String>();
    let provider_data = events.iter().find_map(|event| match event {
        Event::ReasoningEnd { provider_data, .. } => provider_data.clone(),
        _ => None,
    });
    assert_eq!(reasoning, "Check the repo");
    assert!(
        provider_data.is_some(),
        "reasoning replay marker must survive the stream"
    );
    let text_end = events
        .iter()
        .position(|event| matches!(event, Event::TextEnd { .. }))
        .expect("text part closes");
    let tool_start = events
        .iter()
        .position(|event| matches!(event, Event::ToolInputStart { .. }))
        .expect("tool input starts");
    assert!(
        text_end < tool_start,
        "text must close before live tool input"
    );

    let mut request = assistant_tool_request(json!({ "path": "a" }));
    request.tools.push(ToolSchema {
        name: ToolName::new("read"),
        description: "Read a file".to_string(),
        input_schema: json!({"type":"object"}),
        output_schema: None,
    });
    let Message::Assistant { parts, .. } = &mut request.messages[1] else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: reasoning,
            provider_data,
        },
    );
    let body = protocol.encode(&request).expect("encode replay");
    assert_eq!(body["messages"][1]["reasoning_content"], "Check the repo");
    assert_eq!(body["messages"][1]["content"], "ok");
    assert_eq!(
        body["messages"][1]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(body["messages"][2]["role"], "tool");
}

#[test]
fn openai_chat_deepseek_replays_empty_reasoning_content_for_tool_turn() {
    let protocol = OpenAiChatProtocol;
    let mut request = assistant_tool_request(json!({"command": "pwd"}));
    request.model = ModelRef::new("deepseek-flash");
    request.tools.push(ToolSchema {
        name: ToolName::new("read"),
        description: "Read a file".to_string(),
        input_schema: json!({"type":"object"}),
        output_schema: None,
    });

    let body = protocol.encode(&request).expect("encode replay");
    assert_eq!(body["messages"][1]["reasoning_content"], "");
    assert_eq!(body["messages"][1]["content"], "ok");
    assert_eq!(body["messages"][2]["role"], "tool");
}

#[test]
fn openai_chat_replays_reasoning_from_older_sessions() {
    let mut request = assistant_tool_request(json!({"path": "a"}));
    request.model = ModelRef::new("kimi-thinking");
    let Message::Assistant { parts, .. } = &mut request.messages[1] else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: "old thought".into(),
            provider_data: Some(json!({"openai_chat_reasoning_content": true})),
        },
    );

    let body = OpenAiChatProtocol
        .encode(&request)
        .expect("encode historical reasoning");
    assert_eq!(body["messages"][1]["reasoning_content"], "old thought");
}

#[test]
fn openai_chat_keeps_an_assistant_with_only_reasoning() {
    let mut request = assistant_tool_request(json!({"path": "a"}));
    request.model = ModelRef::new("kimi-thinking");
    let Message::Assistant { parts, .. } = &mut request.messages[1] else {
        panic!("assistant fixture");
    };
    *parts = vec![Part::Reasoning {
        id: PartId::new(),
        text: "thinking".into(),
        provider_data: Some(json!({"type": "reasoning_content"})),
    }];

    let body = OpenAiChatProtocol
        .encode(&request)
        .expect("encode reasoning-only reply");
    assert_eq!(body["messages"][1]["reasoning_content"], "thinking");
    assert_eq!(body["messages"][1]["content"], "");
}

#[tokio::test]
async fn fake_and_openai_agree_on_canonical_shape() {
    let protocol = OpenAiChatProtocol;
    let openai = decode_all(
        &protocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hello world"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ],
    );
    let fake = FakeProvider::materialize(
        &[
            FakeStep::Text("Hello world".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        SessionId::new(),
        MessageId::new(),
    );
    assert_eq!(summarize(&openai), summarize(&fake));
}

fn assistant_tool_request(input: serde_json::Value) -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new("m"),
        system: None,
        messages: vec![
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "read it".to_string(),
                }],
            },
            Message::Assistant {
                id: MessageId::new(),
                agent: AgentName::new("build"),
                model: ModelRef::new("m"),
                parts: vec![
                    Part::Text {
                        id: PartId::new(),
                        text: "ok".to_string(),
                    },
                    Part::Tool {
                        id: PartId::new(),
                        call_id: ToolCallId::new(),
                        name: ToolName::new("read"),
                        state: ToolPartState::Completed {
                            input,
                            output: json!("hello"),
                            time_ms: 3,
                        },
                        provider_data: None,
                    },
                ],
                finish: None,
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

#[test]
fn fake_provider_materializes_reasoning_tool_usage_and_finish() {
    let events = FakeProvider::materialize(
        &[
            FakeStep::Reasoning("think".to_string()),
            FakeStep::ToolCall {
                name: "read".to_string(),
                input: json!({ "path": "a" }),
            },
            FakeStep::Usage(TokenUsage {
                input: 1,
                output: 2,
                reasoning: 3,
                cache_read: 0,
                cache_write: 0,
                reasoning_unknown: false,
            }),
            FakeStep::Finish(FinishReason::Stop),
        ],
        SessionId::new(),
        MessageId::new(),
    );

    assert_eq!(
        summarize(&events),
        vec![
            "reasoning_start",
            "reasoning_delta:think",
            "reasoning_end",
            "tool_start:read",
            "tool_input_delta",
            "tool_call:read:{\"path\":\"a\"}",
            "finish:Stop",
        ]
    );
    assert_eq!(
        finished_tokens(&events),
        Some(TokenUsage {
            input: 1,
            output: 2,
            reasoning: 3,
            cache_read: 0,
            cache_write: 0,
            reasoning_unknown: false,
        })
    );
}

#[test]
fn openai_encodes_tool_call_and_result() {
    let body = OpenAiChatProtocol
        .encode(&assistant_tool_request(json!({ "path": "a" })))
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, vec!["user", "assistant", "tool"]);
    let asst = &msgs[1];
    assert_eq!(asst["content"], "ok");
    assert_eq!(asst["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(
        asst["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a\"}",
        "arguments must be a JSON string"
    );
    assert_eq!(
        msgs[2]["tool_call_id"], asst["tool_calls"][0]["id"],
        "result id must match the call id"
    );
    assert_eq!(msgs[2]["content"], "hello");
}

#[tokio::test]
async fn openai_chat_decodes_reasoning_content_and_closes_parts_in_order() {
    let fixture = [
        r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"find "},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"slides"},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"content":"I'll look."},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"glob","arguments":"{}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ];
    let events = decode_all(&OpenAiChatProtocol, &fixture);
    // Text and reasoning persist at their end event, so each must end before
    // the next part starts or the stored message is reordered.
    assert_eq!(
        summarize(&events),
        vec![
            "reasoning_start",
            "reasoning_delta:find ",
            "reasoning_delta:slides",
            "reasoning_end",
            "text_start",
            "text_delta:I'll look.",
            "text_end",
            "tool_start:glob",
            "tool_input_delta",
            "tool_call:glob:{}",
            "finish:ToolCalls",
        ]
    );
    let provider_data = events.iter().find_map(|event| match event {
        Event::ReasoningEnd { provider_data, .. } => provider_data.clone(),
        _ => None,
    });
    assert_eq!(provider_data, Some(json!({"type": "reasoning_content"})));
}

fn chat_reasoning(text: &str) -> Part {
    Part::Reasoning {
        id: PartId::new(),
        text: text.into(),
        provider_data: Some(json!({"type": "reasoning_content"})),
    }
}

#[test]
fn openai_chat_replays_reasoning_content_on_every_assistant_message() {
    let mut req = assistant_tool_request(json!({"path": "a"}));
    // An earlier turn whose tool call carried no reasoning, then the current
    // turn with chat-native reasoning before its text and tool call.
    let earlier = req.messages[1].clone();
    req.messages.insert(1, earlier);
    req.messages.insert(
        2,
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "again".into(),
            }],
        },
    );
    let Message::Assistant { parts, .. } = &mut req.messages[3] else {
        panic!("assistant fixture");
    };
    parts.insert(0, chat_reasoning("think"));
    parts.push(chat_reasoning("wrap"));
    parts.push(Part::Text {
        id: PartId::new(),
        text: "done".into(),
    });

    let body = OpenAiChatProtocol.encode(&req).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        vec![
            "user",
            "assistant",
            "tool",
            "user",
            "assistant",
            "tool",
            "assistant"
        ]
    );
    assert_eq!(msgs[1]["reasoning_content"], "", "thinking transcripts pad");
    assert_eq!(msgs[4]["reasoning_content"], "think");
    assert_eq!(msgs[4]["content"], "ok");
    assert_eq!(msgs[4]["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(msgs[6]["reasoning_content"], "wrap");
    assert_eq!(msgs[6]["content"], "done");
}

#[test]
fn openai_chat_omits_reasoning_content_without_chat_native_reasoning() {
    let mut req = assistant_tool_request(json!({"path": "a"}));
    let Message::Assistant { parts, .. } = &mut req.messages[1] else {
        panic!("assistant fixture");
    };
    // Reasoning from another protocol (after a model switch) is not resent.
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: "think".into(),
            provider_data: Some(json!({"type":"thinking","signature":"sig"})),
        },
    );
    let body = OpenAiChatProtocol.encode(&req).unwrap();
    assert!(body["messages"][1].get("reasoning_content").is_none());
}

fn decode_all_google(protocol: &GoogleProtocol, lines: &[&str]) -> Vec<Event> {
    let s = SessionId::new();
    let m = MessageId::new();
    let mut decoder = protocol.decoder(s, m, None);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

fn decode_all_anthropic(protocol: &AnthropicMessagesProtocol, lines: &[&str]) -> Vec<Event> {
    let s = SessionId::new();
    let m = MessageId::new();
    let mut decoder = protocol.decoder(s, m, None);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

#[test]
fn anthropic_decodes_message_usage() {
    let fixture = [
        r#"{"type":"message_start","message":{"usage":{"input_tokens":13,"cache_creation_input_tokens":7,"cache_read_input_tokens":5}}}"#,
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        r#"{"type":"content_block_stop","index":0}"#,
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
        r#"{"type":"message_stop"}"#,
    ];

    let events = decode_all_anthropic(&AnthropicMessagesProtocol, &fixture);

    assert_eq!(
        finished_tokens(&events),
        // Anthropic `input_tokens` already excludes cache reads and writes;
        // `output_tokens` includes thinking but the split is not reported.
        Some(TokenUsage {
            input: 13,
            output: 4,
            reasoning: 0,
            cache_read: 5,
            cache_write: 7,
            reasoning_unknown: true,
        })
    );
}

#[tokio::test]
async fn google_decodes_streamed_text() {
    let protocol = GoogleProtocol;
    let fixture = [
        r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}],"role":"model"}}]}"#,
        r#"{"candidates":[{"content":{"parts":[{"text":"lo"}],"role":"model"}}]}"#,
        r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}]}"#,
    ];
    let events = decode_all_google(&protocol, &fixture);
    assert_eq!(
        summarize(&events),
        vec![
            "text_start",
            "text_delta:Hel",
            "text_delta:lo",
            "text_end",
            "finish:Stop"
        ]
    );
}

#[tokio::test]
async fn google_decodes_usage_metadata() {
    let protocol = GoogleProtocol;
    let fixture = [
        r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":17,"candidatesTokenCount":6,"thoughtsTokenCount":4,"cachedContentTokenCount":9}}"#,
    ];

    let events = decode_all_google(&protocol, &fixture);

    assert_eq!(
        finished_tokens(&events),
        // Normalized: `promptTokenCount` includes cached content, and
        // `candidatesTokenCount` excludes thoughts, so output adds them back.
        Some(TokenUsage {
            input: 8,
            output: 10,
            reasoning: 4,
            cache_read: 9,
            cache_write: 0,
            reasoning_unknown: false,
        })
    );
}

#[tokio::test]
async fn google_decodes_function_call() {
    let protocol = GoogleProtocol;
    let fixture = [
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"/tmp/a"}}}],"role":"model"},"finishReason":"STOP"}]}"#,
    ];
    let events = decode_all_google(&protocol, &fixture);
    assert_eq!(
        summarize(&events),
        vec![
            "tool_start:read",
            "tool_call:read:{\"path\":\"/tmp/a\"}",
            "finish:ToolCalls",
        ]
    );
}

#[test]
fn encoders_emit_reasoning_only_when_set() {
    let mut req = CompletionRequest {
        model: ModelRef::new("m"),
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: Some(ReasoningEffort::High),
        headers: Default::default(),
    };
    let a = AnthropicMessagesProtocol.encode(&req).unwrap();
    assert_eq!(a["thinking"]["type"], "enabled");
    assert_eq!(a["thinking"]["budget_tokens"], 16000);
    assert!(a["max_tokens"].as_u64().unwrap() > 16384);
    let o = OpenAiChatProtocol.encode(&req).unwrap();
    assert_eq!(o["reasoning_effort"], "high");
    let g = GoogleProtocol.encode(&req).unwrap();
    assert_eq!(
        g["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        16000
    );

    req.reasoning = None;
    assert!(
        AnthropicMessagesProtocol
            .encode(&req)
            .unwrap()
            .get("thinking")
            .is_none()
    );
    assert!(
        OpenAiChatProtocol
            .encode(&req)
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );
}

#[test]
fn google_encodes_system_user_and_tools() {
    let req = CompletionRequest {
        model: ModelRef::new("gemini-2.0-flash"),
        system: Some("be terse".to_string()),
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "hi".to_string(),
            }],
        }],
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".to_string(),
            input_schema: json!({"type":"object"}),
            output_schema: None,
        }],
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let body = GoogleProtocol.encode(&req).unwrap();
    assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be terse");
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
    assert_eq!(body["tools"][0]["functionDeclarations"][0]["name"], "read");
}

#[test]
fn google_encodes_audio_and_video_media_parts() {
    let req = CompletionRequest {
        model: ModelRef::new("gemini-2.0-flash"),
        system: None,
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![
                Part::Text {
                    id: PartId::new(),
                    text: "inspect these".to_string(),
                },
                Part::Media {
                    id: PartId::new(),
                    media_type: "audio/wav".to_string(),
                    data: "ZGF0YQ==".to_string(),
                    filename: Some("clip.wav".to_string()),
                },
                Part::Media {
                    id: PartId::new(),
                    media_type: "video/mp4".to_string(),
                    data: "ZGF0YQ==".to_string(),
                    filename: Some("clip.mp4".to_string()),
                },
            ],
        }],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let body = GoogleProtocol.encode(&req).unwrap();
    let parts = body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "inspect these");
    assert_eq!(parts[1]["inlineData"]["mimeType"], "audio/wav");
    assert_eq!(parts[1]["inlineData"]["data"], "ZGF0YQ==");
    assert_eq!(parts[2]["inlineData"]["mimeType"], "video/mp4");
    assert_eq!(parts[2]["inlineData"]["data"], "ZGF0YQ==");
}

#[test]
fn openai_null_tool_input_becomes_empty_object_string() {
    let body = OpenAiChatProtocol
        .encode(&assistant_tool_request(serde_json::Value::Null))
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], "{}");
}

#[test]
fn anthropic_null_tool_input_becomes_empty_object() {
    let body = AnthropicMessagesProtocol
        .encode(&assistant_tool_request(serde_json::Value::Null))
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
    assert_eq!(msgs[1]["content"][1]["input"], json!({}));
}

#[test]
fn anthropic_encodes_tool_use_and_result() {
    let body = AnthropicMessagesProtocol
        .encode(&assistant_tool_request(json!({ "path": "a" })))
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(
        msgs.len(),
        3,
        "user, assistant(tool_use), user(tool_result)"
    );
    let asst = &msgs[1];
    assert_eq!(asst["role"], "assistant");
    assert_eq!(asst["content"][0]["type"], "text");
    assert_eq!(asst["content"][1]["type"], "tool_use");
    assert_eq!(
        asst["content"][1]["input"]["path"], "a",
        "input must be an object"
    );
    let result = &msgs[2];
    assert_eq!(result["role"], "user");
    assert_eq!(result["content"][0]["type"], "tool_result");
    assert_eq!(result["content"][0]["is_error"], false);
    assert_eq!(
        result["content"][0]["tool_use_id"], asst["content"][1]["id"],
        "tool_result must reference the tool_use id"
    );
    assert_eq!(result["content"][0]["content"], "hello");
}

#[test]
fn anthropic_replays_signed_reasoning_before_tool_use() {
    let mut req = assistant_tool_request(json!({"path":"a"}));
    let Message::Assistant { parts, .. } = &mut req.messages[1] else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: "think".into(),
            provider_data: Some(json!({"type":"thinking","signature":"sig"})),
        },
    );
    let body = AnthropicMessagesProtocol.encode(&req).unwrap();
    let content = &body["messages"][1]["content"];
    assert_eq!(
        content[0],
        json!({"type":"thinking","thinking":"think","signature":"sig"})
    );
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[2]["type"], "tool_use");
}

#[test]
fn anthropic_replay_omits_old_and_unsigned_reasoning_and_replays_redacted() {
    let mut req = assistant_tool_request(json!({}));
    let old = req.messages[1].clone();
    req.messages.insert(1, old);
    req.messages.insert(
        2,
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "new turn".into(),
            }],
        },
    );
    for index in [1, 3] {
        let Message::Assistant { parts, .. } = &mut req.messages[index] else {
            panic!("assistant fixture");
        };
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "hidden".into(),
                provider_data: None,
            },
        );
    }
    let Message::Assistant { parts, .. } = &mut req.messages[3] else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: String::new(),
            provider_data: Some(json!({"type":"redacted_thinking","data":"opaque"})),
        },
    );
    let body = AnthropicMessagesProtocol.encode(&req).unwrap();
    let blocks: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .collect();
    assert_eq!(
        blocks
            .iter()
            .filter(|b| b["type"] == "redacted_thinking")
            .count(),
        1
    );
    assert_eq!(blocks.iter().filter(|b| b["type"] == "thinking").count(), 0);
}

#[test]
fn anthropic_legacy_final_tool_text_ends_with_tool_results() {
    let mut req = assistant_tool_request(json!({}));
    let Message::Assistant { parts, .. } = &mut req.messages[1] else {
        panic!("assistant fixture");
    };
    parts.swap(0, 1);
    let messages = AnthropicMessagesProtocol.encode(&req).unwrap()["messages"].clone();
    assert_eq!(messages.as_array().unwrap().len(), 3);
    assert_eq!(messages[1]["content"][0]["type"], "text");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[2]["role"], "user");
}

#[test]
fn anthropic_replays_two_reasoning_tool_steps_in_order() {
    let mut req = assistant_tool_request(json!({"step": 1}));
    let mut second = req.messages[1].clone();
    let Message::Assistant { parts, .. } = &mut second else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: "second".into(),
            provider_data: Some(json!({"type":"thinking","signature":"s2"})),
        },
    );
    req.messages.push(second);
    let Message::Assistant { parts, .. } = &mut req.messages[1] else {
        panic!("assistant fixture");
    };
    parts.insert(
        0,
        Part::Reasoning {
            id: PartId::new(),
            text: "first".into(),
            provider_data: Some(json!({"type":"thinking","signature":"s1"})),
        },
    );
    let messages = AnthropicMessagesProtocol.encode(&req).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        messages.iter().filter(|m| m["role"] == "assistant").count(),
        2
    );
    assert_eq!(messages[1]["content"][0]["signature"], "s1");
    assert_eq!(messages[3]["content"][0]["signature"], "s2");
}

/// A compaction summary is injected as a mid-conversation `Message::System`
/// carrying `HYA_COMPACTED_CONTEXT`, and the turn loop slices the transcript to
/// start at that marker. If the encoder drops the marker message, Anthropic
/// receives neither the folded history nor the summary that replaced it.
#[test]
fn anthropic_preserves_mid_conversation_system_message() {
    let req = CompletionRequest {
        model: ModelRef::new("claude-sonnet-4"),
        system: Some("cached agent prefix".to_string()),
        messages: vec![
            Message::System {
                id: MessageId::new(),
                content: "HYA_COMPACTED_CONTEXT\nearlier work: edited lib.rs".to_string(),
            },
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "carry on".to_string(),
                }],
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let body = AnthropicMessagesProtocol.encode(&req).unwrap();
    assert_eq!(
        body["system"], "cached agent prefix",
        "the summary must not be folded into the cached system prefix"
    );
    let encoded = body["messages"].to_string();
    assert!(
        encoded.contains("HYA_COMPACTED_CONTEXT"),
        "compaction marker must survive encoding, got {encoded}"
    );
    assert!(
        encoded.contains("earlier work: edited lib.rs"),
        "summary body must survive encoding, got {encoded}"
    );
}

fn image_request() -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new("image-model"),
        system: None,
        messages: vec![Message::User {
            id: MessageId::new(),
            parts: vec![
                Part::Text {
                    id: PartId::new(),
                    text: "inspect".to_string(),
                },
                Part::Media {
                    id: PartId::new(),
                    media_type: "image/png".to_string(),
                    data: "data:image/png;base64,aGVsbG8=".to_string(),
                    filename: Some("pixel.png".to_string()),
                },
            ],
        }],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

#[test]
fn openai_responses_encodes_ordered_image_content() {
    let body = OpenAiResponsesProtocol.encode(&image_request()).unwrap();
    assert_eq!(
        body["input"][0]["content"],
        json!([
            {"type": "input_text", "text": "inspect"},
            {"type": "input_image", "image_url": "data:image/png;base64,aGVsbG8="},
        ])
    );
}

#[test]
fn openai_responses_encodes_raw_jpeg_bytes_without_path_rejection() {
    let raw_jpeg = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIQAxAAAAH/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/9oACAEBAAEFAqf/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAEDAQE/AT//xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oACAECAQE/AT//2gAMAwEAAgADAAAAEP/EABQQAQAAAAAAAAAAAAAAAAAAABD/2gAIAQMBAT8Qf//EABQRAQAAAAAAAAAAAAAAAAAAABD/2gAIAQIBAT8Qf//Z";
    let mut request = image_request();
    if let Message::User { parts, .. } = &mut request.messages[0] {
        parts[1] = Part::Media {
            id: PartId::new(),
            media_type: "image/jpeg".to_string(),
            data: raw_jpeg.to_string(),
            filename: Some("pixel.jpg".to_string()),
        };
    }
    let body = OpenAiResponsesProtocol.encode(&request).unwrap();
    assert_eq!(
        body["input"][0]["content"][1]["image_url"],
        format!("data:image/jpeg;base64,{raw_jpeg}")
    );
}

#[test]
fn openai_chat_encodes_ordered_image_content() {
    let body = OpenAiChatProtocol.encode(&image_request()).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}},
        ])
    );
}

#[test]
fn anthropic_encodes_ordered_image_content() {
    let body = AnthropicMessagesProtocol.encode(&image_request()).unwrap();
    assert_eq!(
        body["messages"][0]["content"],
        json!([
            {"type": "text", "text": "inspect"},
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "aGVsbG8="
                }
            },
        ])
    );
}

#[test]
fn openai_image_encoders_reject_unsupported_media_without_dropping_it() {
    let mut request = image_request();
    if let Message::User { parts, .. } = &mut request.messages[0] {
        parts[1] = Part::Media {
            id: PartId::new(),
            media_type: "audio/wav".to_string(),
            data: "ZGF0YQ==".to_string(),
            filename: None,
        };
    }
    assert!(OpenAiResponsesProtocol.encode(&request).is_err());
    assert!(OpenAiChatProtocol.encode(&request).is_err());
}

fn decode_with(protocol: &dyn Protocol, lines: &[&str]) -> Vec<Event> {
    let mut decoder = protocol.decoder(SessionId::new(), MessageId::new(), None);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

/// The requested call's input and the round's finish.
fn requested_call(events: &[Event]) -> (serde_json::Value, FinishReason) {
    let input = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallRequested { input, .. } => Some(input.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no tool call in {:?}", summarize(events)));
    let finish = events
        .iter()
        .find_map(|event| match event {
            Event::MessageFinished { finish, .. } => Some(*finish),
            _ => None,
        })
        .unwrap();
    (input, finish)
}

const CUT_ARGS: &str = r#"{"command":"ec"#;
const CUT_ERROR: &str = "EOF while parsing a string at line 1 column 14";
const TRAILING_ARGS: &str = r#"{"ops":[]}}"#;
const TRAILING_ERROR: &str = "trailing characters at line 1 column 11";

/// Each streaming decoder records a call whose argument text is not valid
/// JSON (cut off at the output limit, or followed by extra text) as a tool
/// call carrying the parser error, not as a decode error.
#[test]
fn decoders_record_a_call_with_invalid_argument_json() {
    let chat = |args: &str, finish: &str| {
        let args = serde_json::to_string(args).unwrap();
        decode_with(
            &OpenAiChatProtocol,
            &[
                &format!(
                    r#"{{"choices":[{{"delta":{{"tool_calls":[{{"index":0,"id":"call_1","function":{{"name":"bash","arguments":{args}}}}}]}},"finish_reason":null}}]}}"#
                ),
                &format!(r#"{{"choices":[{{"delta":{{}},"finish_reason":"{finish}"}}]}}"#),
                "[DONE]",
            ],
        )
    };
    let anthropic = |args: &str, stop: &str| {
        let args = serde_json::to_string(args).unwrap();
        decode_with(
            &AnthropicMessagesProtocol,
            &[
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash","input":{}}}"#,
                &format!(
                    r#"{{"type":"content_block_delta","index":0,"delta":{{"type":"input_json_delta","partial_json":{args}}}}}"#
                ),
                r#"{"type":"content_block_stop","index":0}"#,
                &format!(r#"{{"type":"message_delta","delta":{{"stop_reason":"{stop}"}}}}"#),
                r#"{"type":"message_stop"}"#,
            ],
        )
    };
    // Cut: the response ends before the item is done. Trailing: the done
    // item carries the arguments.
    let responses = |args: &str, done: bool| {
        let args = serde_json::to_string(args).unwrap();
        let last = if done {
            format!(
                r#"{{"type":"response.output_item.done","output_index":0,"item":{{"type":"function_call","call_id":"call_1","name":"bash","arguments":{args}}}}}"#
            )
        } else {
            format!(
                r#"{{"type":"response.function_call_arguments.delta","output_index":0,"delta":{args}}}"#
            )
        };
        let terminal = if done {
            "response.completed"
        } else {
            "response.incomplete"
        };
        decode_with(
            &OpenAiResponsesProtocol,
            &[
                r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"call_1","name":"bash","arguments":""}}"#,
                &last,
                &format!(r#"{{"type":"{terminal}","response":{{}}}}"#),
            ],
        )
    };
    let cases = [
        (
            "chat cut",
            chat(CUT_ARGS, "length"),
            CUT_ERROR,
            FinishReason::Length,
        ),
        (
            "chat trailing",
            chat(TRAILING_ARGS, "tool_calls"),
            TRAILING_ERROR,
            FinishReason::ToolCalls,
        ),
        (
            "anthropic cut",
            anthropic(CUT_ARGS, "max_tokens"),
            CUT_ERROR,
            FinishReason::Length,
        ),
        (
            "anthropic trailing",
            anthropic(TRAILING_ARGS, "tool_use"),
            TRAILING_ERROR,
            FinishReason::ToolCalls,
        ),
        (
            "responses cut",
            responses(CUT_ARGS, false),
            CUT_ERROR,
            FinishReason::Length,
        ),
        (
            "responses trailing",
            responses(TRAILING_ARGS, true),
            TRAILING_ERROR,
            FinishReason::ToolCalls,
        ),
    ];
    for (case, events, error, finish) in cases {
        let (input, reported) = requested_call(&events);
        assert_eq!(
            hya_provider::invalid_tool_arguments(&input),
            Some(error),
            "{case}: {input}"
        );
        assert_eq!(reported, finish, "{case}");
    }
}

/// The recorded input of such a call replays as an object on every route.
#[test]
fn a_call_with_invalid_argument_json_replays_on_every_route() {
    let (input, _) = requested_call(&decode_all(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":"length"}]}"#,
        ],
    ));
    let mut request = assistant_tool_request(input.clone());
    let Message::Assistant { parts, .. } = &mut request.messages[1] else {
        panic!("assistant fixture");
    };
    let Part::Tool { state, .. } = &mut parts[1] else {
        panic!("tool fixture");
    };
    *state = ToolPartState::Error {
        input: input.clone(),
        message: "arguments are not valid JSON".to_string(),
        value: None,
    };
    let as_object = |arguments: &serde_json::Value| {
        serde_json::from_str::<serde_json::Value>(arguments.as_str().unwrap()).unwrap()
    };

    let chat = OpenAiChatProtocol.encode(&request).unwrap();
    assert_eq!(
        as_object(&chat["messages"][1]["tool_calls"][0]["function"]["arguments"]),
        input
    );
    let responses = OpenAiResponsesProtocol.encode(&request).unwrap();
    let call = responses["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(as_object(&call["arguments"]), input);
    let anthropic = AnthropicMessagesProtocol.encode(&request).unwrap();
    assert_eq!(anthropic["messages"][1]["content"][1]["input"], input);
    let google = GoogleProtocol.encode(&request).unwrap();
    assert_eq!(
        google["contents"][1]["parts"][1]["functionCall"]["args"],
        input
    );
    assert!(input.is_object());
}

#[test]
fn anthropic_preserves_upstream_tool_id_on_same_model_and_strips_across_models() {
    let events = decode_all_anthropic(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01ABC","name":"read"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let (part, call_id, provider_data) = events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallRequested {
                part,
                call,
                provider_data,
                ..
            } => Some((*part, *call, provider_data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(provider_data, Some(json!({"call_id": "toolu_01ABC"})));

    let make_req = |served_model: &str, target_model: &str| CompletionRequest {
        model: ModelRef::new(target_model),
        system: None,
        messages: vec![
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "read a".into(),
                }],
            },
            Message::Assistant {
                id: MessageId::new(),
                agent: AgentName::new("build"),
                model: ModelRef::new(served_model),
                parts: vec![Part::Tool {
                    id: part,
                    call_id,
                    name: ToolName::new("read"),
                    state: ToolPartState::Completed {
                        input: json!({"path": "a"}),
                        output: json!("hello"),
                        time_ms: 1,
                    },
                    provider_data: provider_data.clone(),
                }],
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };

    let same = AnthropicMessagesProtocol
        .encode(&make_req(
            "claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5#high",
        ))
        .unwrap();
    assert_eq!(same["messages"][1]["content"][0]["id"], "toolu_01ABC");
    assert_eq!(
        same["messages"][2]["content"][0]["tool_use_id"],
        "toolu_01ABC"
    );

    let cross = AnthropicMessagesProtocol
        .encode(&make_req("claude-sonnet-4-5", "claude-opus-4-6"))
        .unwrap();
    assert_eq!(
        cross["messages"][1]["content"][0]["id"],
        call_id.to_string()
    );
    assert_eq!(
        cross["messages"][2]["content"][0]["tool_use_id"],
        call_id.to_string()
    );
}

#[test]
fn openai_chat_and_responses_preserve_upstream_call_id_and_strip_foreign_state() {
    // OpenAI Chat captures upstream `id` and `extra_content.google.thought_signature`.
    let chat_events = decode_all(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_chat_1","extra_content":{"google":{"thought_signature":"sig_xyz"}},"function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    let (chat_call, chat_data) = chat_events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallRequested {
                call,
                provider_data,
                ..
            } => Some((*call, provider_data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        chat_data,
        Some(json!({"call_id": "call_chat_1", "google_thought_signature": "sig_xyz"}))
    );

    let mut chat_req = assistant_tool_request(json!({}));
    chat_req.model = ModelRef::new("google/gemini-2.5-pro");
    if let Message::Assistant { model, parts, .. } = &mut chat_req.messages[1] {
        *model = ModelRef::new("gemini-2.5-pro");
        if let Part::Tool {
            call_id,
            provider_data,
            ..
        } = &mut parts[1]
        {
            *call_id = chat_call;
            *provider_data = chat_data.clone();
        }
    }
    let chat_same = OpenAiChatProtocol.encode(&chat_req).unwrap();
    assert_eq!(
        chat_same["messages"][1]["tool_calls"][0]["id"],
        "call_chat_1"
    );
    assert_eq!(
        chat_same["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "sig_xyz"
    );
    assert_eq!(chat_same["messages"][2]["tool_call_id"], "call_chat_1");

    chat_req.model = ModelRef::new("openai/gpt-5");
    let chat_cross = OpenAiChatProtocol.encode(&chat_req).unwrap();
    assert_eq!(
        chat_cross["messages"][1]["tool_calls"][0]["id"],
        chat_call.to_string()
    );
    assert!(
        chat_cross["messages"][1]["tool_calls"][0]
            .get("extra_content")
            .is_none()
    );

    // OpenAI Responses captures upstream `call_id` (not `fc_` id), replays same-model
    // reasoning items, and strips foreign or cross-model reasoning and call IDs.
    let resp_events = decode_with(
        &OpenAiResponsesProtocol,
        &[
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_999","call_id":"call_resp_1","name":"read","arguments":"{}"}}"#,
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_999","call_id":"call_resp_1","name":"read","arguments":"{}"}}"#,
            r#"{"type":"response.completed","response":{"status":"completed"}}"#,
        ],
    );
    let (resp_call, resp_data) = resp_events
        .iter()
        .find_map(|event| match event {
            Event::ToolCallRequested {
                call,
                provider_data,
                ..
            } => Some((*call, provider_data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(resp_data, Some(json!({"call_id": "call_resp_1"})));

    let make_resp_req =
        |served_model: &str, target_model: &str, reasoning_data: serde_json::Value| {
            CompletionRequest {
                model: ModelRef::new(target_model),
                system: None,
                messages: vec![Message::Assistant {
                    id: MessageId::new(),
                    agent: AgentName::new("build"),
                    model: ModelRef::new(served_model),
                    parts: vec![
                        Part::Reasoning {
                            id: PartId::new(),
                            text: "thought".into(),
                            provider_data: Some(reasoning_data),
                        },
                        Part::Tool {
                            id: PartId::new(),
                            call_id: resp_call,
                            name: ToolName::new("read"),
                            state: ToolPartState::Completed {
                                input: json!({}),
                                output: json!("ok"),
                                time_ms: 1,
                            },
                            provider_data: resp_data.clone(),
                        },
                    ],
                    finish: None,
                    tokens: None,
                    last_round: None,
                    last_step_start: None,
                }],
                tools: Vec::new(),
                temperature: None,
                max_output_tokens: None,
                reasoning: None,
                headers: Default::default(),
            }
        };

    let native_item = json!({"id": "rs_1", "type": "reasoning", "encrypted_content": "enc"});
    let resp_same = OpenAiResponsesProtocol
        .encode(&make_resp_req(
            "gpt-5.6-sol",
            "openai/gpt-5.6-sol#high",
            native_item.clone(),
        ))
        .unwrap();
    assert_eq!(resp_same["input"][0], native_item);
    assert_eq!(resp_same["input"][1]["call_id"], "call_resp_1");
    assert!(resp_same["input"][1].get("id").is_none());
    assert_eq!(resp_same["input"][2]["call_id"], "call_resp_1");

    let resp_cross = OpenAiResponsesProtocol
        .encode(&make_resp_req("gpt-5.6-sol", "gpt-6-astra", native_item))
        .unwrap();
    assert!(
        resp_cross["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("type").and_then(|v| v.as_str()) != Some("reasoning"))
    );
    assert_eq!(resp_cross["input"][0]["call_id"], resp_call.to_string());

    let resp_foreign = OpenAiResponsesProtocol
        .encode(&make_resp_req(
            "gpt-5.6-sol",
            "gpt-5.6-sol",
            json!({"type": "thinking", "signature": "sig"}),
        ))
        .unwrap();
    assert!(
        resp_foreign["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item.get("type").and_then(|v| v.as_str()) != Some("thinking"))
    );
}
