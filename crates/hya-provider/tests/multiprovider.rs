#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{Event, FinishReason, MessageId, SessionId, ToolName, ToolSchema};
use hya_provider::{
    AnthropicMessagesProtocol, Capabilities, CompletionRequest, FakeProvider, FakeStep,
    OpenAiChatProtocol, Protocol, preflight,
};
use serde_json::json;

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
            Event::ReasoningStart { .. } => "reasoning_start".to_string(),
            Event::ReasoningDelta { delta, .. } => format!("reasoning_delta:{delta}"),
            Event::ReasoningEnd { .. } => "reasoning_end".to_string(),
            other => format!("other:{other:?}"),
        })
        .collect()
}

fn decode_all<P: Protocol>(protocol: &P, lines: &[&str]) -> Vec<Event> {
    let mut decoder = protocol.decoder(SessionId::new(), MessageId::new());
    let mut out = Vec::new();
    for line in lines {
        out.extend(decoder.push(line).unwrap());
    }
    out.extend(decoder.finish().unwrap());
    out
}

#[test]
fn anthropic_decodes_text() {
    let events = decode_all(
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
    assert_eq!(
        summarize(&events),
        vec![
            "text_start",
            "text_delta:Hello world",
            "text_end",
            "finish:Stop"
        ]
    );
}

#[test]
fn anthropic_decodes_thinking() {
    let events = decode_all(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"message_start","message":{}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me reason"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"42"}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    assert_eq!(
        summarize(&events),
        vec![
            "reasoning_start",
            "reasoning_delta:Let me reason",
            "reasoning_end",
            "text_start",
            "text_delta:42",
            "text_end",
            "finish:Stop"
        ]
    );
}

#[test]
fn anthropic_decodes_tool_call() {
    let events = decode_all(
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
fn all_providers_agree_on_text_shape() {
    let anthropic = decode_all(
        &AnthropicMessagesProtocol,
        &[
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello world"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ],
    );
    let openai = decode_all(
        &OpenAiChatProtocol,
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
    assert_eq!(summarize(&anthropic), summarize(&openai));
    assert_eq!(summarize(&openai), summarize(&fake));
}

#[test]
fn preflight_rejects_tools_on_incapable_route() {
    let tool = ToolSchema {
        name: ToolName::new("read"),
        description: "read".to_string(),
        input_schema: json!({"type": "object"}),
        output_schema: None,
    };
    let with_tools = CompletionRequest {
        model: "m".into(),
        system: None,
        messages: Vec::new(),
        tools: vec![tool],
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let no_tools = CompletionRequest {
        tools: Vec::new(),
        ..with_tools.clone()
    };

    let incapable = Capabilities::default();
    let capable = Capabilities {
        streaming_tool_calls: true,
        ..Capabilities::default()
    };

    assert!(preflight(&incapable, &with_tools).is_err());
    assert!(preflight(&capable, &with_tools).is_ok());
    assert!(preflight(&incapable, &no_tools).is_ok());
}
