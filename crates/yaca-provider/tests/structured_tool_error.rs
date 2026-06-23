#![allow(clippy::unwrap_used)]

use serde_json::{Value, json};
use yaca_proto::{AgentName, Message, ModelRef, Part, PartId, ToolCallId, ToolName, ToolPartState};
use yaca_provider::{AnthropicMessagesProtocol, CompletionRequest, OpenAiChatProtocol, Protocol};

fn request(error_value: Value, message: &str) -> CompletionRequest {
    CompletionRequest {
        model: ModelRef::new("test-model"),
        system: None,
        messages: vec![Message::Assistant {
            id: yaca_proto::MessageId::new(),
            agent: AgentName::new("build"),
            model: ModelRef::new("test-model"),
            parts: vec![
                Part::Text {
                    id: PartId::new(),
                    text: "running tool".to_string(),
                },
                Part::Tool {
                    id: PartId::new(),
                    call_id: ToolCallId::new(),
                    name: ToolName::new("bash"),
                    state: ToolPartState::Error {
                        input: json!({ "command": "sleep 10" }),
                        message: message.to_string(),
                        value: Some(error_value),
                    },
                },
            ],
            finish: None,
            tokens: None,
        }],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

#[test]
fn openai_chat_preserves_structured_tool_errors() {
    let error = json!({
        "error": { "type": "unknown", "message": "Tool execution interrupted" },
        "content": [],
        "structured": {},
    });
    let body = OpenAiChatProtocol
        .encode(&request(error.clone(), "interrupted"))
        .unwrap();
    let messages = body["messages"].as_array().unwrap();

    assert_eq!(
        messages.last().unwrap()["content"],
        serde_json::to_string(&error).unwrap()
    );
}

#[test]
fn anthropic_preserves_structured_tool_errors_and_error_flag() {
    let error = json!({ "error": { "type": "permission", "message": "denied" } });
    let body = AnthropicMessagesProtocol
        .encode(&request(error.clone(), "denied"))
        .unwrap();
    let messages = body["messages"].as_array().unwrap();
    let result = &messages.last().unwrap()["content"][0];

    assert_eq!(result["content"], serde_json::to_string(&error).unwrap());
    assert_eq!(result["is_error"], true);
}

#[test]
fn primitive_tool_errors_remain_plain_text() {
    let body = OpenAiChatProtocol
        .encode(&request(json!(503), "503"))
        .unwrap();
    let messages = body["messages"].as_array().unwrap();

    assert_eq!(messages.last().unwrap()["content"], "503");
}
