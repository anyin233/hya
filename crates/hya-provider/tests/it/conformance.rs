//! Cross-provider conformance of routing, decoding, encoding, reasoning replay,
//! media handling, and structured/invalid tool call contracts.

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
    ProviderError, ProviderRouter, ReasoningEffort, preflight,
};
use serde_json::{Value, json};

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

fn decode_with(protocol: &dyn Protocol, lines: &[&str]) -> Vec<Event> {
    let mut decoder = protocol.decoder(SessionId::new(), MessageId::new(), None);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

fn finished_tokens(events: &[Event]) -> Option<TokenUsage> {
    events.iter().find_map(|event| match event {
        Event::MessageFinished { tokens, .. } => *tokens,
        _ => None,
    })
}

fn assistant_tool_request(input: Value) -> CompletionRequest {
    CompletionRequest {
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
        ..req("m")
    }
}

fn chat_reasoning(text: &str) -> Part {
    Part::Reasoning {
        id: PartId::new(),
        text: text.into(),
        provider_data: Some(json!({"type": "reasoning_content"})),
    }
}

fn requested_call(events: &[Event]) -> (Value, FinishReason) {
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

#[tokio::test]
async fn router_and_fake_provider_routing_capabilities_preflight_and_no_failover() {
    // 1. FakeProvider round-trips canonical events and materializes reasoning/tool/usage.
    let fake_router = ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![
        FakeStep::Text("Hello world".to_string()),
        FakeStep::Finish(FinishReason::Stop),
    ])));
    let events: Vec<Event> = fake_router
        .stream(req("fake"), SessionId::new(), MessageId::new())
        .await
        .unwrap()
        .map(Result::unwrap)
        .collect()
        .await;
    assert_eq!(
        summarize(&events),
        [
            "text_start",
            "text_delta:Hello world",
            "text_end",
            "finish:Stop"
        ]
    );

    let materialized = FakeProvider::materialize(
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
        summarize(&materialized),
        [
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
        finished_tokens(&materialized),
        Some(TokenUsage {
            input: 1,
            output: 2,
            reasoning: 3,
            cache_read: 0,
            cache_write: 0,
            reasoning_unknown: false,
        })
    );

    // 2. Empty router returns UnknownModel.
    assert!(matches!(
        ProviderRouter::new()
            .stream(req("locally-unconfigured"), SessionId::new(), MessageId::new())
            .await,
        Err(ProviderError::UnknownModel(m)) if m == "locally-unconfigured"
    ));

    // 3. Router clears reasoning when provider lacks reasoning_request capability.
    let rec = Arc::new(RecordingProvider::new(Capabilities {
        reasoning_request: false,
        ..Capabilities::default()
    }));
    let mut with_reasoning = req("recording");
    with_reasoning.reasoning = Some(ReasoningEffort::High);
    let _ = ProviderRouter::new()
        .with(rec.clone())
        .stream(with_reasoning, SessionId::new(), MessageId::new())
        .await
        .unwrap();
    assert_eq!(
        rec.recorded.lock().unwrap().as_ref().unwrap().reasoning,
        None
    );

    // 4. Router never fails over (retryable pre-stream, non-retryable pre-stream, or in-stream).
    for (fail_after, retryable) in [(false, true), (false, false), (true, true)] {
        let primary = Arc::new(FailingProvider {
            calls: AtomicUsize::new(0),
            fail_after_stream_start: fail_after,
            retryable,
        });
        let backup = Arc::new(RecordingProvider::new(Capabilities::default()));
        let router = ProviderRouter::new()
            .with(primary.clone())
            .with(backup.clone());
        let res = router
            .stream(req("shared-model"), SessionId::new(), MessageId::new())
            .await;
        if fail_after {
            let events: Vec<_> = res.unwrap().collect().await;
            assert!(matches!(
                events.as_slice(),
                [Err(ProviderError::Transport(_))]
            ));
        } else if retryable {
            assert!(matches!(res, Err(ProviderError::Transport(_))));
        } else {
            assert!(matches!(
                res,
                Err(ProviderError::HttpStatus { status: 400, .. })
            ));
        }
        assert_eq!(primary.calls.load(Ordering::SeqCst), 1);
        assert!(backup.recorded.lock().unwrap().is_none());
    }

    // 5. Preflight rejects tools on incapable routes only when tools are present.
    let mut with_tools = req("m");
    with_tools.tools.push(ToolSchema {
        name: ToolName::new("read"),
        description: "read".to_string(),
        input_schema: json!({"type": "object"}),
        output_schema: None,
    });
    let incapable = Capabilities::default();
    let capable = Capabilities {
        streaming_tool_calls: true,
        ..Capabilities::default()
    };
    assert!(preflight(&incapable, &with_tools).is_err());
    assert!(preflight(&capable, &with_tools).is_ok());
    assert!(preflight(&incapable, &req("m")).is_ok());
}

#[test]
fn openai_chat_meets_the_canonical_decoding_contract() {
    hya_provider::test_support::assert_canonical_text_tool_usage(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hello "},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"world"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ],
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":\"/tmp/a\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ],
        &[
            r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
            "[DONE]",
        ],
        TokenUsage {
            input: 6,
            output: 3,
            reasoning: 2,
            cache_read: 5,
            cache_write: 0,
            reasoning_unknown: false,
        },
    );
}

#[test]
fn all_protocols_agree_on_canonical_text_tool_and_usage_decoding() {
    // Canonical text shape agreement across OpenAI Chat, Anthropic, Google, and Fake.
    let openai_text = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"role":"assistant","content":""},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"Hel"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"content":"lo"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ],
    );
    let google_text = decode_with(
        &GoogleProtocol,
        &[
            r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"lo"}],"role":"model"}}]}"#,
            r#"{"candidates":[{"content":{"parts":[]},"finishReason":"STOP"}]}"#,
        ],
    );
    assert_eq!(
        summarize(&openai_text),
        [
            "text_start",
            "text_delta:Hel",
            "text_delta:lo",
            "text_end",
            "finish:Stop"
        ]
    );
    assert_eq!(summarize(&google_text), summarize(&openai_text));

    let anthropic_hw = decode_with(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"message_start","message":{}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let openai_hw = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hello world"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
        ],
    );
    let fake_hw = FakeProvider::materialize(
        &[
            FakeStep::Text("Hello world".to_string()),
            FakeStep::Finish(FinishReason::Stop),
        ],
        SessionId::new(),
        MessageId::new(),
    );
    assert_eq!(summarize(&anthropic_hw), summarize(&openai_hw));
    assert_eq!(summarize(&openai_hw), summarize(&fake_hw));

    // Streamed tool calls across OpenAI Chat, Anthropic, and Google.
    let openai_tool = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"/tmp/a\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
        ],
    );
    let anthropic_tool = decode_with(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"read"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":":\"/tmp/a\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let expected_streamed_tool = [
        "tool_start:read",
        "tool_input_delta",
        "tool_input_delta",
        "tool_call:read:{\"path\":\"/tmp/a\"}",
        "finish:ToolCalls",
    ];
    assert_eq!(summarize(&openai_tool), expected_streamed_tool);
    assert_eq!(summarize(&anthropic_tool), expected_streamed_tool);

    let google_tool = decode_with(
        &GoogleProtocol,
        &[
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","args":{"path":"/tmp/a"}}}],"role":"model"},"finishReason":"STOP"}]}"#,
        ],
    );
    assert_eq!(
        summarize(&google_tool),
        [
            "tool_start:read",
            "tool_call:read:{\"path\":\"/tmp/a\"}",
            "finish:ToolCalls"
        ]
    );

    // Usage normalization across OpenAI (cached/reasoning, unknown reasoning, underflow clamp),
    // Anthropic (cache read/write, reasoning_unknown), and Google (cached + thoughts).
    let openai_usage = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":5},"completion_tokens_details":{"reasoning_tokens":2}}}"#,
            "[DONE]",
        ],
    );
    assert_eq!(
        finished_tokens(&openai_usage),
        Some(TokenUsage {
            input: 6,
            output: 3,
            reasoning: 2,
            cache_read: 5,
            cache_write: 0,
            reasoning_unknown: false,
        })
    );

    let openai_unknown = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":4,"prompt_tokens_details":{"cached_tokens":8,"cache_creation_tokens":2}}}"#,
            "[DONE]",
        ],
    );
    assert_eq!(
        finished_tokens(&openai_unknown),
        Some(TokenUsage {
            input: 10,
            output: 4,
            reasoning: 0,
            cache_read: 8,
            cache_write: 2,
            reasoning_unknown: true,
        })
    );

    let openai_underflow = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"content":"Hi"},"finish_reason":"stop"}]}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":1,"prompt_tokens_details":{"cached_tokens":9},"completion_tokens_details":{"reasoning_tokens":0}}}"#,
            "[DONE]",
        ],
    );
    let clamped = finished_tokens(&openai_underflow).unwrap();
    assert_eq!((clamped.input, clamped.cache_read), (0, 9));

    let anthropic_usage = decode_with(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"message_start","message":{"usage":{"input_tokens":13,"cache_creation_input_tokens":7,"cache_read_input_tokens":5}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    assert_eq!(
        finished_tokens(&anthropic_usage),
        Some(TokenUsage {
            input: 13,
            output: 4,
            reasoning: 0,
            cache_read: 5,
            cache_write: 7,
            reasoning_unknown: true,
        })
    );

    let google_usage = decode_with(
        &GoogleProtocol,
        &[
            r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}],"role":"model"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":17,"candidatesTokenCount":6,"thoughtsTokenCount":4,"cachedContentTokenCount":9}}"#,
        ],
    );
    assert_eq!(
        finished_tokens(&google_usage),
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

#[test]
fn openai_chat_reasoning_content_decoding_replay_and_deepseek_empty_padding() {
    // 1. Decodes reasoning_content -> closes before text -> closes before tool_calls.
    let events = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"Check "},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"the repo","content":"I'll look."},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    assert_eq!(
        summarize(&events),
        [
            "reasoning_start",
            "reasoning_delta:Check ",
            "reasoning_delta:the repo",
            "reasoning_end",
            "text_start",
            "text_delta:I'll look.",
            "text_end",
            "tool_start:read",
            "tool_input_delta",
            "tool_call:read:{\"path\":\"a\"}",
            "finish:ToolCalls",
        ]
    );
    let provider_data = events.iter().find_map(|e| match e {
        Event::ReasoningEnd { provider_data, .. } => provider_data.clone(),
        _ => None,
    });
    assert_eq!(provider_data, Some(json!({"type": "reasoning_content"})));

    // 2. Replays streamed reasoning_content, joins multiple parts with \n\n, and pads earlier turns.
    let mut multi = assistant_tool_request(json!({"path": "a"}));
    let earlier = multi.messages[1].clone();
    multi.messages.insert(1, earlier);
    multi.messages.insert(
        2,
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "again".into(),
            }],
        },
    );
    if let Message::Assistant { parts, .. } = &mut multi.messages[3] {
        parts.splice(
            0..0,
            [
                chat_reasoning("first thought"),
                chat_reasoning(""),
                chat_reasoning("next thought"),
            ],
        );
        parts.push(chat_reasoning("wrap"));
        parts.push(Part::Text {
            id: PartId::new(),
            text: "done".into(),
        });
    }
    let body = OpenAiChatProtocol.encode(&multi).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    let roles: Vec<_> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(
        roles,
        [
            "user",
            "assistant",
            "tool",
            "user",
            "assistant",
            "tool",
            "assistant"
        ]
    );
    assert_eq!(msgs[1]["reasoning_content"], "");
    assert_eq!(
        msgs[4]["reasoning_content"],
        "first thought\n\nnext thought"
    );
    assert_eq!(msgs[4]["content"], "ok");
    assert_eq!(msgs[4]["tool_calls"][0]["function"]["name"], "read");
    assert_eq!(msgs[6]["reasoning_content"], "wrap");
    assert_eq!(msgs[6]["content"], "done");

    // 3. DeepSeek replays empty reasoning_content for tool turns without explicit reasoning.
    let mut deepseek = assistant_tool_request(json!({"command": "pwd"}));
    deepseek.model = ModelRef::new("deepseek-flash");
    let ds_body = OpenAiChatProtocol.encode(&deepseek).unwrap();
    assert_eq!(ds_body["messages"][1]["reasoning_content"], "");

    // 4. Historical marker (`openai_chat_reasoning_content: true`) and reasoning-only assistant survive.
    let mut hist = assistant_tool_request(json!({"path": "a"}));
    hist.model = ModelRef::new("kimi-thinking");
    if let Message::Assistant { parts, .. } = &mut hist.messages[1] {
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "old thought".into(),
                provider_data: Some(json!({"openai_chat_reasoning_content": true})),
            },
        );
    }
    assert_eq!(
        OpenAiChatProtocol.encode(&hist).unwrap()["messages"][1]["reasoning_content"],
        "old thought"
    );

    let mut only_r = assistant_tool_request(json!({"path": "a"}));
    only_r.model = ModelRef::new("kimi-thinking");
    if let Message::Assistant { parts, .. } = &mut only_r.messages[1] {
        *parts = vec![chat_reasoning("thinking")];
    }
    let only_body = OpenAiChatProtocol.encode(&only_r).unwrap();
    assert_eq!(only_body["messages"][1]["reasoning_content"], "thinking");
    assert_eq!(only_body["messages"][1]["content"], "");

    // 5. Foreign protocol reasoning (Anthropic thinking) is not resent as reasoning_content.
    let mut foreign = assistant_tool_request(json!({"path": "a"}));
    if let Message::Assistant { parts, .. } = &mut foreign.messages[1] {
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "think".into(),
                provider_data: Some(json!({"type": "thinking", "signature": "sig"})),
            },
        );
    }
    assert!(
        OpenAiChatProtocol.encode(&foreign).unwrap()["messages"][1]
            .get("reasoning_content")
            .is_none()
    );
}

#[test]
fn openai_chat_and_responses_upstream_call_id_and_foreign_state_stripping() {
    // OpenAI Chat captures upstream `id` and `extra_content.google.thought_signature`.
    let chat_events = decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_chat_1","extra_content":{"google":{"thought_signature":"sig_xyz"}},"function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}"#,
            "[DONE]",
        ],
    );
    let (chat_call, chat_data) = chat_events
        .iter()
        .find_map(|e| match e {
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

    // OpenAI Responses preserves upstream `call_id` (not `fc_` id), replays same-model reasoning,
    // and strips cross-model or foreign reasoning & call IDs.
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
        .find_map(|e| match e {
            Event::ToolCallRequested {
                call,
                provider_data,
                ..
            } => Some((*call, provider_data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(resp_data, Some(json!({"call_id": "call_resp_1"})));

    let make_resp_req = |served: &str, target: &str, reasoning_data: Value| CompletionRequest {
        messages: vec![Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new(served),
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
        ..req(target)
    };

    let native_item = json!({"id": "rs_1", "type": "reasoning", "encrypted_content": "enc"});
    let same = OpenAiResponsesProtocol
        .encode(&make_resp_req(
            "gpt-5.6-sol",
            "openai/gpt-5.6-sol#high",
            native_item.clone(),
        ))
        .unwrap();
    assert_eq!(same["input"][0], native_item);
    assert_eq!(same["input"][1]["call_id"], "call_resp_1");
    assert!(same["input"][1].get("id").is_none());
    assert_eq!(same["input"][2]["call_id"], "call_resp_1");

    let cross = OpenAiResponsesProtocol
        .encode(&make_resp_req("gpt-5.6-sol", "gpt-6-astra", native_item))
        .unwrap();
    assert!(
        cross["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("type").and_then(|v| v.as_str()) != Some("reasoning"))
    );
    assert_eq!(cross["input"][0]["call_id"], resp_call.to_string());

    let foreign = OpenAiResponsesProtocol
        .encode(&make_resp_req(
            "gpt-5.6-sol",
            "gpt-5.6-sol",
            json!({"type": "thinking", "signature": "sig"}),
        ))
        .unwrap();
    assert!(
        foreign["input"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("type").and_then(|v| v.as_str()) != Some("thinking"))
    );
}

#[test]
fn anthropic_thinking_signature_deltas_redacted_and_multi_step_replay() {
    // 1. Decodes thinking + concatenated signature_delta + redacted_thinking + ignores input_transformations.
    let decoded = decode_with(
        &AnthropicMessagesProtocol,
        &[
            concat!(
                "{\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"model\":\"claude-opus-4-7\",",
                "\"usage\":{\"input_tokens\":10,\"output_tokens\":1},",
                "\"input_transformations\":[{\"type\":\"thinking_block_binding\",\"action\":\"removed\",\"reason\":\"prefix_mismatch\"}]}}",
            ),
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_1"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"_2"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"42"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    assert_eq!(
        summarize(&decoded),
        [
            "reasoning_start",
            "reasoning_delta:plan",
            "reasoning_end",
            "reasoning_start",
            "reasoning_end",
            "text_start",
            "text_delta:42",
            "text_end",
            "finish:Stop",
        ]
    );
    let rdata: Vec<_> = decoded
        .iter()
        .filter_map(|e| match e {
            Event::ReasoningEnd { provider_data, .. } => provider_data.as_ref(),
            _ => None,
        })
        .collect();
    assert_eq!(
        rdata,
        vec![
            &json!({"type": "thinking", "signature": "sig_1_2"}),
            &json!({"type": "redacted_thinking", "data": "opaque"}),
        ]
    );

    // 2. Replays two signed reasoning+tool steps in order, omits unsigned reasoning, keeps redacted.
    let mut steps = assistant_tool_request(json!({"step": 1}));
    let mut second = steps.messages[1].clone();
    if let Message::Assistant { parts, .. } = &mut steps.messages[1] {
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "unsigned".into(),
                provider_data: None,
            },
        );
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "first".into(),
                provider_data: Some(json!({"type": "thinking", "signature": "s1"})),
            },
        );
    }
    if let Message::Assistant { parts, .. } = &mut second {
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: String::new(),
                provider_data: Some(json!({"type": "redacted_thinking", "data": "opaque"})),
            },
        );
        parts.insert(
            0,
            Part::Reasoning {
                id: PartId::new(),
                text: "second".into(),
                provider_data: Some(json!({"type": "thinking", "signature": "s2"})),
            },
        );
    }
    steps.messages.push(second);
    let encoded_msgs = AnthropicMessagesProtocol.encode(&steps).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(
        encoded_msgs
            .iter()
            .filter(|m| m["role"] == "assistant")
            .count(),
        2
    );
    assert_eq!(encoded_msgs[1]["content"][0]["signature"], "s1");
    assert_eq!(encoded_msgs[1]["content"][1]["type"], "text");
    assert_eq!(encoded_msgs[1]["content"][2]["type"], "tool_use");
    assert_eq!(encoded_msgs[3]["content"][0]["signature"], "s2");
    assert_eq!(encoded_msgs[3]["content"][1]["type"], "redacted_thinking");

    // 3. Swapped legacy final tool+text order still places text before tool_use and ends with tool_result.
    let mut swapped = assistant_tool_request(json!({}));
    if let Message::Assistant { parts, .. } = &mut swapped.messages[1] {
        parts.swap(0, 1);
    }
    let sm = AnthropicMessagesProtocol.encode(&swapped).unwrap()["messages"].clone();
    assert_eq!(sm.as_array().unwrap().len(), 3);
    assert_eq!(sm[1]["content"][0]["type"], "text");
    assert_eq!(sm[1]["content"][1]["type"], "tool_use");
    assert_eq!(sm[2]["role"], "user");

    // 4. Mid-conversation Message::System (compaction summary) stays in messages, not system prefix.
    let sys_req = CompletionRequest {
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
        ..req("claude-sonnet-4")
    };
    let sys_body = AnthropicMessagesProtocol.encode(&sys_req).unwrap();
    assert_eq!(sys_body["system"], "cached agent prefix");
    let serialized = sys_body["messages"].to_string();
    assert!(serialized.contains("HYA_COMPACTED_CONTEXT"));
    assert!(serialized.contains("earlier work: edited lib.rs"));

    // 5. Upstream tool_use id (`toolu_01ABC`) preserved on same model, stripped across models.
    let tool_events = decode_with(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_01ABC","name":"read"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"a\"}"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let (part, call_id, pdata) = tool_events
        .iter()
        .find_map(|e| match e {
            Event::ToolCallRequested {
                part,
                call,
                provider_data,
                ..
            } => Some((*part, *call, provider_data.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(pdata, Some(json!({"call_id": "toolu_01ABC"})));

    let make_id_req = |served: &str, target: &str| CompletionRequest {
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
                model: ModelRef::new(served),
                parts: vec![Part::Tool {
                    id: part,
                    call_id,
                    name: ToolName::new("read"),
                    state: ToolPartState::Completed {
                        input: json!({"path": "a"}),
                        output: json!("hello"),
                        time_ms: 1,
                    },
                    provider_data: pdata.clone(),
                }],
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
        ],
        ..req(target)
    };
    let same_id = AnthropicMessagesProtocol
        .encode(&make_id_req(
            "claude-sonnet-4-5",
            "anthropic/claude-sonnet-4-5#high",
        ))
        .unwrap();
    assert_eq!(same_id["messages"][1]["content"][0]["id"], "toolu_01ABC");
    assert_eq!(
        same_id["messages"][2]["content"][0]["tool_use_id"],
        "toolu_01ABC"
    );
    let cross_id = AnthropicMessagesProtocol
        .encode(&make_id_req("claude-sonnet-4-5", "claude-opus-4-6"))
        .unwrap();
    assert_eq!(
        cross_id["messages"][1]["content"][0]["id"],
        call_id.to_string()
    );
    assert_eq!(
        cross_id["messages"][2]["content"][0]["tool_use_id"],
        call_id.to_string()
    );
}

#[test]
fn anthropic_adaptive_thinking_block_binding_trailing_notices_and_legacy_gating() {
    let user_msg = Message::User {
        id: MessageId::new(),
        parts: vec![Part::Text {
            id: PartId::new(),
            text: "hi".to_string(),
        }],
    };

    // Turn 1 adaptive Claude 4.6..5.5: adaptive thinking, no block_binding, output_config.effort.
    for model_id in [
        "claude-sonnet-4-6",
        "claude-opus-4-7",
        "claude-sonnet-4-8-20260901",
        "claude-opus-5-5",
    ] {
        let mut r = req(model_id);
        r.messages = vec![user_msg.clone()];
        r.reasoning = Some(ReasoningEffort::XHigh);
        let body = AnthropicMessagesProtocol.encode(&r).unwrap();
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"}),
            "{model_id}"
        );
        assert!(body["thinking"].get("block_binding").is_none());
        assert_eq!(body["output_config"], json!({"effort": "xhigh"}));
        assert!(body["max_tokens"].as_u64().unwrap() >= 16_384);
    }

    // Turn 2+ with signed thinking, redacted_thinking, tool_use, and trailing bare user notices.
    let turn2_messages = vec![
        user_msg,
        Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new("claude-opus-4-7"),
            parts: vec![
                Part::Reasoning {
                    id: PartId::new(),
                    text: "still in current turn".to_string(),
                    provider_data: Some(json!({
                        "type": "thinking",
                        "thinking": "still in current turn",
                        "signature": "sig_keep"
                    })),
                },
                Part::Reasoning {
                    id: PartId::new(),
                    text: String::new(),
                    provider_data: Some(json!({
                        "type": "redacted_thinking",
                        "data": "opaque_redacted_blob"
                    })),
                },
                Part::Tool {
                    id: PartId::new(),
                    call_id: ToolCallId::new(),
                    name: ToolName::new("bash"),
                    state: ToolPartState::Completed {
                        input: json!({"command": "echo hi"}),
                        output: json!("ok"),
                        time_ms: 1,
                    },
                    provider_data: Some(json!({"call_id": "toolu_upstream_1"})),
                },
            ],
            finish: Some(FinishReason::ToolCalls),
            tokens: None,
            last_round: None,
            last_step_start: None,
        },
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "HEADLESS RUN — clock: 360s of 3600s used, 3240s left.".to_string(),
            }],
        },
        Message::User {
            id: MessageId::new(),
            parts: vec![Part::Text {
                id: PartId::new(),
                text: "[hya] Reasoning replay notice: dropped".to_string(),
            }],
        },
    ];

    let mut same = req("claude-opus-4-7");
    same.messages = turn2_messages.clone();
    same.reasoning = Some(ReasoningEffort::High);
    let same_body = AnthropicMessagesProtocol.encode(&same).unwrap();
    assert_eq!(
        same_body["thinking"],
        json!({
            "type": "adaptive",
            "display": "summarized",
            "block_binding": {"prefix_mismatch_behavior": "drop_block"}
        })
    );
    assert_eq!(same_body["output_config"], json!({"effort": "high"}));
    let same_msgs = same_body["messages"].as_array().unwrap();
    assert_eq!(same_msgs.len(), 3);
    assert_eq!(same_msgs[1]["content"][0]["signature"], "sig_keep");
    assert_eq!(same_msgs[1]["content"][1]["type"], "redacted_thinking");
    assert_eq!(same_msgs[1]["content"][2]["id"], "toolu_upstream_1");

    // Cross-model switch strips prior thinking & upstream tool id and omits block_binding.
    let mut switched = same.clone();
    switched.model = ModelRef::new("claude-sonnet-4-6");
    let sw_body = AnthropicMessagesProtocol.encode(&switched).unwrap();
    let sw_asst = sw_body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(sw_asst.len(), 1);
    assert_eq!(sw_asst[0]["type"], "tool_use");
    assert_ne!(sw_asst[0]["id"], "toolu_upstream_1");
    assert!(sw_body["thinking"].get("block_binding").is_none());

    // Legacy Claude <= 4.5 and non-Claude keep enabled+budget_tokens, no output_config/block_binding.
    for legacy_model in [
        "claude-sonnet-4-5-20250929",
        "claude-3-7-sonnet-20250219",
        "glm-5.3-flash",
    ] {
        let mut legacy = same.clone();
        legacy.model = ModelRef::new(legacy_model);
        if let Message::Assistant { model, .. } = &mut legacy.messages[1] {
            *model = ModelRef::new(legacy_model);
        }
        let body = AnthropicMessagesProtocol.encode(&legacy).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled", "{legacy_model}");
        assert!(body["thinking"]["budget_tokens"].as_u64().is_some());
        assert!(body["thinking"].get("block_binding").is_none());
        assert!(body.get("output_config").is_none());
    }
}

#[test]
fn google_encoding_media_and_thought_signatures_clusters_and_model_switch() {
    use hya_proto::context::{GOOGLE_TEXT_SIGNATURE_TYPE, GOOGLE_THOUGHT_TYPE};

    // 1. System, user, tools, and audio/video inlineData media encoding.
    let media_req = CompletionRequest {
        system: Some("be terse".to_string()),
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
        tools: vec![ToolSchema {
            name: ToolName::new("read"),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }],
        ..req("gemini-2.0-flash")
    };
    let media_body = GoogleProtocol.encode(&media_req).unwrap();
    assert_eq!(
        media_body["systemInstruction"]["parts"][0]["text"],
        "be terse"
    );
    assert_eq!(
        media_body["tools"][0]["functionDeclarations"][0]["name"],
        "read"
    );
    let parts = media_body["contents"][0]["parts"].as_array().unwrap();
    assert_eq!(parts[1]["inlineData"]["mimeType"], "audio/wav");
    assert_eq!(parts[2]["inlineData"]["mimeType"], "video/mp4");

    // 2. Decode thought/text/parallel functionCall signatures, replay multi-step clusters,
    //    coalesce mid-conversation System, strip on cross-model switch, and check Gemini 2.5 vs 3.
    let events = decode_with(
        &GoogleProtocol,
        &[
            r#"{"candidates":[{"content":{"parts":[{"thought":true,"text":"plan step 1","thoughtSignature":"sig_t1"},{"text":"checking","thoughtSignature":"sig_txt"},{"functionCall":{"id":"fc_1","name":"read","args":{"path":"a"}},"thoughtSignature":"sig_fc1"},{"functionCall":{"id":"fc_2","name":"glob","args":{"pattern":"*"}}}],"role":"model"},"finishReason":"STOP"}]}"#,
        ],
    );
    assert!(
        events
            .iter()
            .all(|e| !matches!(e, Event::TextDelta { delta, .. } if delta.contains("plan step 1")))
    );
    let rdata: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ReasoningEnd { provider_data, .. } => provider_data.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(
        rdata,
        vec![
            json!({"type": GOOGLE_THOUGHT_TYPE, "thought_signature": "sig_t1"}),
            json!({"type": GOOGLE_TEXT_SIGNATURE_TYPE, "thought_signature": "sig_txt"}),
        ]
    );
    let tools: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolCallRequested {
                call,
                name,
                provider_data,
                ..
            } => Some((name.to_string(), *call, provider_data.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        tools[0].2,
        Some(json!({"call_id": "fc_1", "thought_signature": "sig_fc1"}))
    );
    assert_eq!(tools[1].2, Some(json!({"call_id": "fc_2"})));

    let replay_req = CompletionRequest {
        system: Some("static system".into()),
        reasoning: Some(ReasoningEffort::High),
        messages: vec![
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
                model: ModelRef::new("gemini-3-pro-preview"),
                parts: vec![
                    Part::Reasoning {
                        id: PartId::new(),
                        text: "Claude leak".into(),
                        provider_data: Some(json!({"type": "thinking", "signature": "cl_sig"})),
                    },
                    Part::Reasoning {
                        id: PartId::new(),
                        text: "plan step 1".into(),
                        provider_data: Some(json!({
                            "type": GOOGLE_THOUGHT_TYPE,
                            "thought_signature": "sig_t1"
                        })),
                    },
                    Part::Text {
                        id: PartId::new(),
                        text: "checking".into(),
                    },
                    Part::Reasoning {
                        id: PartId::new(),
                        text: String::new(),
                        provider_data: Some(json!({
                            "type": GOOGLE_TEXT_SIGNATURE_TYPE,
                            "thought_signature": "sig_txt"
                        })),
                    },
                    Part::Tool {
                        id: PartId::new(),
                        call_id: tools[0].1,
                        name: ToolName::new("read"),
                        state: ToolPartState::Completed {
                            input: json!({"path": "a"}),
                            output: json!("content a"),
                            time_ms: 1,
                        },
                        provider_data: tools[0].2.clone(),
                    },
                    Part::Tool {
                        id: PartId::new(),
                        call_id: tools[1].1,
                        name: ToolName::new("glob"),
                        state: ToolPartState::Completed {
                            input: json!({"pattern": "*"}),
                            output: json!("a"),
                            time_ms: 1,
                        },
                        provider_data: tools[1].2.clone(),
                    },
                    Part::Tool {
                        id: PartId::new(),
                        call_id: ToolCallId::new(),
                        name: ToolName::new("read"),
                        state: ToolPartState::Completed {
                            input: json!({"path": "b"}),
                            output: json!("content b"),
                            time_ms: 1,
                        },
                        provider_data: Some(json!({
                            "call_id": "fc_3",
                            "thought_signature": "sig_fc3"
                        })),
                    },
                ],
                finish: Some(FinishReason::ToolCalls),
                tokens: None,
                last_round: None,
                last_step_start: None,
            },
            Message::System {
                id: MessageId::new(),
                content: "HYA_COMPACTED_CONTEXT\nsummary".into(),
            },
        ],
        ..req("google/gemini-3-pro-preview#high")
    };
    let body = GoogleProtocol.encode(&replay_req).unwrap();
    assert_eq!(
        body["systemInstruction"],
        json!({"parts": [{"text": "static system"}]})
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "high", "includeThoughts": true})
    );
    let contents = body["contents"].as_array().unwrap();
    assert_eq!(contents.len(), 5);
    let step1 = contents[1]["parts"].as_array().unwrap();
    assert_eq!(
        step1[0],
        json!({"thought": true, "text": "plan step 1", "thoughtSignature": "sig_t1"})
    );
    assert_eq!(
        step1[1],
        json!({"text": "checking", "thoughtSignature": "sig_txt"})
    );
    assert_eq!(
        step1[2],
        json!({"functionCall": {"id": "fc_1", "name": "read", "args": {"path": "a"}}, "thoughtSignature": "sig_fc1"})
    );
    assert_eq!(contents[2]["parts"][0]["functionResponse"]["id"], "fc_1");
    assert_eq!(contents[2]["parts"][1]["functionResponse"]["id"], "fc_2");
    assert_eq!(contents[3]["parts"][0]["thoughtSignature"], "sig_fc3");
    assert_eq!(contents[4]["parts"][0]["functionResponse"]["id"], "fc_3");
    assert_eq!(
        contents[4]["parts"][1]["text"],
        "HYA_COMPACTED_CONTEXT\nsummary"
    );

    let mut switched = replay_req.clone();
    switched.model = ModelRef::new("gemini-3.5-flash");
    let encoded = GoogleProtocol.encode(&switched).unwrap()["contents"].to_string();
    for stripped in ["sig_fc1", "sig_t1", "sig_txt", "fc_1", "\"thought\":true"] {
        assert!(!encoded.contains(stripped), "leaked {stripped}");
    }

    let mut legacy = replay_req;
    legacy.model = ModelRef::new("gemini-2.5-pro");
    let legacy_body = GoogleProtocol.encode(&legacy).unwrap();
    assert_eq!(
        legacy_body["generationConfig"]["thinkingConfig"],
        json!({"thinkingBudget": 16000, "includeThoughts": true})
    );
}

#[test]
fn encoders_emit_reasoning_null_tool_inputs_and_structured_tool_errors() {
    // 1. Reasoning emitted only when set.
    let mut r = req("m");
    r.reasoning = Some(ReasoningEffort::High);
    let a = AnthropicMessagesProtocol.encode(&r).unwrap();
    assert_eq!(a["thinking"]["type"], "enabled");
    assert_eq!(a["thinking"]["budget_tokens"], 16000);
    assert!(a["max_tokens"].as_u64().unwrap() > 16384);
    assert_eq!(
        OpenAiChatProtocol.encode(&r).unwrap()["reasoning_effort"],
        "high"
    );
    assert_eq!(
        GoogleProtocol.encode(&r).unwrap()["generationConfig"]["thinkingConfig"]["thinkingBudget"],
        16000
    );
    r.reasoning = None;
    assert!(
        AnthropicMessagesProtocol
            .encode(&r)
            .unwrap()
            .get("thinking")
            .is_none()
    );
    assert!(
        OpenAiChatProtocol
            .encode(&r)
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );

    // 2. Tool call & result encoding + Null tool input normalized to {}.
    let oai = OpenAiChatProtocol
        .encode(&assistant_tool_request(json!({"path": "a"})))
        .unwrap();
    let oai_msgs = oai["messages"].as_array().unwrap();
    assert_eq!(
        oai_msgs[1]["tool_calls"][0]["function"]["arguments"],
        "{\"path\":\"a\"}"
    );
    assert_eq!(
        oai_msgs[2]["tool_call_id"],
        oai_msgs[1]["tool_calls"][0]["id"]
    );
    assert_eq!(oai_msgs[2]["content"], "hello");

    let anth = AnthropicMessagesProtocol
        .encode(&assistant_tool_request(json!({"path": "a"})))
        .unwrap();
    let anth_msgs = anth["messages"].as_array().unwrap();
    assert_eq!(anth_msgs[1]["content"][1]["input"]["path"], "a");
    assert_eq!(anth_msgs[2]["content"][0]["is_error"], false);
    assert_eq!(
        anth_msgs[2]["content"][0]["tool_use_id"],
        anth_msgs[1]["content"][1]["id"]
    );

    assert_eq!(
        OpenAiChatProtocol
            .encode(&assistant_tool_request(Value::Null))
            .unwrap()["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{}"
    );
    assert_eq!(
        AnthropicMessagesProtocol
            .encode(&assistant_tool_request(Value::Null))
            .unwrap()["messages"][1]["content"][1]["input"],
        json!({})
    );

    // 3. Structured vs primitive tool errors on OpenAI Chat and Anthropic.
    let err_req = |val: Value, msg: &str| CompletionRequest {
        messages: vec![Message::Assistant {
            id: MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new("test-model"),
            parts: vec![Part::Tool {
                id: PartId::new(),
                call_id: ToolCallId::new(),
                name: ToolName::new("bash"),
                state: ToolPartState::Error {
                    input: json!({"command": "sleep 10"}),
                    message: msg.to_string(),
                    value: Some(val),
                },
                provider_data: None,
            }],
            finish: None,
            tokens: None,
            last_round: None,
            last_step_start: None,
        }],
        ..req("test-model")
    };
    let structured = json!({
        "error": {"type": "unknown", "message": "Tool execution interrupted"},
        "content": [],
        "structured": {},
    });
    assert_eq!(
        OpenAiChatProtocol
            .encode(&err_req(structured.clone(), "interrupted"))
            .unwrap()["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"],
        serde_json::to_string(&structured).unwrap()
    );
    let anth_err = AnthropicMessagesProtocol
        .encode(&err_req(structured.clone(), "denied"))
        .unwrap();
    let result = &anth_err["messages"].as_array().unwrap().last().unwrap()["content"][0];
    assert_eq!(
        result["content"],
        serde_json::to_string(&structured).unwrap()
    );
    assert_eq!(result["is_error"], true);

    assert_eq!(
        OpenAiChatProtocol
            .encode(&err_req(json!(503), "503"))
            .unwrap()["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"],
        "503"
    );
}

#[test]
fn image_content_encoding_and_unsupported_media_rejection() {
    let mut image_req = req("image-model");
    image_req.messages = vec![Message::User {
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
    }];

    assert_eq!(
        OpenAiResponsesProtocol.encode(&image_req).unwrap()["input"][0]["content"],
        json!([
            {"type": "input_text", "text": "inspect"},
            {"type": "input_image", "image_url": "data:image/png;base64,aGVsbG8="},
        ])
    );
    assert_eq!(
        OpenAiChatProtocol.encode(&image_req).unwrap()["messages"][0]["content"],
        json!([
            {"type": "text", "text": "inspect"},
            {"type": "image_url", "image_url": {"url": "data:image/png;base64,aGVsbG8="}},
        ])
    );
    assert_eq!(
        AnthropicMessagesProtocol.encode(&image_req).unwrap()["messages"][0]["content"],
        json!([
            {"type": "text", "text": "inspect"},
            {
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": "aGVsbG8="}
            },
        ])
    );

    // Raw JPEG base64 starting with '/' is encoded as data URL without path rejection.
    let raw_jpeg = "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0a";
    let mut jpeg_req = image_req.clone();
    if let Message::User { parts, .. } = &mut jpeg_req.messages[0] {
        parts[1] = Part::Media {
            id: PartId::new(),
            media_type: "image/jpeg".to_string(),
            data: raw_jpeg.to_string(),
            filename: Some("pixel.jpg".to_string()),
        };
    }
    assert_eq!(
        OpenAiResponsesProtocol.encode(&jpeg_req).unwrap()["input"][0]["content"][1]["image_url"],
        format!("data:image/jpeg;base64,{raw_jpeg}")
    );

    // Unsupported media type (audio/wav) fails closed on OpenAI encoders.
    let mut audio_req = image_req;
    if let Message::User { parts, .. } = &mut audio_req.messages[0] {
        parts[1] = Part::Media {
            id: PartId::new(),
            media_type: "audio/wav".to_string(),
            data: "ZGF0YQ==".to_string(),
            filename: None,
        };
    }
    assert!(OpenAiResponsesProtocol.encode(&audio_req).is_err());
    assert!(OpenAiChatProtocol.encode(&audio_req).is_err());
}

#[test]
fn decoders_record_invalid_tool_argument_json_and_replay_as_object() {
    const CUT_ARGS: &str = r#"{"command":"ec"#;
    const CUT_ERROR: &str = "EOF while parsing a string at line 1 column 14";
    const TRAILING_ARGS: &str = r#"{"ops":[]}}"#;
    const TRAILING_ERROR: &str = "trailing characters at line 1 column 11";

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

    for (label, events, err, finish) in [
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
    ] {
        let (input, reported) = requested_call(&events);
        assert_eq!(
            hya_provider::invalid_tool_arguments(&input),
            Some(err),
            "{label}"
        );
        assert_eq!(reported, finish, "{label}");
    }

    // Invalid-argument object replays cleanly across all 4 protocols.
    let (input, _) = requested_call(&decode_with(
        &OpenAiChatProtocol,
        &[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":"length"}]}"#,
        ],
    ));
    assert!(input.is_object());
    let mut request = assistant_tool_request(input.clone());
    if let Message::Assistant { parts, .. } = &mut request.messages[1]
        && let Part::Tool { state, .. } = &mut parts[1]
    {
        *state = ToolPartState::Error {
            input: input.clone(),
            message: "arguments are not valid JSON".to_string(),
            value: None,
        };
    }
    let as_obj = |v: &Value| serde_json::from_str::<Value>(v.as_str().unwrap()).unwrap();
    assert_eq!(
        as_obj(
            &OpenAiChatProtocol.encode(&request).unwrap()["messages"][1]["tool_calls"][0]["function"]
                ["arguments"]
        ),
        input
    );
    let resp_body = OpenAiResponsesProtocol.encode(&request).unwrap();
    let call = resp_body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "function_call")
        .unwrap();
    assert_eq!(as_obj(&call["arguments"]), input);
    assert_eq!(
        AnthropicMessagesProtocol.encode(&request).unwrap()["messages"][1]["content"][1]["input"],
        input
    );
    assert_eq!(
        GoogleProtocol.encode(&request).unwrap()["contents"][1]["parts"][1]["functionCall"]["args"],
        input
    );
}
