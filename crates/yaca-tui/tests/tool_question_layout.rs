#![allow(dead_code)]

mod render_support;

use render_support::render;
use serde_json::json;
use yaca_proto::{
    Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId, ToolCallId, ToolName,
};
use yaca_tui::AppState;

#[test]
fn completed_ask_user_renders_opencode_question_snapshot() {
    // Given: an assistant turn with a completed ask_user tool.
    let mut state = AppState::default();
    with_completed_question(&mut state, "Continue with deploy?", "yes");

    // When: the transcript is rendered.
    let output = render(&mut state, 100, 28);

    // Then: yaca mirrors OpenCode's question summary and structured answer snapshot.
    assert!(
        output.contains("→ Asked 1 question"),
        "question tool status row should not expose the raw ask_user name:\n{output}"
    );
    assert!(
        output.contains("# Questions"),
        "question snapshot title missing:\n{output}"
    );
    assert!(
        output.contains("Question: Continue with deploy?"),
        "question snapshot should include the prompt:\n{output}"
    );
    assert!(
        output.contains("Answer: yes"),
        "question snapshot should include the answer:\n{output}"
    );
    assert!(
        !output.contains(r#""question":"#),
        "question snapshot should not expose raw JSON input:\n{output}"
    );
}

fn with_completed_question(state: &mut AppState, question: &str, answer: &str) {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let call = ToolCallId::new();
    let name = ToolName::new("ask_user");
    state.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
        },
    ));
    state.apply(&env(
        2,
        Event::ToolInputStart {
            session,
            message,
            part,
            call,
            name: name.clone(),
        },
    ));
    state.apply(&env(
        3,
        Event::ToolCallRequested {
            session,
            message,
            part,
            call,
            name,
            input: json!({
                "question": question,
                "kind": "text"
            }),
        },
    ));
    state.apply(&env(
        4,
        Event::ToolResult {
            session,
            message,
            part,
            call,
            output: json!({ "answer": answer }),
            time_ms: 9,
        },
    ));
}

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}
