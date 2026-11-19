//! An image a tool returned reaches a model that takes images, per protocol,
//! in the request after the round that read it; any other model is told in
//! text that it is not shown.

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
const NOT_ATTACHED: &str = "(image/png) is not attached to this request";
const NOT_FITTED: &str = "(image/png) was not shown: this step's images exceed";

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

/// A request continuing an assistant message made of `steps`; its last step
/// is the round the request answers.
fn continuing(steps: Vec<Vec<Part>>) -> CompletionRequest {
    let last_step_start = steps[..steps.len() - 1].iter().map(Vec::len).sum();
    CompletionRequest {
        model: ModelRef::new("m"),
        system: None,
        messages: vec![
            Message::User {
                id: MessageId::new(),
                parts: vec![Part::Text {
                    id: PartId::new(),
                    text: "what is on board.png?".to_string(),
                }],
            },
            Message::Assistant {
                id: MessageId::new(),
                agent: AgentName::new("build"),
                model: ModelRef::new("m"),
                parts: steps.concat(),
                finish: None,
                tokens: None,
                last_round: None,
                last_step_start: Some(last_step_start),
            },
        ],
        tools: Vec::new(),
        temperature: None,
        max_output_tokens: None,
        reasoning: None,
        headers: Default::default(),
    }
}

fn text(text: &str) -> Part {
    Part::Text {
        id: PartId::new(),
        text: text.to_string(),
    }
}

/// The first step reads the image; the second adds text and another tool call.
fn requests() -> (CompletionRequest, CompletionRequest) {
    let read = read_image();
    (
        continuing(vec![vec![read.clone()]]),
        continuing(vec![
            vec![read],
            vec![
                text("A chess board."),
                tool("bash", json!({"output": "ok"})),
            ],
        ]),
    )
}

/// The request after the second step carries the result as text with the notice.
fn assert_not_sent_again(later: &Value) {
    let later = later.to_string();
    assert!(!later.contains("iVBORw0KGgo"), "{later}");
    assert!(
        later.contains(&format!(
            "{READ_TEXT}\\n\\n[The image of this result {NOT_ATTACHED}"
        )),
        "{later}"
    );
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
    assert_not_sent_again(&later);
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
    assert_not_sent_again(&later);
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
    assert_not_sent_again(&later);
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

const PROTOCOLS: [&dyn Protocol; 3] = [
    &AnthropicMessagesProtocol,
    &OpenAiResponsesProtocol,
    &OpenAiChatProtocol,
];

/// A valid PNG data URL of exactly `len` bytes (`len` is 2 mod 4).
fn url(len: usize) -> String {
    format!("data:image/png;base64,iVBORw0KGgoA{}", "A".repeat(len - 34))
}

fn read_page(n: usize, url: &str) -> Part {
    tool(
        "read",
        json!({
            "title": format!("page{n}.png"),
            "output": format!("Image file page{n}."),
            "metadata": {},
            "attachments": [{"type": "file", "mime": "image/png", "url": url}],
        }),
    )
}

/// Steps of image reads, numbered page1, page2, … in call order.
fn pages(steps: &[&[&str]]) -> Vec<Vec<Part>> {
    let mut n = 0;
    steps
        .iter()
        .map(|urls| {
            urls.iter()
                .map(|url| {
                    n += 1;
                    read_page(n, url)
                })
                .collect()
        })
        .collect()
}

fn encode(protocol: &dyn Protocol, steps: Vec<Vec<Part>>) -> String {
    protocol
        .encode_for_model(&continuing(steps), None, true)
        .unwrap()
        .to_string()
}

/// Image payloads in a request body.
fn sent(body: &str) -> usize {
    body.matches("iVBORw0KGgo").count()
}

/// Page `page`'s result carries the notice that starts `[The image {notice}`.
fn noticed(body: &str, page: usize, notice: &str) -> bool {
    body.contains(&format!("Image file page{page}.\\n\\n[The image {notice}"))
}

fn not_fitted() -> String {
    format!("attached to this result {NOT_FITTED}")
}

fn not_attached() -> String {
    format!("of this result {NOT_ATTACHED}")
}

/// One round's images go in call order within 2 MiB, the first always.
#[test]
fn the_newest_rounds_images_fill_the_budget_and_the_rest_become_text() {
    // About 900 KiB per URL: two fit the budget, three exceed it.
    let big = url(900 * 1024 + 2);
    let oversized = url(3 * 1024 * 1024 + 2);
    for protocol in PROTOCOLS {
        // Two images read in the same round both go.
        let two = encode(protocol, pages(&[&[&big, &big]]));
        assert_eq!(sent(&two), 2);
        assert!(!two.contains(NOT_ATTACHED) && !two.contains(NOT_FITTED));

        // Two images that total exactly 2 MiB both go; four bytes more do not.
        let exact = encode(protocol, pages(&[&[&url(1_048_578), &url(1_048_574)]]));
        assert_eq!(sent(&exact), 2);
        let over = encode(protocol, pages(&[&[&url(1_048_578), &url(1_048_578)]]));
        assert_eq!(sent(&over), 1);
        assert!(noticed(&over, 2, &not_fitted()));

        // Over the budget: the first two go, the rest, even a tiny one, become text.
        let four = encode(protocol, pages(&[&[&big, &big, &big, PNG]]));
        assert_eq!(sent(&four), 2);
        assert!(noticed(&four, 3, &not_fitted()) && noticed(&four, 4, &not_fitted()));

        // The first image goes even when it alone exceeds the budget.
        let first = encode(protocol, pages(&[&[&oversized, PNG]]));
        assert_eq!(sent(&first), 1);
        assert!(first.contains(&oversized[22..]) && noticed(&first, 2, &not_fitted()));
    }
}

/// A response of reasoning, a call that reads an image, and text: the image
/// goes in the next request.
#[test]
fn an_image_read_by_a_call_before_text_in_the_same_step_is_sent() {
    for protocol in PROTOCOLS {
        let body = encode(
            protocol,
            vec![vec![
                Part::Reasoning {
                    id: PartId::new(),
                    text: "look at the board".to_string(),
                    provider_data: None,
                },
                read_page(1, PNG),
                text("Reading the board."),
            ]],
        );
        assert_eq!(sent(&body), 1, "{body}");
    }
}

/// Two successive rounds of one image read each, without reasoning or text:
/// only the second round's image goes.
#[test]
fn only_the_last_of_two_tool_only_rounds_sends_its_image() {
    let big = url(900 * 1024 + 2);
    for protocol in PROTOCOLS {
        let body = encode(protocol, pages(&[&[PNG], &[&big]]));
        assert_eq!(sent(&body), 1);
        assert!(body.contains(&big[22..]) && noticed(&body, 1, &not_attached()));
    }
}

/// A round without tool calls (a completion check, or a round without parts)
/// after the image rounds: no image goes.
#[test]
fn a_round_without_tool_calls_sends_no_image() {
    let big = url(900 * 1024 + 2);
    for protocol in PROTOCOLS {
        for after in [vec![text("Done.")], Vec::new()] {
            let mut steps = pages(&[&[PNG], &[&big]]);
            steps.push(after);
            let body = encode(protocol, steps);
            assert_eq!(sent(&body), 0);
            assert!(noticed(&body, 1, &not_attached()) && noticed(&body, 2, &not_attached()));
        }
    }
}
