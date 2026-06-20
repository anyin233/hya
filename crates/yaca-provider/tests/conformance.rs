#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use yaca_proto::{
    AgentName, Event, FinishReason, Message, MessageId, ModelRef, Part, PartId, SessionId,
    ToolCallId, ToolName, ToolPartState,
};
use yaca_provider::{
    AnthropicMessagesProtocol, CompletionRequest, FakeProvider, FakeStep, OpenAiChatProtocol,
    Protocol, ProviderRouter,
};

fn summarize(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::TextStart { .. } => "text_start".to_string(),
            Event::TextDelta { delta, .. } => format!("text_delta:{delta}"),
            Event::TextEnd { .. } => "text_end".to_string(),
            Event::ToolInputStart { name, .. } => format!("tool_start:{name}"),
            Event::ToolInputDelta { .. } => "tool_input_delta".to_string(),
            Event::ToolCallRequested { name, input, .. } => format!("tool_call:{name}:{input}"),
            Event::MessageFinished { finish, .. } => format!("finish:{finish:?}"),
            other => format!("other:{other:?}"),
        })
        .collect()
}

fn decode_all(protocol: &OpenAiChatProtocol, lines: &[&str]) -> Vec<Event> {
    let s = SessionId::new();
    let m = MessageId::new();
    let mut decoder = protocol.decoder(s, m);
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
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
                    },
                ],
                finish: None,
                tokens: None,
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
    }
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

#[test]
fn openai_null_tool_input_becomes_empty_object_string() {
    let body = OpenAiChatProtocol
        .encode(&assistant_tool_request(serde_json::Value::Null))
        .unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs[1]["tool_calls"][0]["function"]["arguments"], "{}");
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
