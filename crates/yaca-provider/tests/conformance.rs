#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use futures::StreamExt;
use yaca_proto::{Event, FinishReason, MessageId, ModelRef, SessionId};
use yaca_provider::{
    CompletionRequest, FakeProvider, FakeStep, OpenAiChatProtocol, Protocol, ProviderRouter,
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
