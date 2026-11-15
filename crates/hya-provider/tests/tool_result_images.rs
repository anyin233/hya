//! An image a tool returned reaches a model that takes images, per protocol;
//! any other model is told in text that it is not shown.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{
    AgentName, Message, MessageId, ModelRef, Part, PartId, ToolCallId, ToolName, ToolPartState,
};
use hya_provider::{
    AnthropicMessagesProtocol, CompletionRequest, OpenAiChatProtocol, OpenAiResponsesProtocol,
    Protocol,
};
use serde_json::{Value, json};

const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";
const READ_TEXT: &str = "Image file (image/png, 8 bytes).";
const NOT_SHOWN: &str = "(image/png) is not shown to you";

fn tool(name: &str, output: Value) -> Part {
    Part::Tool {
        id: PartId::new(),
        call_id: ToolCallId::new(),
        name: ToolName::new(name),
        state: ToolPartState::Completed {
            input: json!({"path": "board.png"}),
            output,
            time_ms: 1,
        },
    }
}

fn read_image() -> Part {
    tool(
        "read",
        json!({
            "title": "board.png",
            "output": READ_TEXT,
            "metadata": {"preview": "Image read successfully", "truncated": false, "loaded": []},
            "attachments": [{"type": "file", "mime": "image/png", "url": PNG}],
        }),
    )
}

/// The first step reads the image; the second adds text and another tool call.
fn requests() -> (CompletionRequest, CompletionRequest) {
    let user = Message::User {
        id: MessageId::new(),
        parts: vec![Part::Text {
            id: PartId::new(),
            text: "what is on board.png?".to_string(),
        }],
    };
    let assistant = |parts: Vec<Part>| Message::Assistant {
        id: MessageId::new(),
        agent: AgentName::new("build"),
        model: ModelRef::new("m"),
        parts,
        finish: None,
        tokens: None,
        last_round: None,
    };
    let request = |messages: Vec<Message>| CompletionRequest {
        model: ModelRef::new("m"),
        system: None,
        messages,
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    };
    let first_step = vec![read_image()];
    let mut both_steps = first_step.clone();
    both_steps.push(Part::Text {
        id: PartId::new(),
        text: "A chess board.".to_string(),
    });
    both_steps.push(tool("bash", json!({"output": "ok"})));
    (
        request(vec![user.clone(), assistant(first_step)]),
        request(vec![user, assistant(both_steps)]),
    )
}

/// The second step's request starts with the first step's request: the image
/// sits at the same place in every later request of the session.
fn assert_prefix(first: &Value, second: &Value) {
    let (first, second) = (first.as_array().unwrap(), second.as_array().unwrap());
    assert!(second.len() > first.len());
    assert_eq!(first[..], second[..first.len()]);
}

#[test]
fn anthropic_sends_the_image_inside_the_tool_result() {
    let (first, second) = requests();
    let body = AnthropicMessagesProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    let content = &body["messages"][2]["content"][0]["content"];
    assert_eq!(content[0], json!({"type": "text", "text": READ_TEXT}));
    assert_eq!(
        content[1],
        json!({"type": "image", "source": {
            "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=",
        }})
    );
    assert_eq!(content.as_array().unwrap().len(), 2);

    let later = AnthropicMessagesProtocol
        .encode_for_model(&second, None, true)
        .unwrap();
    assert_prefix(&body["messages"], &later["messages"]);
}

#[test]
fn openai_responses_sends_the_image_in_the_function_call_output() {
    let (first, second) = requests();
    let body = OpenAiResponsesProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    let output = &body["input"][2];
    assert_eq!(output["type"], "function_call_output");
    assert_eq!(
        output["output"],
        json!([
            {"type": "input_text", "text": READ_TEXT},
            {"type": "input_image", "image_url": PNG},
        ])
    );

    let later = OpenAiResponsesProtocol
        .encode_for_model(&second, None, true)
        .unwrap();
    assert_prefix(&body["input"], &later["input"]);
}

#[test]
fn openai_chat_sends_the_image_in_a_user_message_after_the_tool_messages() {
    let (first, second) = requests();
    let body = OpenAiChatProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["content"], READ_TEXT);
    let call = messages[1]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(
        messages[3],
        json!({"role": "user", "content": [
            {"type": "text", "text": format!("Image from the result of tool call {call}:")},
            {"type": "image_url", "image_url": {"url": PNG}},
        ]})
    );
    assert_eq!(messages.len(), 4);

    let later = OpenAiChatProtocol
        .encode_for_model(&second, None, true)
        .unwrap();
    assert_prefix(&body["messages"], &later["messages"]);
}

/// Without image input (`Some(false)` or unknown) every protocol sends text
/// only, and the text says the image is not shown.
#[test]
fn a_model_without_image_input_is_told_the_image_is_not_shown() {
    let (first, _) = requests();
    let anthropic = AnthropicMessagesProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    let responses = OpenAiResponsesProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    let chat = OpenAiChatProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    let chat_messages = chat["messages"].as_array().unwrap();
    assert_eq!(chat_messages.len(), 3, "no user message with the image");
    for text in [
        &anthropic["messages"][2]["content"][0]["content"],
        &responses["input"][2]["output"],
        &chat_messages[2]["content"],
    ] {
        let text = text.as_str().unwrap();
        assert!(
            text.starts_with(READ_TEXT) && text.contains(NOT_SHOWN),
            "{text}"
        );
    }
    // `encode` is the same as no image input.
    assert_eq!(AnthropicMessagesProtocol.encode(&first).unwrap(), anthropic);
    assert_eq!(OpenAiResponsesProtocol.encode(&first).unwrap(), responses);
    assert_eq!(OpenAiChatProtocol.encode(&first).unwrap(), chat);
}
