//! Tool-result image attachment: attached per protocol only on the immediately
//! following request, gated by model vision capability, and bounded to 2 MiB.

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
const NOT_ATTACHED: &str = "An image is sent to you only in the request right after the call \
     that produced it; later requests, like this one, carry this note instead. This is the \
     normal course, not a failure. To look at the image of this result (image/png), make the \
     call again";
const NOT_FITTED: &str = "(image/png) was not shown: this step's images exceed";

const PROTOCOLS: [&dyn Protocol; 3] = [
    &AnthropicMessagesProtocol,
    &OpenAiResponsesProtocol,
    &OpenAiChatProtocol,
];

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
        provider_data: None,
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

fn text(t: &str) -> Part {
    Part::Text {
        id: PartId::new(),
        text: t.to_string(),
    }
}

fn url(len: usize) -> String {
    format!("data:image/png;base64,iVBORw0KGgoA{}", "A".repeat(len - 34))
}

fn read_page(n: usize, data_url: &str) -> Part {
    tool(
        "read",
        json!({
            "title": format!("page{n}.png"),
            "output": format!("Image file page{n}."),
            "metadata": {},
            "attachments": [{"type": "file", "mime": "image/png", "url": data_url}],
        }),
    )
}

fn pages(steps: &[&[&str]]) -> Vec<Vec<Part>> {
    let mut n = 0;
    steps
        .iter()
        .map(|urls| {
            urls.iter()
                .map(|u| {
                    n += 1;
                    read_page(n, u)
                })
                .collect()
        })
        .collect()
}

fn encode_val(protocol: &dyn Protocol, steps: Vec<Vec<Part>>) -> Value {
    protocol
        .encode_for_model(&continuing(steps), None, true)
        .unwrap()
}

fn count_and_scan(
    value: &Value,
    images: &mut usize,
    texts: &mut Vec<String>,
    oversized_needle: Option<&str>,
    saw_oversized: &mut bool,
) {
    match value {
        Value::String(s) => {
            if let Some(needle) = oversized_needle
                && s.len() >= needle.len()
                && s.ends_with(needle)
            {
                *saw_oversized = true;
            }
            if s.starts_with("data:image/png;base64,iVBORw0KGgo") || s.starts_with("iVBORw0KGgo") {
                *images += 1;
            } else if s.len() < 4096 {
                texts.push(s.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                count_and_scan(item, images, texts, oversized_needle, saw_oversized);
            }
        }
        Value::Object(map) => {
            for v in map.values() {
                count_and_scan(v, images, texts, oversized_needle, saw_oversized);
            }
        }
        _ => {}
    }
}

fn inspect(value: &Value, oversized_needle: Option<&str>) -> (usize, Vec<String>, bool) {
    let mut images = 0;
    let mut texts = Vec::new();
    let mut saw_oversized = false;
    count_and_scan(
        value,
        &mut images,
        &mut texts,
        oversized_needle,
        &mut saw_oversized,
    );
    (images, texts, saw_oversized)
}

fn noticed_in(texts: &[String], page: usize, notice: &str) -> bool {
    let prefix = format!("Image file page{page}.\n\n[{notice}");
    texts.iter().any(|t| t.contains(&prefix))
}

fn not_fitted() -> String {
    format!("The image attached to this result {NOT_FITTED}")
}

fn assert_not_sent_again(later: &Value) {
    let (images, texts, _) = inspect(later, None);
    assert_eq!(images, 0, "image should not be re-sent: {texts:?}");
    let expected = format!("{READ_TEXT}\n\n[{NOT_ATTACHED}");
    assert!(
        texts.iter().any(|t| t.contains(&expected)),
        "missing not-attached notice: {texts:?}"
    );
}

#[test]
fn protocols_attach_tool_result_images_on_first_round_only_and_gate_non_vision_models() {
    let read = read_image();
    let first = continuing(vec![vec![read.clone()]]);
    let second = continuing(vec![
        vec![read],
        vec![
            text("A chess board."),
            tool("bash", json!({"output": "ok"})),
        ],
    ]);

    // Anthropic: inside tool_result content array.
    let anth = AnthropicMessagesProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    let content = &anth["messages"][2]["content"][0]["content"];
    assert_eq!(content.as_array().unwrap().len(), 2);
    assert_eq!(content[0], json!({"type": "text", "text": READ_TEXT}));
    assert_eq!(
        content[1],
        json!({"type": "image", "source": {
            "type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo=",
        }})
    );
    assert_not_sent_again(
        &AnthropicMessagesProtocol
            .encode_for_model(&second, None, true)
            .unwrap(),
    );

    // OpenAI Responses: inside function_call_output.
    let resp = OpenAiResponsesProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    assert_eq!(resp["input"][2]["type"], "function_call_output");
    assert_eq!(
        resp["input"][2]["output"],
        json!([
            {"type": "input_text", "text": READ_TEXT},
            {"type": "input_image", "image_url": PNG},
        ])
    );
    assert_not_sent_again(
        &OpenAiResponsesProtocol
            .encode_for_model(&second, None, true)
            .unwrap(),
    );

    // OpenAI Chat: follow-up user message after tool messages.
    let chat = OpenAiChatProtocol
        .encode_for_model(&first, None, true)
        .unwrap();
    let msgs = chat["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[2]["role"], "tool");
    assert_eq!(msgs[2]["content"], READ_TEXT);
    let call = msgs[1]["tool_calls"][0]["id"].as_str().unwrap();
    assert_eq!(
        msgs[3],
        json!({"role": "user", "content": [
            {"type": "text", "text": format!("Image from the result of tool call {call}:")},
            {"type": "image_url", "image_url": {"url": PNG}},
        ]})
    );
    assert_not_sent_again(
        &OpenAiChatProtocol
            .encode_for_model(&second, None, true)
            .unwrap(),
    );

    // Without image input (`false` or `encode()`): text only with NOT_SHOWN notice.
    let anth_no = AnthropicMessagesProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    let resp_no = OpenAiResponsesProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    let chat_no = OpenAiChatProtocol
        .encode_for_model(&first, None, false)
        .unwrap();
    assert_eq!(chat_no["messages"].as_array().unwrap().len(), 3);
    for t in [
        &anth_no["messages"][2]["content"][0]["content"],
        &resp_no["input"][2]["output"],
        &chat_no["messages"][2]["content"],
    ] {
        let s = t.as_str().unwrap();
        assert!(s.starts_with(READ_TEXT) && s.contains(NOT_SHOWN), "{s}");
    }
    assert_eq!(AnthropicMessagesProtocol.encode(&first).unwrap(), anth_no);
    assert_eq!(OpenAiResponsesProtocol.encode(&first).unwrap(), resp_no);
    assert_eq!(OpenAiChatProtocol.encode(&first).unwrap(), chat_no);
}

#[test]
fn tool_result_images_enforce_two_mib_budget_preserving_first_image() {
    let big = url(900 * 1024 + 2);
    let oversized = url(3 * 1024 * 1024 + 2);
    let exact_a = url(1_048_578);
    let exact_b = url(1_048_574);
    let nf = not_fitted();
    for protocol in PROTOCOLS {
        let (n, texts, _) = inspect(&encode_val(protocol, pages(&[&[&big, &big]])), None);
        assert_eq!(n, 2);
        assert!(
            texts
                .iter()
                .all(|t| !t.contains(NOT_ATTACHED) && !t.contains(NOT_FITTED))
        );

        let (n, _, _) = inspect(&encode_val(protocol, pages(&[&[&exact_a, &exact_b]])), None);
        assert_eq!(n, 2);
        let (n, texts, _) = inspect(&encode_val(protocol, pages(&[&[&exact_a, &exact_a]])), None);
        assert_eq!(n, 1);
        assert!(noticed_in(&texts, 2, &nf));

        let (n, texts, _) = inspect(
            &encode_val(protocol, pages(&[&[&big, &big, &big, PNG]])),
            None,
        );
        assert_eq!(n, 2);
        assert!(noticed_in(&texts, 3, &nf) && noticed_in(&texts, 4, &nf));

        let (n, texts, saw_oversized) = inspect(
            &encode_val(protocol, pages(&[&[&oversized, PNG]])),
            Some(&oversized[22..]),
        );
        assert_eq!(n, 1);
        assert!(saw_oversized && noticed_in(&texts, 2, &nf));
    }
}

#[test]
fn tool_result_images_follow_latest_tool_round_only() {
    let big = url(900 * 1024 + 2);
    for protocol in PROTOCOLS {
        // Reasoning + call + text in the same step still sends the image.
        let (n, _, _) = inspect(
            &encode_val(
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
            ),
            None,
        );
        assert_eq!(n, 1);

        // Two successive tool-only rounds: only the second round's image is sent.
        let (n, texts, saw_big) = inspect(
            &encode_val(protocol, pages(&[&[PNG], &[&big]])),
            Some(&big[22..]),
        );
        assert_eq!(n, 1);
        assert!(saw_big && noticed_in(&texts, 1, NOT_ATTACHED));

        // Round without tool calls after image rounds: no images sent.
        for after in [vec![text("Done.")], Vec::new()] {
            let mut steps = pages(&[&[PNG], &[&big]]);
            steps.push(after);
            let (n, texts, _) = inspect(&encode_val(protocol, steps), None);
            assert_eq!(n, 0);
            assert!(noticed_in(&texts, 1, NOT_ATTACHED) && noticed_in(&texts, 2, NOT_ATTACHED));
        }
    }
}
